//! Bounded runtime-neutral actor mailbox loop.

use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    time::Duration,
};

use super::CancellationToken;

/// Mailbox creation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidMailboxCapacity;

impl core::fmt::Display for InvalidMailboxCapacity {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("actor mailbox capacity must be above zero")
    }
}

impl std::error::Error for InvalidMailboxCapacity {}

/// Ownership-preserving mailbox send failure.
#[derive(Debug, Eq, PartialEq)]
pub enum MailboxSendError<M> {
    Full(M),
    Closed(M),
}

/// Cloneable bounded actor ingress.
#[derive(Debug)]
pub struct ActorIngress<M: Send>(SyncSender<M>);

impl<M: Send> Clone for ActorIngress<M> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<M: Send> ActorIngress<M> {
    pub fn try_send(&self, message: M) -> Result<(), MailboxSendError<M>> {
        self.0.try_send(message).map_err(|error| match error {
            TrySendError::Full(value) => MailboxSendError::Full(value),
            TrySendError::Disconnected(value) => MailboxSendError::Closed(value),
        })
    }
}

/// Receiver held by exactly one actor task.
pub struct ActorReceiver<M: Send>(Receiver<M>);

impl<M: Send> ActorReceiver<M> {
    /// Receives with a finite owner poll boundary.
    pub fn receive_timeout(&self, timeout: Duration) -> Result<M, MailboxReceiveError> {
        if timeout.is_zero() {
            return Err(MailboxReceiveError::InvalidTimeout);
        }
        self.0.recv_timeout(timeout).map_err(|error| match error {
            RecvTimeoutError::Timeout => MailboxReceiveError::TimedOut,
            RecvTimeoutError::Disconnected => MailboxReceiveError::Closed,
        })
    }
}

/// Finite mailbox receive outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MailboxReceiveError {
    TimedOut,
    Closed,
    InvalidTimeout,
}

/// Creates a strictly positive-capacity actor mailbox.
pub fn actor_mailbox<M: Send>(
    capacity: usize,
) -> Result<(ActorIngress<M>, ActorReceiver<M>), InvalidMailboxCapacity> {
    if capacity == 0 {
        return Err(InvalidMailboxCapacity);
    }
    let (sender, receiver) = mpsc::sync_channel(capacity);
    Ok((ActorIngress(sender), ActorReceiver(receiver)))
}

/// Handler decision after one message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorDirective {
    Continue,
    Stop,
}

/// Actor-owned message handler.
pub trait ActorHandler<M: Send> {
    type Error;
    fn handle(&mut self, message: M) -> Result<ActorDirective, Self::Error>;
}

/// Explicit actor loop completion/failure.
#[derive(Debug, Eq, PartialEq)]
pub enum ActorLoopExit<E> {
    Cancelled,
    HandlerStopped,
    MailboxClosed,
    InvalidPollInterval,
    HandlerFailed(E),
}

/// Runs one actor until cancellation, stop directive, closure, or handler error.
pub fn run_actor<M: Send, H>(
    receiver: ActorReceiver<M>,
    cancellation: &CancellationToken,
    poll_interval: Duration,
    handler: &mut H,
) -> ActorLoopExit<H::Error>
where
    H: ActorHandler<M>,
{
    if poll_interval.is_zero() {
        return ActorLoopExit::InvalidPollInterval;
    }
    loop {
        if cancellation.is_cancelled() {
            return ActorLoopExit::Cancelled;
        }
        match receiver.receive_timeout(poll_interval) {
            Ok(message) => match handler.handle(message) {
                Ok(ActorDirective::Continue) => {}
                Ok(ActorDirective::Stop) => return ActorLoopExit::HandlerStopped,
                Err(error) => return ActorLoopExit::HandlerFailed(error),
            },
            Err(MailboxReceiveError::TimedOut) => {}
            Err(MailboxReceiveError::Closed) => return ActorLoopExit::MailboxClosed,
            Err(MailboxReceiveError::InvalidTimeout) => {
                return ActorLoopExit::InvalidPollInterval;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StopHandler;
    impl ActorHandler<u8> for StopHandler {
        type Error = ();
        fn handle(&mut self, _message: u8) -> Result<ActorDirective, Self::Error> {
            Ok(ActorDirective::Stop)
        }
    }

    #[test]
    fn handler_can_stop_owned_loop() -> Result<(), InvalidMailboxCapacity> {
        let (ingress, receiver) = actor_mailbox(1)?;
        let cancellation = CancellationToken::new();
        assert_eq!(ingress.try_send(1), Ok(()));
        let exit = run_actor(
            receiver,
            &cancellation,
            Duration::from_millis(1),
            &mut StopHandler,
        );
        assert_eq!(exit, ActorLoopExit::HandlerStopped);
        Ok(())
    }
}

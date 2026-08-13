//! Concrete bounded fanout to owner actor mailboxes.

use crate::{
    routing::{CommandSink, CommandTarget, RouteDispatchError, RoutedCommand},
    runtime::{ActorIngress, MailboxSendError},
};

/// One bounded ingress per command owner category.
pub struct ActorCommandFanout {
    pub core_lifecycle: ActorIngress<RoutedCommand>,
    pub config: ActorIngress<RoutedCommand>,
    pub core: ActorIngress<RoutedCommand>,
    pub subscription: ActorIngress<RoutedCommand>,
    pub platform: ActorIngress<RoutedCommand>,
}

impl CommandSink for ActorCommandFanout {
    fn try_dispatch(&mut self, command: RoutedCommand) -> Result<(), RouteDispatchError> {
        let result = match command.target {
            CommandTarget::CoreLifecycle => self.core_lifecycle.try_send(command),
            CommandTarget::ConfigActor => self.config.try_send(command),
            CommandTarget::CoreActor => self.core.try_send(command),
            CommandTarget::SubscriptionActor => self.subscription.try_send(command),
            CommandTarget::PlatformActor => self.platform.try_send(command),
        };
        result.map_err(|error| match error {
            MailboxSendError::Full(value) => RouteDispatchError::ResourceExhausted(value),
            MailboxSendError::Closed(value) => RouteDispatchError::TargetClosed(value),
        })
    }
}

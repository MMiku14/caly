//! Linux template-worker parent runner.
//!
//! Spawns the worker executable, writes a bounded request frame to stdin, reads
//! the bounded response frame from stdout on a background thread, enforces a
//! wall-clock timeout with kill-and-reap, and reaps the child before returning.

use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use caly_platform::PlatformFailure;

use super::{
    MAX_RESPONSE_FRAME_BYTES, TemplateRequest, TemplateResponse, TemplateWorker,
    TemplateWorkerFailure, TemplateWorkerPlan, WorkerResponse, decode_response, encode_request,
};

/// Linux backend for the `TemplateWorker` contract.
#[derive(Default)]
pub struct LinuxTemplateWorker;

impl TemplateWorker for LinuxTemplateWorker {
    fn render(
        &mut self,
        plan: &TemplateWorkerPlan,
        request: TemplateRequest,
    ) -> Result<TemplateResponse, TemplateWorkerFailure> {
        let mut child = spawn(plan)?;
        if let Err(error) = write_request(&mut child, &request) {
            let _ = kill_and_reap(&mut child);
            return Err(TemplateWorkerFailure::WriteRequest(error));
        }
        let reader = child
            .stdout
            .take()
            .ok_or_else(|| TemplateWorkerFailure::Spawn(failure("stdout was not created")))?;
        let handle = thread::spawn(move || read_frame(reader));
        match wait_for_exit(&mut child, plan.wall_timeout) {
            Ok(()) => {
                let _ = child.wait();
                match handle.join() {
                    Ok(Ok(frame)) => decode_worker(frame),
                    Ok(Err(error)) => Err(TemplateWorkerFailure::ReadResponse(error)),
                    Err(_) => Err(TemplateWorkerFailure::ReadResponse(failure(
                        "worker stdout reader panicked",
                    ))),
                }
            }
            Err(reap) => {
                let _ = handle.join();
                Err(TemplateWorkerFailure::TimedOut { kill: None, reap })
            }
        }
    }
}

fn spawn(plan: &TemplateWorkerPlan) -> Result<std::process::Child, TemplateWorkerFailure> {
    Command::new(&plan.executable)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            TemplateWorkerFailure::Spawn(failure(&format!("cannot spawn template worker: {error}")))
        })
}

fn write_request(
    child: &mut std::process::Child,
    request: &TemplateRequest,
) -> Result<(), PlatformFailure> {
    let frame = encode_request(TemplateRequest {
        request_id: request.request_id,
        source: request.source.clone(),
        context: request.context.clone(),
    })
    .map_err(|_| failure("cannot encode template request frame"))?;
    let stdin = child
        .stdin
        .as_mut()
        .ok_or_else(|| failure("stdin was not created"))?;
    stdin
        .write_all(&frame)
        .and_then(|()| stdin.flush())
        .map_err(|error| failure(&format!("cannot write worker request: {error}")))?;
    // Closing stdin signals EOF to the worker's bounded `read_to_end`, which
    // unblocks its frame decode; leaving it open would deadlock the child.
    drop(child.stdin.take());
    Ok(())
}

fn read_frame(stdout: std::process::ChildStdout) -> Result<Vec<u8>, PlatformFailure> {
    let mut reader = stdout.take((MAX_RESPONSE_FRAME_BYTES as u64) + 1);
    let mut frame = Vec::new();
    reader
        .read_to_end(&mut frame)
        .map_err(|error| failure(&format!("cannot read worker response: {error}")))?;
    if frame.len() > MAX_RESPONSE_FRAME_BYTES {
        return Err(failure("worker response exceeded the bounded frame"));
    }
    Ok(frame)
}

/// Returns `Ok(())` when the child exited within the timeout, else reaps and
/// returns a reap failure.
fn wait_for_exit(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<(), Option<PlatformFailure>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(kill_and_reap(child).err());
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn kill_and_reap(child: &mut std::process::Child) -> Result<(), PlatformFailure> {
    let _ = child.kill();
    child
        .wait()
        .map(|_| ())
        .map_err(|error| failure(&format!("cannot reap worker: {error}")))
}

fn decode_worker(frame: Vec<u8>) -> Result<TemplateResponse, TemplateWorkerFailure> {
    match decode_response(&frame).map_err(|_| {
        TemplateWorkerFailure::InvalidResponse(bounded_text(
            "template worker returned an invalid response frame",
        ))
    })? {
        WorkerResponse::Rendered(response) => Ok(response),
        WorkerResponse::Rejected { diagnostic, .. } => Err(TemplateWorkerFailure::InvalidResponse(
            diagnostic.unwrap_or_else(|| bounded_text("template worker rejected the request")),
        )),
    }
}

/// Delegates to the infallible `from_nonempty_clamped`; an over-long
/// diagnostic keeps its prefix instead of aborting the daemon.
fn bounded_text(value: &str) -> caly_domain::BoundedText<1_024> {
    caly_domain::BoundedText::from_nonempty_clamped(value.to_owned(), "template worker failed")
}

fn failure(message: &str) -> PlatformFailure {
    PlatformFailure {
        operation: bounded::<64>("linux-template-worker"),
        resource: bounded::<256>("template-worker"),
        message: bounded::<1_024>(message),
        suggested_action: bounded::<512>("inspect the worker executable and template request"),
    }
}

fn bounded<const MAX: usize>(value: &str) -> caly_domain::BoundedText<MAX> {
    caly_domain::BoundedText::from_nonempty_clamped(value.to_owned(), "template-worker")
}

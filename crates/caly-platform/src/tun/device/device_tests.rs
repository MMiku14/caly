//! Tests for `tun/device.rs`, extracted to the sibling
//! convention file (audit #70 file-length budget).

//! Round 31: the engage / restore tests stay
//! co-located with the orchestration so a
//! future change to the "adopt / drop /
//! create" state machine lands next to its
//! tests. The escalation policy tests live
//! in `escalate.rs`; the shell-quoting tests
//! live in `script.rs`; the failure-shape
//! tests live in `capability.rs`.

use super::{engage_with, interface_exists, is_busy_failure, restore_tun};
use crate::PlatformFailure;
use crate::command::{CommandOutput, CommandResult, CommandRunner};
use crate::tun::TunEscalation;
use crate::tun::TunRequest;
use caly_domain::BoundedText;
use std::cell::RefCell;
use std::rc::Rc;

/// Records every command without executing
/// anything, so stale-interface handling can
/// be asserted safely (never touches a real
/// device).
#[derive(Clone, Default)]
struct RecordingRunner {
    calls: Rc<RefCell<Vec<String>>>,
}

impl CommandRunner for RecordingRunner {
    fn run_bounded(
        &mut self,
        request: crate::command::CommandRequest,
    ) -> Result<CommandResult, PlatformFailure> {
        let mut line = request.executable.to_string_lossy().into_owned();
        for argument in &request.arguments {
            line.push(' ');
            line.push_str(argument.as_str());
        }
        self.calls.borrow_mut().push(line);
        Ok(CommandResult {
            exit_code: Some(0),
            stdout: CommandOutput::new(),
            stderr: CommandOutput::new(),
        })
    }
}

fn tun_request(name: &str) -> TunRequest {
    TunRequest {
        interface: BoundedText::new(name.to_owned()).unwrap(),
        mtu: 1400,
    }
}

#[test]
fn interface_exists_detects_real_interfaces_only() {
    // `lo` always exists on Linux; a unique name never does.
    assert!(interface_exists("lo"));
    // Pid-bounded: `caly-test-{pid}` overflows the 15-byte IFNAMSIZ
    // limit once pids reach 6 digits (regression observed on hosts
    // with a high pid counter); the low 6 digits keep the name
    // unique per process and well under the cap.
    let unique = format!("caly-t{}", std::process::id() % 1_000_000);
    assert!(!interface_exists(&unique));
}

#[test]
fn interface_name_validation_enforces_kernel_limits() {
    assert!(super::is_valid_interface_name("caly0"));
    assert!(super::is_valid_interface_name("lo"));
    assert!(super::is_valid_interface_name("tun-foo_1.2"));
    assert!(!super::is_valid_interface_name("")); // empty
    assert!(!super::is_valid_interface_name("0123456789abcdef")); // 16 bytes > IFNAMSIZ-1
    assert!(!super::is_valid_interface_name("../x")); // traversal into sysfs join
    assert!(!super::is_valid_interface_name(".."));
    assert!(!super::is_valid_interface_name("a/b"));
    assert!(!super::is_valid_interface_name("a b"));
    // Traversal attempts must never reach the sysfs join.
    assert!(!interface_exists("../sys"));
}

#[test]
fn engage_drops_stale_interface_before_creating_fresh() -> Result<(), String> {
    let mut runner = RecordingRunner::default();
    // `lo` exists, so engage must delete it first (recorded only; the mock
    // never executes anything) and then recreate + configure it.
    let _owned = engage_with(&mut runner, TunEscalation::None, tun_request("lo"))
        .map_err(|e| e.to_string())?;
    let calls = runner.calls.borrow();
    assert!(
        calls
            .first()
            .is_some_and(|line| line.contains("tuntap del dev lo")),
        "stale interface must be deleted first, got {calls:?}"
    );
    assert!(
        calls.iter().any(|line| line.contains("tuntap add dev lo")),
        "a fresh interface must then be created, got {calls:?}"
    );
    assert!(calls.len() >= 3, "add + mtu + up expected: {calls:?}");
    Ok(())
}

#[test]
fn engage_without_stale_interface_goes_straight_to_add() -> Result<(), String> {
    let mut runner = RecordingRunner::default();
    // Pid-bounded: `caly-test-{pid}` overflows the 15-byte IFNAMSIZ
    // limit once pids reach 6 digits (regression observed on hosts
    // with a high pid counter); the low 6 digits keep the name
    // unique per process and well under the cap.
    let unique = format!("caly-t{}", std::process::id() % 1_000_000);
    engage_with(&mut runner, TunEscalation::None, tun_request(&unique))
        .map_err(|e| e.to_string())?;
    let calls = runner.calls.borrow();
    assert!(
        calls
            .first()
            .is_some_and(|line| line.contains("tuntap add dev")),
        "no stale interface: the first command must be add, got {calls:?}"
    );
    Ok(())
}

/// A runner that fails every direct attempt
/// (as a daemon without `CAP_NET_ADMIN`
/// would) and records escalated calls.
#[derive(Clone, Default)]
struct EscalatingRunner {
    calls: Rc<RefCell<Vec<String>>>,
}

impl CommandRunner for EscalatingRunner {
    fn run_bounded(
        &mut self,
        request: crate::command::CommandRequest,
    ) -> Result<CommandResult, PlatformFailure> {
        let mut line = request.executable.to_string_lossy().into_owned();
        for argument in &request.arguments {
            line.push(' ');
            line.push_str(argument.as_str());
        }
        self.calls.borrow_mut().push(line);
        let escalated = request.executable.to_string_lossy() == "pkexec";
        Ok(CommandResult {
            exit_code: Some(i32::from(!escalated)),
            stdout: CommandOutput::new(),
            stderr: CommandOutput::new(),
        })
    }
}

#[test]
fn engage_without_privileges_escalates_one_batch_not_per_command() -> Result<(), String> {
    let mut runner = EscalatingRunner::default();
    // Pid-bounded: `caly-test-{pid}` overflows the 15-byte IFNAMSIZ
    // limit once pids reach 6 digits (regression observed on hosts
    // with a high pid counter); the low 6 digits keep the name
    // unique per process and well under the cap.
    let unique = format!("caly-t{}", std::process::id() % 1_000_000);
    engage_with(&mut runner, TunEscalation::Pkexec, tun_request(&unique))
        .map_err(|e| e.to_string())?;
    let calls = runner.calls.borrow();
    // Every direct `ip` attempt happened (and failed), then exactly ONE
    // escalated `sh -c` batch carries the whole sequence.
    let direct: Vec<&String> = calls
        .iter()
        .filter(|line| line.starts_with("ip "))
        .collect();
    let escalated: Vec<&String> = calls
        .iter()
        .filter(|line| line.starts_with("pkexec "))
        .collect();
    assert_eq!(
        escalated.len(),
        1,
        "one password prompt expected: {calls:?}"
    );
    assert!(
        escalated[0].contains("sh -c ip tuntap add dev ")
            && escalated[0].contains(" && ip link set dev "),
        "escalated batch must chain the remaining commands: {escalated:?}"
    );
    // The direct phase stops at the first failure (add), so exactly one
    // direct attempt precedes the single escalated batch.
    assert_eq!(
        direct.len(),
        1,
        "first direct attempt then one batch: {calls:?}"
    );
    Ok(())
}

#[test]
fn engage_with_direct_privileges_never_escalates() -> Result<(), String> {
    let mut runner = RecordingRunner::default();
    // Pid-bounded: `caly-test-{pid}` overflows the 15-byte IFNAMSIZ
    // limit once pids reach 6 digits (regression observed on hosts
    // with a high pid counter); the low 6 digits keep the name
    // unique per process and well under the cap.
    let unique = format!("caly-t{}", std::process::id() % 1_000_000);
    engage_with(&mut runner, TunEscalation::Pkexec, tun_request(&unique))
        .map_err(|e| e.to_string())?;
    let calls = runner.calls.borrow();
    assert!(
        calls.iter().all(|line| line.starts_with("ip ")),
        "no escalation when direct commands succeed: {calls:?}"
    );
    assert_eq!(calls.len(), 3);
    Ok(())
}

/// Every escalation attempt must FAIL so
/// the engage itself reports an error and
/// rolls back. The pre-Round 31 inline
/// shape propagated the caller's
/// `escalation` argument into the cleanup
/// `run_ip` call, so a failed engage would
/// have surfaced a SECOND password prompt
/// for the best-effort rollback. The
/// Round 31 cleanup is a direct `ip tuntap
/// del` only, so the test pins the
/// `pkexec` call count to exactly 1 (the
/// failed engage batch) regardless of
/// policy.
#[derive(Clone, Default)]
struct AlwaysFailRunner {
    calls: Rc<RefCell<Vec<String>>>,
}

impl CommandRunner for AlwaysFailRunner {
    fn run_bounded(
        &mut self,
        request: crate::command::CommandRequest,
    ) -> Result<CommandResult, PlatformFailure> {
        let mut line = request.executable.to_string_lossy().into_owned();
        for argument in &request.arguments {
            line.push(' ');
            line.push_str(argument.as_str());
        }
        self.calls.borrow_mut().push(line);
        Ok(CommandResult {
            exit_code: Some(1),
            stdout: CommandOutput::new(),
            stderr: CommandOutput::new(),
        })
    }
}

/// A runner whose `ip tuntap del` fails with
/// EBUSY (stderr carries the kernel's "Device
/// or resource busy") while every other
/// command succeeds — the exact shape of a
/// device held by the running core's tun
/// inbound.
#[derive(Clone, Default)]
struct BusyDeleteRunner {
    calls: Rc<RefCell<Vec<String>>>,
}

impl CommandRunner for BusyDeleteRunner {
    fn run_bounded(
        &mut self,
        request: crate::command::CommandRequest,
    ) -> Result<CommandResult, PlatformFailure> {
        let mut line = request.executable.to_string_lossy().into_owned();
        for argument in &request.arguments {
            line.push(' ');
            line.push_str(argument.as_str());
        }
        let is_delete = line.contains("tuntap del dev");
        self.calls.borrow_mut().push(line);
        let busy = CommandOutput::try_from_vec(
            b"Cannot delete TUN device: Device or resource busy\n".to_vec(),
        )
        .unwrap_or_else(|_| CommandOutput::new());
        Ok(CommandResult {
            exit_code: Some(i32::from(is_delete)),
            stdout: CommandOutput::new(),
            stderr: if is_delete {
                busy
            } else {
                CommandOutput::new()
            },
        })
    }
}

/// The core's tun inbound created the
/// device before the platform engage ran
/// (config apply precedes SetTun). The
/// engage must ADOPT the held device — no
/// delete, no recreate, no escalation —
/// and confirm MTU/UP idempotently instead
/// of failing on the busy delete.
#[test]
fn engage_adopts_interface_held_by_core() -> Result<(), String> {
    let mut runner = BusyDeleteRunner::default();
    // `lo` always exists, so the engage probes the delete and sees EBUSY.
    let _owned = engage_with(&mut runner, TunEscalation::Pkexec, tun_request("lo"))
        .map_err(|e| e.to_string())?;
    let calls = runner.calls.borrow();
    assert!(
        !calls.iter().any(|line| line.contains("tuntap add dev lo")),
        "a held device must not be recreated: {calls:?}"
    );
    assert!(
        !calls.iter().any(|line| line.starts_with("pkexec ")),
        "EBUSY must short-circuit before any escalation prompt: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|line| line.contains("link set dev lo mtu"))
            && calls.iter().any(|line| line.contains("link set dev lo up")),
        "adoption confirms MTU/UP idempotently: {calls:?}"
    );
    Ok(())
}

/// A stale interface (no holder) must still
/// be dropped and recreated: the delete
/// succeeds and the fresh-create path runs
/// as before.
#[test]
fn restore_tolerates_busy_interface() -> Result<(), String> {
    let mut runner = BusyDeleteRunner::default();
    // `lo` is "held" from the runner's point of view; the core will
    // release it on restart, so a busy restore is a successful no-op.
    restore_tun(&mut runner, TunEscalation::None, "lo").map_err(|e| e.to_string())?;
    let calls = runner.calls.borrow();
    assert_eq!(calls.len(), 1, "one delete attempt: {calls:?}");
    assert!(calls[0].contains("tuntap del dev lo"));
    Ok(())
}

#[test]
fn restore_propagates_non_busy_failures() {
    let mut runner = AlwaysFailRunner::default();
    let outcome = restore_tun(&mut runner, TunEscalation::None, "lo");
    assert!(
        outcome.is_err(),
        "a permission failure must not be swallowed"
    );
    assert!(!matches!(outcome, Err(ref error) if is_busy_failure(error)));
}

/// Regression: a failed engage under any
/// escalation policy must clean up with a
/// direct `ip tuntap del` only — never a
/// `pkexec` or `sudo -n` batch. The inline
/// comment of the old implementation said
/// "never another password prompt" but the
/// call site passed the original
/// `escalation` through, contradicting the
/// comment.
#[test]
fn failed_engage_cleanup_never_escalates() -> Result<(), String> {
    let mut runner = AlwaysFailRunner::default();
    // Pid-bounded: `caly-test-{pid}` overflows the 15-byte IFNAMSIZ
    // limit once pids reach 6 digits (regression observed on hosts
    // with a high pid counter); the low 6 digits keep the name
    // unique per process and well under the cap.
    let unique = format!("caly-t{}", std::process::id() % 1_000_000);
    // `Pkexec` is the most likely policy to
    // misbehave: every direct attempt
    // fails (no CAP_NET_ADMIN), then the
    // escalated batch also fails (mock
    // returns exit 1), so the engage
    // returns Err and the cleanup path
    // runs. The cleanup must not surface
    // another `pkexec`.
    let result = engage_with(&mut runner, TunEscalation::Pkexec, tun_request(&unique));
    if result.is_ok() {
        return Err("engage must fail when every attempt fails".to_owned());
    }
    let calls = runner.calls.borrow();
    let escalated: Vec<&String> = calls
        .iter()
        .filter(|line| line.starts_with("pkexec ") || line.starts_with("sudo "))
        .collect();
    if escalated.len() != 1 {
        return Err(format!(
            "exactly one escalated batch is allowed (the failed engage); \
             the cleanup must not escalate again: {calls:?}"
        ));
    }
    // The cleanup command is the LAST one
    // in the call log and must start with
    // the bare `ip` executable.
    let cleanup = calls
        .iter()
        .rev()
        .find(|line| line.contains("tuntap del dev"));
    match cleanup {
        Some(line) if line.starts_with("ip ") => Ok(()),
        Some(line) => Err(format!(
            "cleanup must run direct `ip tuntap del`, got {line:?}"
        )),
        None => Err(format!("cleanup `tuntap del` must be recorded: {calls:?}")),
    }
}

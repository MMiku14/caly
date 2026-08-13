//! `Refreshable` — the single shape for "declare a remote
//! source + refresh it".
//!
//! Three resources share the same mental model: a *declared*
//! name, an *underlying URL or path*, a *cached body* under
//! `<state>/...`, and a *refresh* action that re-fetches the
//! body through the SSRF-safe pipeline.
//!
//! - `set sub refresh` — refreshes the body of one or every
//!   subscription source URL.
//! - `set profile refresh` — refreshes the body of one or
//!   every `Remote` profile (depth-first for `Merge`).
//! - `set rule-provider refresh` — re-validates the declared
//!   `rule_providers:` list and reports how many entries
//!   would be re-materialized on the next `config apply`.
//!
//! # Round 28: trait surface simplified
//!
//! The pre-Round 28 `Refreshable::refresh` trait method
//! took a `&RefreshContext` whose four fields were
//! `output` / `apply` / `dry_run` / `command`, and all
//! three impls discarded the entire `ctx` argument with
//! `let _ = ctx;`. The Round 28 shape folds the
//! actually-needed bits (`output` for the success
//! envelope, `command` for the error envelope) into the
//! trait's parameter list directly, so the
//! `RefreshContext` struct / `always_writes` constructor /
//! `format_outcome` trait method / `report_refresh_error`
//! dispatch helper are all gone. The trait's contract
//! stays identical: an `Ok(_)` returns the `target` +
//! `count` summary, an `Err(_)` returns a typed
//! `RefreshError` whose `code()` / `message()` / `hint()`
//! project into the dispatch's `CliError` envelope.

use std::process::ExitCode;

use crate::output::CliOutput;

/// Uniform shape of "what to refresh" across the remote
/// resources. Each `Refreshable` impl projects its domain
/// identifier into this enum so the dispatch in
/// `commands::set` can route the right one.
///
/// Round 28: the pre-Round 28 `SubscriptionUrl` variant
/// was reserved for a future `set sub refresh` route
/// through the `Refreshable` trait. The current
/// `set sub refresh` dispatch (`commands::set::sub`)
/// drives the daemon RPC directly through
/// `crate::client::run_client(RefreshSubscription, options)`,
/// which is the only path that preserves the operator's
/// `--json` / `--core` flags (the trait's `refresh`
/// currently uses `CliOptions::default()`). Until the
/// trait grows a way to thread the operator's options
/// through, the `SubscriptionUrl` variant stays
/// un-built. The variant is dropped here so the enum
/// surface is exactly what the live dispatchers use;
/// adding it back when the `set sub refresh` migration
/// lands is a 1-line change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefreshTarget {
    /// A single declared profile id.
    ProfileId(String),
    /// A single declared rule-provider name.
    RuleProviderName(String),
}

impl RefreshTarget {
    pub fn as_str(&self) -> &str {
        match self {
            Self::ProfileId(s) | Self::RuleProviderName(s) => s,
        }
    }
}

/// What a refresh can report. Round 28: a single
/// `fetched: usize` counter. The pre-Round 28
/// `summary: Option<String>` field was a write-only
/// option (set by `with_summary`, read by the deleted
/// `Refreshable::format_outcome` default impl), so it
/// carried no information past the call site that set
/// it — the only `report_refreshed` consumer
/// synthesises its own summary from `target` +
/// `fetched`. The field is gone; the implementations
/// that wanted a richer summary thread it through the
/// human-output channel directly (the dispatch
/// `set sub refresh` path, once it migrates to the
/// trait, will pass its own `output.success(…)`
/// summary).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RefreshOutcome {
    /// Number of bodies that were actually re-fetched (a
    /// `NotModified` HTTP 304 does **not** count).
    pub fetched: usize,
}

impl RefreshOutcome {
    pub fn fetched(count: usize) -> Self {
        Self { fetched: count }
    }
}

/// Unified refresh error type. Each `Refreshable` impl maps
/// its domain-specific failure (e.g. `ProfileCmdError`,
/// `SubscriptionRefreshFailure`) into one of these
/// variants; the dispatch then maps to a stable
/// `code: &'static str` for the JSON envelope.
#[derive(Debug)]
pub enum RefreshError {
    /// The operator-supplied identifier is not declared in
    /// the resource's config.
    NotDeclared { family: &'static str, id: String },
    /// The body could not be fetched (network or SSRF).
    Fetch {
        family: &'static str,
        id: String,
        reason: String,
    },
    /// `Merge.parts` referenced a `Remote` whose fetch failed;
    /// the outer error wraps the inner failure so the
    /// operator sees both the offender and the cause.
    FetchChain {
        family: &'static str,
        id: String,
        reason: String,
    },
    /// The on-disk cache write failed.
    Store {
        family: &'static str,
        id: String,
        reason: String,
    },
    /// Catch-all for refreshes that don't have a more
    /// specific shape yet.
    Other {
        family: &'static str,
        id: String,
        reason: String,
    },
}

impl RefreshError {
    /// Stable error code (e.g. `"profile.not_declared"`).
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotDeclared { family, .. } => match *family {
                "sub" => "sub.not_declared",
                "profile" => "profile.not_declared",
                "rule_provider" => "rule_provider.not_declared",
                _ => "refresh.not_declared",
            },
            Self::Fetch { family, .. } => match *family {
                "sub" => "sub.fetch_failed",
                "profile" => "profile.fetch",
                "rule_provider" => "rule_provider.fetch_failed",
                _ => "refresh.fetch",
            },
            Self::FetchChain { family, .. } => match *family {
                "profile" => "profile.fetch_chain",
                _ => "refresh.fetch_chain",
            },
            Self::Store { family, .. } => match *family {
                "profile" => "profile.store",
                _ => "refresh.store",
            },
            Self::Other { family, .. } => match *family {
                "profile" => "profile.refresh_failed",
                "sub" => "sub.refresh_failed",
                "rule_provider" => "rule_provider.refresh_failed",
                _ => "refresh.failed",
            },
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::NotDeclared { family, id } => format!("{family} `{id}` is not declared"),
            Self::Fetch { family, id, reason } => format!("{family} `{id}` fetch: {reason}"),
            Self::FetchChain { family, id, reason } => {
                format!("{family} `{id}` chain fetch: {reason}")
            }
            Self::Store { family, id, reason } => format!("{family} `{id}` store: {reason}"),
            Self::Other { family, id, reason } => format!("{family} `{id}` refresh: {reason}"),
        }
    }

    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::NotDeclared { family, .. } => match *family {
                "profile" => Some("declare it first with `caly profile add <id> ...`"),
                "sub" => {
                    Some("configure the URL in <config>/config.yaml and run `caly sub refresh`")
                }
                "rule_provider" => Some(
                    "declare it with `caly set rule-provider add-http|add-file|add-inline <name> ...`",
                ),
                _ => None,
            },
            Self::Fetch { .. } | Self::FetchChain { .. } => {
                Some("check the URL, your network, and the SSRF guard; try `--dry-run` first")
            }
            Self::Store { family, .. } => match *family {
                "profile" => Some("check the state directory is writable"),
                _ => None,
            },
            Self::Other { .. } => None,
        }
    }
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for RefreshError {}

/// A "remote source" resource that can be refreshed.
pub trait Refreshable {
    /// Refreshes the resource identified by `target`.
    /// Returns the [`RefreshOutcome`] (a single
    /// `fetched: usize` counter) on success or a typed
    /// [`RefreshError`] on failure. Round 29: dropped
    /// the `output: CliOutput` parameter that Round 28
    /// kept "reserved for a future `SubscriptionRefresh`
    /// impl" — both live impls (`ProfileRefresh` /
    /// `RuleProviderRefresh`) ignored it with
    /// `let _ = output;` and the dispatch callers
    /// (`set profile refresh` / `set rule-provider
    /// refresh`) construct the success / error
    /// envelopes themselves through
    /// [`run_refresh`]. The `output` is `Copy` so
    /// removing the parameter saves a `Copy` per
    /// refresh call (a real, if microscopic,
    /// optimisation).
    fn refresh(target: &RefreshTarget) -> Result<RefreshOutcome, RefreshError>;
}

// ── Three concrete impls ────────────────────────────────────
//
// Round 28: `SubscriptionRefresh` (the third impl) was
// removed. The pre-Round 28 shape had `set sub refresh`
// ready to migrate to the `Refreshable` trait, but the
// trait's `refresh` method only forwards
// `CliOptions::default()` to the daemon RPC, losing the
// operator's `--json` / `--core` flags. The current
// `set sub refresh` dispatch drives the daemon RPC
// directly through `crate::client::run_client(…,
// options)`, which is the only path that preserves those
// flags. Until the trait grows a way to thread the
// operator's options, the `SubscriptionUrl` variant +
// `SubscriptionRefresh` impl stay un-built. The trait
// surface stays valid (the `Refreshable` trait still
// unifies the live `ProfileRefresh` / `RuleProviderRefresh`
// impls), and adding the sub refresh back is a 1-line
// enum variant + a small impl when the options threading
// lands.

/// `set profile refresh` dispatch. Round 13: delegates to
/// `client::profile::refresh_all`, mapping `ProfileCmdError`
/// into the shared `RefreshError` shape.
pub struct ProfileRefresh;

impl Refreshable for ProfileRefresh {
    fn refresh(target: &RefreshTarget) -> Result<RefreshOutcome, RefreshError> {
        let id = match target {
            RefreshTarget::ProfileId(id) => Some(id.as_str()),
            RefreshTarget::RuleProviderName(_) => None,
        };
        let paths = crate::client::profile::resolve_paths();
        crate::client::profile::refresh_all(&paths, id).map_or_else(
            |err| {
                use crate::client::profile::ProfileCmdError as E;
                let id_owned = || id.unwrap_or("(all)").to_owned();
                let mapped = match &err {
                    E::NotDeclared(name) => RefreshError::NotDeclared {
                        family: "profile",
                        id: name.clone(),
                    },
                    E::Fetch(reason) => RefreshError::Fetch {
                        family: "profile",
                        id: id_owned(),
                        reason: reason.to_string(),
                    },
                    E::FetchChain { id, source } => RefreshError::FetchChain {
                        family: "profile",
                        id: id.clone(),
                        reason: source.to_string(),
                    },
                    E::Store(reason) => RefreshError::Store {
                        family: "profile",
                        id: id_owned(),
                        reason: reason.to_string(),
                    },
                    _ => RefreshError::Other {
                        family: "profile",
                        id: id_owned(),
                        reason: err.to_string(),
                    },
                };
                Err(mapped)
            },
            |count| Ok(RefreshOutcome::fetched(count)),
        )
    }
}

/// `set rule-provider refresh` dispatch.
///
/// Round 22: the daemon materialises rule-provider bodies
/// at `config apply` time from the layered
/// `rule_providers:` list (no separate body cache to
/// refresh). The CLI cannot fetch a body the daemon
/// doesn't keep, so the honest "refresh" is to
/// re-validate the local declared list and report how many
/// entries would be re-materialized on the next apply.
///
/// For a specific `name`, the refresh is `NotDeclared` if
/// the name isn't in the layered list — the same contract
/// the other two `Refreshable` impls expose. For `(all)`,
/// the count is the total declared list.
pub struct RuleProviderRefresh;

impl Refreshable for RuleProviderRefresh {
    fn refresh(target: &RefreshTarget) -> Result<RefreshOutcome, RefreshError> {
        let paths = caly_platform::paths::AppPaths::from_env();
        refresh_with_paths(&paths, target)
    }
}

/// Path-injected core of `RuleProviderRefresh::refresh`.
/// Split out so the tests can drive the validation with
/// a hermetic `AppPaths` (no process-wide env mutation).
fn refresh_with_paths(
    paths: &caly_platform::paths::AppPaths,
    target: &RefreshTarget,
) -> Result<RefreshOutcome, RefreshError> {
    let name = target.as_str();
    let declared = crate::commands::rule_provider::read_declared(paths);
    let count = declared.len();
    // Per-name refresh: validate the name is declared.
    // `(all)` is the advisory target the dispatch uses
    // when the user didn't type a name; the count is
    // the same.
    if name != "(all)" {
        let exists = declared.iter().any(|(n, _, _, _)| n == name);
        if !exists {
            return Err(RefreshError::NotDeclared {
                family: "rule_provider",
                id: name.to_owned(),
            });
        }
    }
    Ok(RefreshOutcome::fetched(count))
}

/// Round 33: the `Ok` / `Err` envelope split lives
/// here so the 2 live dispatchers (`set profile
/// refresh` / `set rule-provider refresh`) stop
/// duplicating the same 5-line match. The
/// pre-Round-33 shape in each dispatch was:
///
/// ```ignore
/// match <R as Refreshable>::refresh(&target) {
///     Ok(outcome) => {
///         report_refreshed(output, &target, &outcome);
///         ExitCode::SUCCESS
///     }
///     Err(e) => report_refresh_error(output, &e, "set X refresh"),
/// }
/// ```
///
/// — 8 lines × 2 dispatchers = 16 lines of
/// duplicated envelope projection. The helper
/// takes the `R: Refreshable` impl + the
/// `target` + the leaf string + the `output`
/// and returns the typed `ExitCode`. The
/// `Ok`-arm and `Err`-arm envelopes are inlined
/// here (not factored into separate helpers)
/// so the dispatch helper is a single
/// self-contained 12-line function — the
/// `Ok`-arm is 6 lines (target → summary +
/// payload + `output.success`), the `Err`-arm
/// is 6 lines (`CliError::new` + optional
/// `with_hint` + `report_error_returning`).
/// A future `set sub refresh` migration just
/// needs `run_refresh::<SubRefresh>(output,
/// target, "set sub refresh")`; no separate
/// `report_*` calls required.
pub fn run_refresh<R: Refreshable>(
    output: CliOutput,
    target: RefreshTarget,
    leaf: &str,
) -> ExitCode {
    match R::refresh(&target) {
        Ok(outcome) => {
            let summary = match &target {
                RefreshTarget::ProfileId(id) => {
                    format!("refreshed profile `{id}` ({} source(s))", outcome.fetched)
                }
                RefreshTarget::RuleProviderName(name) => {
                    format!("refreshed rule-provider `{name}`")
                }
            };
            let payload = serde_json::json!({
                "refreshed": outcome.fetched,
                "target": target.as_str(),
            });
            output.success(&summary, payload);
            ExitCode::SUCCESS
        }
        Err(error) => {
            let mut cli = crate::output::CliError::new(error.code(), error.message(), leaf);
            if let Some(hint) = error.hint() {
                cli = cli.with_hint(hint);
            }
            crate::output::report_error_returning(output, cli)
        }
    }
}

#[cfg(test)]
mod tests;

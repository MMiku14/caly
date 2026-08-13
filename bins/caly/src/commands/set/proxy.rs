//! `set proxy …` dispatch.
//!
//! Round 15: `On` / `Off` route through the typed
//! `client::run_client` path. Round 16: the four CRUD
//! leaves (`add` / `edit` / `remove` / `import`) write
//! `<state>/inline-proxies/<id>.json` via
//! `client::inline_proxy`.
//!
//! Round 23: every CRUD leaf routes through the shared
//! `commands::set::common::run_writer` envelope (the
//! fifth resource on the envelope, joining `set sub` /
//! `set profile` / `set rule-provider` /
//! `set proxy-group`). The four per-leaf bodies that
//! each hand-rolled their own `Ok(_)` / `Err(_)`
//! envelope builders collapsed into one-line shims;
//! the writer's `InlineProxyOutcome { Applied, DryRun }`
//! carries the on-disk `id` and the `ImportOutcome
//! { Applied, DryRun, NoOp }` carries the count, both
//! projected into the JSON envelope via the
//! `extra_payload` closure.

use std::process::ExitCode;

use crate::cli::SetProxyCmd;
use crate::client::inline_proxy::{ImportOutcome, InlineProxyError, InlineProxyOutcome};
use crate::client::legacy::SysCmd;
use crate::commands::set::common::{
    OutcomeKind, ResourceVerb, Summaries, WriteEnvelope, run_writer,
};
use crate::output::CliOutput;

/// The singular resource name used by the
/// [`Summaries::standard`] constructor for the
/// `set proxy remove` CRUD leaf (`add` / `edit`
/// are bespoke because their `InlineProxyOutcome`
/// carries an on-disk `id` that the envelope
/// projects via `inline_proxy_payload`; `import`
/// is the 1-of-1 oddity whose `applied` /
/// `dry_run` strings differ from the "X added"
/// pattern).
const NOUN: &str = "inline proxy";

pub fn dispatch(c: SetProxyCmd, options: crate::cli::CliOptions, output: CliOutput) -> ExitCode {
    use crate::client::inline_proxy as cmd;
    let paths = cmd::resolve_paths_pub();
    match c {
        SetProxyCmd::On => {
            crate::client::run_client(crate::ClientCommand::Sys(SysCmd::Proxy(true)), options)
        }
        SetProxyCmd::Off => {
            crate::client::run_client(crate::ClientCommand::Sys(SysCmd::Proxy(false)), options)
        }
        // W-PAC (2026-08-12): `sysproxy pac [url]` — without a URL the
        // CLI writes a generated PAC (local subnets direct, everything
        // else via the local proxy) into the state directory and points
        // the desktop at it; with a URL that URL is used verbatim. The
        // daemon only performs the desktop switch (auto mode +
        // autoconfig-url); the PAC authoring is fully offline.
        SetProxyCmd::Pac { url } => dispatch_pac(url, options),
        SetProxyCmd::Add {
            uri,
            group,
            apply,
            dry_run,
        } => {
            let effective_apply = apply && !dry_run;
            let leaf = format!("set proxy add {uri}");
            // The `name` slot in the envelope is the
            // user-typed URI (the operator-facing
            // identifier for the success line + the
            // JSON envelope's `name` field); the
            // on-disk `id` (a hash of the URI) is the
            // writer-side identity projected via
            // `inline_proxy_payload`.
            run_writer(
                output,
                Summaries::standard(NOUN, ResourceVerb::Add),
                &WriteEnvelope {
                    name: &uri,
                    leaf: &leaf,
                },
                || cmd::add_proxy(&paths, &uri, group.as_deref(), effective_apply),
                classify_inline,
                code_for,
                inline_proxy_payload,
            )
        }
        SetProxyCmd::Edit { id, apply, dry_run } => {
            let effective_apply = apply && !dry_run;
            let leaf = format!("set proxy edit {id}");
            run_writer(
                output,
                // `edit` isn't a `ResourceVerb` (it's
                // a profile-only verb); the bespoke
                // summary triple uses
                // [`Summaries::custom`] (Round 26) so
                // the 3-line literal collapses into a
                // one-line call site. The no-change
                // line uses the standard "inline
                // proxy already edited" shape (a
                // future writer that surfaces
                // `NoChange` for `edit` will reuse
                // this).
                Summaries::custom(
                    "inline proxy edited",
                    "inline proxy would be edited (dry-run)",
                    "inline proxy already edited",
                ),
                &WriteEnvelope {
                    name: &id,
                    leaf: &leaf,
                },
                || cmd::edit_proxy(&paths, &id, effective_apply),
                classify_inline,
                code_for,
                inline_proxy_payload,
            )
        }
        SetProxyCmd::Remove { id, apply, dry_run } => {
            // Round 29: `remove` fits the standard
            // `X <past-tense>` template but the
            // writer returns the bespoke
            // `InlineProxyOutcome` enum (the
            // `Applied` / `DryRun` variants carry
            // the on-disk `id` so the envelope is
            // uniform with `add` / `edit` /
            // `import`). The dispatch therefore
            // keeps using [`run_writer`] directly
            // with the inline-proxy `classify`
            // closure + the
            // [`inline_proxy_payload`] id
            // projection. The new
            // [`run_standard_writer`] helper is
            // `O`-constrained to
            // `ResourceWriteOutcome` and the
            // inline-proxy enums are bespoke by
            // design.
            let effective_apply = apply && !dry_run;
            let leaf = format!("set proxy remove {id}");
            run_writer(
                output,
                Summaries::standard(NOUN, ResourceVerb::Remove),
                &WriteEnvelope {
                    name: &id,
                    leaf: &leaf,
                },
                || cmd::remove_proxy(&paths, &id, effective_apply),
                classify_inline,
                code_for,
                inline_proxy_payload,
            )
        }
        SetProxyCmd::Import {
            path,
            apply,
            dry_run,
        } => {
            let effective_apply = apply && !dry_run;
            // The `name` slot is the path string
            // (the operator's input). `envelope.leaf`
            // carries the literal `set proxy import
            // /tmp/list.txt` for log-grep.
            let path_str = path.display().to_string();
            let leaf = format!("set proxy import {path_str}");
            run_writer(
                output,
                // `import` isn't a `ResourceVerb`
                // (the standard 4-verb table has
                // add/remove/enable/disable); its
                // summary shape is bespoke ("imported
                // inline proxies" / "would import
                // inline proxies (dry-run)" / "no new
                // inline proxies to import") so it
                // uses [`Summaries::custom`] (Round
                // 26) instead of the 3-line literal.
                Summaries::custom(
                    "imported inline proxies",
                    "would import inline proxies (dry-run)",
                    "no new inline proxies to import",
                ),
                &WriteEnvelope {
                    name: &path_str,
                    leaf: &leaf,
                },
                || cmd::import_proxy(&paths, &path, effective_apply),
                classify_import,
                code_for,
                import_payload,
            )
        }
    }
}

/// Projects an `InlineProxyError` to the stable CLI
/// `code:` used in the JSON error envelope. The four
/// variants are 1-for-1 with the `set proxy <verb>`
/// failure modes; pre-Round-23 each leaf inlined this
/// match and the `code` strings drifted between call
/// sites. The shared `common::run_writer` envelope
/// surfaces the same `code` to every operator-facing
/// `jq` consumer.
fn code_for(error: &InlineProxyError) -> &'static str {
    match error {
        InlineProxyError::InvalidUri(_) => "proxy.invalid_uri",
        InlineProxyError::AlreadyDeclared(_) => "proxy.already_declared",
        InlineProxyError::NotDeclared(_) => "proxy.not_declared",
        InlineProxyError::Io(_) => "proxy.write_failed",
    }
}

/// Round 25: the `classify` closure for
/// `InlineProxyOutcome`. The `set proxy add | edit |
/// remove` leaves use this; `import` has its own
/// `classify_import` because `ImportOutcome` is a
/// separate enum. Round 23 folded the per-call id
/// into the outcome variants (the `Applied` /
/// `DryRun` cases now carry `id: String`), so the
/// `match` arms destructure the enum but only
/// inspect the variant tag. The `InlineProxyOutcome`
/// enum does not surface a `NoChange` arm today
/// (the writer's `add_proxy` short-circuits on
/// duplicate URIs with `AlreadyDeclared`; `edit` /
/// `remove` short-circuit on unknown ids with
/// `NotDeclared`); the closure still matches
/// `NoChange` for the future caller that needs
/// idempotent success.
fn classify_inline(outcome: &InlineProxyOutcome) -> OutcomeKind {
    match outcome {
        InlineProxyOutcome::DryRun { .. } => OutcomeKind::DryRun,
        InlineProxyOutcome::Applied { .. } => OutcomeKind::Applied,
    }
}

/// Round 25: the `classify` closure for
/// `ImportOutcome`. `NoOp` is the idempotent
/// success path — the operator passed `--apply`,
/// but every line of the import file was already
/// declared, so the writer did not change any
/// on-disk state. The dispatch surfaces this as
/// `dry_run: false` (the user did not opt in to a
/// preview) with a distinct human summary
/// (`"no new inline proxies to import"`) so the
/// operator can tell apart "I imported N
/// entries" from "I confirmed everything was
/// already there". Pre-Round 25 `NoOp` was
/// collapsed into `DryRun` (the writer returned
/// `DryRun` when `count == 0`), which lied to
/// the operator: `--apply` followed by a
/// `dry_run: true` envelope suggested the write
/// was held back when in fact it was held back by
/// the no-change check, not the dry-run policy.
fn classify_import(outcome: &ImportOutcome) -> OutcomeKind {
    match outcome {
        ImportOutcome::DryRun { .. } => OutcomeKind::DryRun,
        ImportOutcome::Applied { .. } => OutcomeKind::Applied,
        ImportOutcome::NoOp => OutcomeKind::NoChange,
    }
}

/// Extra-payload projection for `InlineProxyOutcome`.
/// The on-disk `id` is the only data the envelope needs
/// beyond the base `name` / `leaf` / `dry_run` triple.
/// The dispatch's `add` / `edit` / `remove` leaves
/// reuse this closure verbatim.
fn inline_proxy_payload(outcome: &InlineProxyOutcome) -> serde_json::Value {
    let id = match outcome {
        InlineProxyOutcome::Applied { id } | InlineProxyOutcome::DryRun { id } => id.clone(),
    };
    serde_json::json!({ "id": id })
}

/// Extra-payload projection for `ImportOutcome`. The
/// `count` is reported for every variant; `NoOp`
/// collapses to `count: 0`. The path lives in
/// `envelope.name` (the user-typed `path`), so we
/// only need to project the count here.
fn import_payload(outcome: &ImportOutcome) -> serde_json::Value {
    let count = match outcome {
        ImportOutcome::Applied { count } | ImportOutcome::DryRun { count } => *count,
        ImportOutcome::NoOp => 0,
    };
    serde_json::json!({ "count": count })
}

#[cfg(test)]
mod dry_run_envelope_tests;

/// `sysproxy pac [url]` dispatch (extracted for the 100-line budget):
/// without a URL the CLI writes a generated PAC into the state
/// directory and points the desktop at it; with a URL that URL is used
/// verbatim. The daemon only performs the desktop switch (auto mode +
/// autoconfig-url); the PAC authoring is fully offline.
fn dispatch_pac(url: Option<String>, options: crate::cli::CliOptions) -> ExitCode {
    let pac_url = match url {
        Some(url) => url,
        None => match generate_pac_file() {
            Ok(url) => url,
            Err(code) => return code,
        },
    };
    crate::client::run_client(
        crate::ClientCommand::Sys(SysCmd::ProxyPac(pac_url)),
        options,
    )
}

/// Writes the caly-generated PAC file into the state directory and
/// returns its `file://` URL for the desktop's autoconfig-url.
///
/// PAC policy: local subnets and `.local` names go DIRECT, everything
/// else via the local proxy port (the config's mixed port, default
/// 7890) — the same policy surface the manual `sysproxy on` engages.
fn generate_pac_file() -> Result<String, ExitCode> {
    let paths = caly_platform::paths::AppPaths::from_env();
    // The PAC must point at the port the daemon would actually listen on.
    // A missing config falls back to the daemon's 7890 default, but a
    // present-but-unreadable config must fail loudly — silently generating a
    // PAC against the wrong port black-holes everything through the proxy.
    let port: u16 = match crate::daemon_config::load_from(paths.config.clone()) {
        Ok(Some(config)) => config.kernel.mixed_port,
        Ok(None) => 7890,
        Err(error) => {
            return Err(pac_write_error(&format!(
                "cannot read config.yaml for the proxy port: {error}"
            )));
        }
    };
    let content = format!(
        r#"// Generated by caly `sysproxy pac` — local subnets direct,
// everything else through the local proxy (127.0.0.1:{port}).
function FindProxyForURL(url, host) {{
    if (isPlainHostName(host) ||
        isInNet(host, "10.0.0.0", "255.0.0.0") ||
        isInNet(host, "172.16.0.0", "255.240.0.0") ||
        isInNet(host, "192.168.0.0", "255.255.0.0") ||
        isInNet(host, "127.0.0.0", "255.0.0.0") ||
        isInNet(host, "169.254.0.0", "255.255.0.0") ||
        dnsDomainIs(host, ".local"))
        return "DIRECT";
    return "PROXY 127.0.0.1:{port}";
}}
"#
    );
    // `AppPaths::state` already includes the `caly` component.
    let pac_dir = paths.state;
    if let Err(error) = std::fs::create_dir_all(&pac_dir) {
        return Err(pac_write_error(&format!(
            "cannot create state directory: {error}"
        )));
    }
    let pac_path = pac_dir.join("proxy.pac");
    // Atomic publish so a concurrent reader never sees a half-written
    // file (same discipline as the geoip cache download).
    let staging = pac_dir.join(format!("proxy.pac.{}.tmp", std::process::id()));
    if let Err(error) = std::fs::write(&staging, content) {
        return Err(pac_write_error(&format!(
            "cannot write the PAC file: {error}"
        )));
    }
    if let Err(error) = std::fs::rename(&staging, &pac_path) {
        let _ = std::fs::remove_file(&staging);
        return Err(pac_write_error(&format!(
            "cannot publish the PAC file: {error}"
        )));
    }
    Ok(format!("file://{}", pac_path.display()))
}

/// Builds the structured error for a `sysproxy pac` file-operation failure
/// and prints it through the same envelope as other CLI errors.
fn pac_write_error(detail: &str) -> ExitCode {
    let cli = crate::output::CliError::new(
        "sysproxy.pac_write_failed",
        detail.to_owned(),
        "sysproxy pac",
    );
    eprintln!("{}", cli.message);
    cli.exit_code()
}

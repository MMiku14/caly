//! Round 15: `set rule-provider` CLI leaf dispatcher.
//!
//! Each `SetRuleProviderCmd` variant is one leaf in the
//! `set rule-provider` family. The 6 CRUD leaves
//! (`add-http` / `add-file` / `add-inline` / `remove` /
//! `enable` / `disable`) round-trip
//! `rule_providers: Vec<RuleProviderConfig>` in
//! `config.yaml` through `client::rule_provider`. The
//! `refresh` leaf drives the daemon RPC through the
//! `Refreshable` trait (Round 14 fully wired).
//!
//! Round 12 was all `planned_ok(...)` place-holders; Round 13
//! wired the writer module; Round 15 replaces the dispatch
//! arms 1-for-1 with real writer calls (no API churn);
//! Round 21 routes the 4 CRUD leaves through the shared
//! `commands::set::common::run_writer` envelope;
//! Round 29 collapses the 4 CRUD leaves through the
//! higher-level `common::run_standard_writer` so each
//! leaf is a one-line call site.

use std::process::ExitCode;

use crate::cli::{RuleProviderSourceSpec, SetRuleProviderCmd};
use crate::client::rule_provider as cmd;
use crate::commands::set::common::{crud_dispatch, run_standard_writer, ResourceVerb};
use crate::output::CliOutput;

/// The singular resource name used by
/// [`run_standard_writer`] for every `set
/// rule-provider` CRUD leaf. Mirrors the
/// `proxy_group::NOUN` constant — both were
/// Round 23 single-points-of-truth introduced
/// when the 4 inline `Summaries { applied, dry_run }`
/// blocks per resource collapsed into one
/// `Summaries::standard(NOUN, verb)` call. Round 29:
/// `NOUN` is now passed to `run_standard_writer` as
/// the `noun` argument (the helper builds the
/// `Summaries::standard` triple itself).
const NOUN: &str = "rule provider";

/// The leaf-prefix token (the second token in the
/// `set rule-provider <verb> <id>` leaf string).
/// Kept distinct from [`NOUN`] because the leaf
/// wants the CLI form (`"set rule-provider add
/// geosite-cn"`) while the human summary wants
/// the long form (`"rule provider added"`).
const CLI_PREFIX: &str = "rule-provider";

/// Projects a `cmd::RpWriteError` to the stable CLI
/// `code:` used in the JSON error envelope.
fn code_for(error: &cmd::RpWriteError) -> &'static str {
    use cmd::RpWriteError as E;
    match error {
        E::InvalidName(_) => "rule_provider.invalid_name",
        E::AlreadyDeclared(_) => "rule_provider.already_declared",
        E::NotDeclared(_) => "rule_provider.not_declared",
        E::InvalidBehavior(_) => "rule_provider.invalid_behavior",
        E::InvalidUrl(_) => "rule_provider.invalid_url",
        E::Shared(shared) => match shared {
            crate::client::resource_writer::ResourceError::Invalid(_) => "rule_provider.invalid",
            crate::client::resource_writer::ResourceError::Write(_) => "rule_provider.write_failed",
            crate::client::resource_writer::ResourceError::Parse(_) => "rule_provider.parse_failed",
            crate::client::resource_writer::ResourceError::Validate(_) => {
                "rule_provider.validate_failed"
            }
            crate::client::resource_writer::ResourceError::AlreadyDeclared(_)
            | crate::client::resource_writer::ResourceError::NotDeclared(_) => {
                "rule_provider.invalid"
            }
        },
    }
}

/// Dispatches a `set rule-provider` leaf to the matching
/// implementation. Round 15: every CRUD leaf is a real
/// writer; `refresh` is the typed trait path; `list` reads
/// the layered `rule_providers:` list.
pub fn dispatch(
    cmd_arg: SetRuleProviderCmd,
    options: crate::cli::CliOptions,
    output: CliOutput,
) -> ExitCode {
    let paths = caly_platform::paths::AppPaths::from_env();
    dispatch_with_paths(cmd_arg, &paths, options, output)
}

/// Round 25: the path-injected dispatch. The
/// `dispatch` shim resolves the operator's `AppPaths`
/// from process env (the production path); the
/// `dispatch_with_paths` form takes the paths
/// explicitly so a test can drive the dispatch with
/// a hermetic `AppPaths::from_env_vars` fixture
/// without mutating the operator's XDG state. The
/// two arms are 1-for-1 — the path resolution is
/// the only thing that differs.
///
/// Round 30: the 4 CRUD bodies (`add` / `remove` /
/// `enable` / `disable`) inlined into the match
/// arms. Pre-Round 30 each was a 4-arg private
/// helper (`add(paths, name, source, apply, output)`
/// etc.) that did nothing but build a
/// `run_standard_writer` call. The 4 helpers were
/// 50 lines of pure indirection — the dispatch's
/// `match` had 5 lines of `match X => helper_x(…)`
/// with no logic in between. The Round 30 shape
/// inlines each helper into its `match` arm so
/// the dispatch is one expression per verb: the
/// 4 helper functions and the 5 `match arm =>
/// helper_x(…)` call sites collapse into the
/// 4-arm `match` below.
pub fn dispatch_with_paths(
    cmd_arg: SetRuleProviderCmd,
    paths: &caly_platform::paths::AppPaths,
    _options: crate::cli::CliOptions,
    output: CliOutput,
) -> ExitCode {
    match cmd_arg {
        SetRuleProviderCmd::Add {
            name,
            source,
            apply,
            dry_run,
        } => {
            // `--apply` and `--dry-run` are mutually exclusive
            // (clap enforces it). `apply: true` writes,
            // `dry_run: true` runs every check + reports
            // planned, both `false` (default) is the safe
            // dry-run path.
            let effective_apply = apply && !dry_run;
            let writer_source = to_writer_source(&source);
            // Round 30: the `add` arm keeps an
            // inline `run_standard_writer` call
            // because the writer takes 2 extra
            // arguments (`&writer_source` /
            // `RpBehavior::DEFAULT`) that the
            // generic `crud_dispatch<W>` helper
            // would have to thread through. The
            // 3 simple-CRUD arms below
            // (`remove` / `enable` / `disable`)
            // collapse into the helper.
            run_standard_writer(
                output,
                CLI_PREFIX,
                NOUN,
                ResourceVerb::Add,
                &name,
                || {
                    cmd::add_provider(
                        paths,
                        &name,
                        &writer_source,
                        cmd::RpBehavior::DEFAULT,
                        effective_apply,
                    )
                },
                code_for,
            )
        }
        SetRuleProviderCmd::Remove {
            name,
            apply,
            dry_run,
        } => crud_dispatch(
            output,
            CLI_PREFIX,
            NOUN,
            &name,
            apply,
            dry_run,
            ResourceVerb::Remove,
            |apply| cmd::remove_provider(paths, &name, apply),
            code_for,
        ),
        SetRuleProviderCmd::Enable {
            name,
            apply,
            dry_run,
        } => crud_dispatch(
            output,
            CLI_PREFIX,
            NOUN,
            &name,
            apply,
            dry_run,
            ResourceVerb::Enable,
            // `set_enabled` has a fixed `enabled` first
            // arg, so it can't be passed as a plain
            // function reference; the closure binds it.
            |apply| cmd::set_enabled(paths, &name, true, apply),
            code_for,
        ),
        SetRuleProviderCmd::Disable {
            name,
            apply,
            dry_run,
        } => crud_dispatch(
            output,
            CLI_PREFIX,
            NOUN,
            &name,
            apply,
            dry_run,
            ResourceVerb::Disable,
            |apply| cmd::set_enabled(paths, &name, false, apply),
            code_for,
        ),
        SetRuleProviderCmd::Refresh { name } => refresh(name, output),
        SetRuleProviderCmd::List => list(paths, output),
    }
}

/// Maps the CLI `RuleProviderSourceSpec` to the writer's
/// `RpSourceSpec`. The writer doesn't import the CLI
/// grammar (separation of concerns), so this conversion
/// stays here.
fn to_writer_source(source: &RuleProviderSourceSpec) -> cmd::RpSourceSpec {
    match source {
        RuleProviderSourceSpec::Http { url, interval_ms } => cmd::RpSourceSpec::Http {
            url: url.clone(),
            interval_ms: *interval_ms,
        },
        RuleProviderSourceSpec::File { path } => cmd::RpSourceSpec::File { path: path.clone() },
        RuleProviderSourceSpec::Inline { payload } => cmd::RpSourceSpec::Inline {
            payload: payload.clone(),
        },
    }
}

fn refresh(name: Option<String>, output: CliOutput) -> ExitCode {
    let target = name.map_or_else(
        || crate::commands::refresh::RefreshTarget::RuleProviderName("(all)".to_owned()),
        crate::commands::refresh::RefreshTarget::RuleProviderName,
    );
    // Round 33: the `Ok` / `Err` envelope split
    // folded into the shared
    // [`refresh::run_refresh`] helper. The
    // dispatch owns only the `target` projection;
    // the success / error envelope shape lives
    // in one place (the `Refreshable` trait impl
    // for `RuleProviderRefresh`).
    crate::commands::refresh::run_refresh::<crate::commands::refresh::RuleProviderRefresh>(
        output,
        target,
        "set rule-provider refresh",
    )
}

fn list(paths: &caly_platform::paths::AppPaths, output: CliOutput) -> ExitCode {
    let providers = read_declared(paths);
    if output.is_json() {
        let items: Vec<serde_json::Value> = providers
            .into_iter()
            .map(|(name, enabled, behavior, source_kind)| {
                serde_json::json!({
                    "name": name,
                    "enabled": enabled,
                    "behavior": behavior,
                    "source": source_kind,
                })
            })
            .collect();
        output.success(
            &format!("listed {} rule provider(s)", items.len()),
            serde_json::json!({ "providers": items, "count": items.len() }),
        );
    } else {
        for (name, enabled, behavior, source_kind) in &providers {
            println!(
                "rule-provider: {name} ({source_kind}, behavior: {behavior}, enabled: {enabled})"
            );
        }
        println!("{} provider(s)", providers.len());
    }
    ExitCode::SUCCESS
}

/// Round 15: real `list` impl using the layered loader. The
/// loader returns the merged `rule_providers:` list (base +
/// `CALY_PROFILE` + `config.d/*`). Each entry's `enabled`
/// and `behavior` flags are read from the typed config.
pub(crate) fn read_declared(
    paths: &caly_platform::paths::AppPaths,
) -> Vec<(String, bool, String, String)> {
    use caly_profile::loader::{InMemoryProfileResolver, LayeredConfigPaths, LoaderLimits};
    use caly_profile::schema::RuleProviderSourceConfig;
    let limits = LoaderLimits::secure_default();
    let layered = LayeredConfigPaths::new(paths.config.clone(), None);
    let resolver = InMemoryProfileResolver::lenient();
    match caly_profile::loader::load_layered_yaml_with(&layered, limits, &resolver) {
        Ok(config) => config
            .rule_providers
            .into_iter()
            .map(|p| {
                let behavior = format!("{:?}", p.behavior).to_lowercase();
                let source_kind = match &p.kind {
                    RuleProviderSourceConfig::Http { .. } => "http",
                    RuleProviderSourceConfig::File { .. } => "file",
                    RuleProviderSourceConfig::Inline { .. } => "inline",
                }
                .to_owned();
                (p.name, p.enabled, behavior, source_kind)
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod dispatch_tests;

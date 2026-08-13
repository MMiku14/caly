//! W1 seam S-W1-1 (cli-v3-design.md C-M): the v3 `node` domain
//! merges protocol nodes and proxy groups, so
//! `node remove|enable|disable <name>` must resolve the entry kind
//! before dispatching. Entry names share one closed set at the
//! domain level, so a name is a node XOR a group: both-hit is a
//! schema breach (defensive error), neither-hit exits 1 with the
//! available candidates.

use std::process::ExitCode;

use crate::cli::{EntryWriteCmd, SetProxyCmd, SetProxyGroupCmd};
use crate::output::CliOutput;

pub fn dispatch(
    cmd: EntryWriteCmd,
    options: crate::cli::CliOptions,
    output: CliOutput,
) -> ExitCode {
    let paths = caly_platform::paths::AppPaths::from_env();
    let id = match &cmd {
        EntryWriteCmd::Remove { id, .. }
        | EntryWriteCmd::Enable { id, .. }
        | EntryWriteCmd::Disable { id, .. } => id.clone(),
    };
    let node_names: Vec<String> = crate::client::inline_proxy::list_proxies(&paths)
        .into_iter()
        .map(|(name, _uri)| name)
        .collect();
    let group_names: Vec<String> = crate::client::proxy_group::read_declared(&paths)
        .into_iter()
        .map(|(name, _enabled, _kind, _count, _has_url_test)| name)
        .collect();
    let is_node = node_names.contains(&id);
    let is_group = group_names.contains(&id);
    match (is_node, is_group) {
        (true, false) => match cmd {
            EntryWriteCmd::Remove { id, apply, dry_run } => super::set::proxy::dispatch(
                SetProxyCmd::Remove { id, apply, dry_run },
                options,
                output,
            ),
            EntryWriteCmd::Enable { .. } | EntryWriteCmd::Disable { .. } => {
                // Unified error exit: JSON-aware envelope instead of a
                // bare stderr line (2026-08-12 error-chain refactor).
                crate::output::report_error_returning(
                    output,
                    crate::output::CliError::new(
                        "node.enable.unsupported",
                        format!(
                            "`{id}` is a node; nodes carry no enable state. \
                             enable/disable applies to groups."
                        ),
                        "node enable/disable",
                    ),
                )
            }
        },
        (false, true) => {
            let mapped = match cmd {
                EntryWriteCmd::Remove { id, apply, dry_run } => SetProxyGroupCmd::Remove {
                    name: id,
                    apply,
                    dry_run,
                },
                EntryWriteCmd::Enable { id, apply, dry_run } => SetProxyGroupCmd::Enable {
                    name: id,
                    apply,
                    dry_run,
                },
                EntryWriteCmd::Disable { id, apply, dry_run } => SetProxyGroupCmd::Disable {
                    name: id,
                    apply,
                    dry_run,
                },
            };
            super::proxy_group::dispatch_with_paths(mapped, &paths, options, output)
        }
        (false, false) => {
            let mut candidates = node_names;
            candidates.extend(group_names);
            crate::output::report_error_returning(
                output,
                crate::output::CliError::new(
                    "entry.not_declared",
                    format!(
                        "entry `{id}` is not declared in the offline config.\navailable: {}",
                        candidates.join(", "),
                    ),
                    "node (any leaf)",
                ),
            )
        }
        (true, true) => {
            // Closed-set breach: the domain layer rejects duplicate
            // names across nodes and groups, so this should be
            // unreachable. Defensive error, never a panic.
            crate::output::report_error_returning(
                output,
                crate::output::CliError::new(
                    "entry.name_collision",
                    format!(
                        "`{id}` is declared as BOTH a node and a group; \
                         the config violates the entry-name closed set. Run `caly config validate`."
                    ),
                    "node (any leaf)",
                ),
            )
        }
    }
}

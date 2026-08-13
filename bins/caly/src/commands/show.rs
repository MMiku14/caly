//! `caly show …` — read-only queries.
//!
//! Round 15: every read-only leaf now routes through
//! the typed `client::run_client` / `client::profile` /
//! `client::config_generate` helpers directly. The
//! `bridge` shim was retired for the 5 `old_*`
//! functions; the 3 `sub_*` shims stay (used by
//! `show sub` / `set sub import` which still need the
//! legacy `SubCmd` enum until Round 16+ folds them).

use std::process::ExitCode;

use crate::cli::{
    SetSubCmd, ShowCmd, ShowConfigCmd, ShowCoreCmd, ShowProfileCmd, ShowProxyCmd, ShowSubCmd,
};
use crate::client::hex;
use crate::output::CliOutput;

pub fn run(cmd: ShowCmd, options: crate::cli::CliOptions, output: CliOutput) -> ExitCode {
    match cmd {
        ShowCmd::Status => super::status::run(options, output),
        ShowCmd::Core(c) => core(c, options, output),
        ShowCmd::Sub(c) => sub(c, options, output),
        ShowCmd::Profile(c) => profile(c, options, output),
        ShowCmd::Proxy(c) => proxy(c, options, output),
        ShowCmd::Config(c) => config(c, options, output),
        ShowCmd::SysproxyStatus => super::sys_status::run_sysproxy(&options),
        ShowCmd::TunStatus => super::sys_status::run_tun(&options),
    }
}

fn core(c: ShowCoreCmd, options: crate::cli::CliOptions, output: CliOutput) -> ExitCode {
    use crate::client::legacy::CoreCmd;
    // Translate `show core X` to the existing `client::run_client` CoreCmd shape.
    let mapped = match c {
        ShowCoreCmd::Nodes => {
            // W3a: `node list --format=tree` renders the offline declared
            // entry tree (the online wire carries no group membership until
            // W3b); bare `node --format=tree` reaches the same projection.
            if options.format == Some(crate::cli::OutputFormat::Tree) {
                let paths = caly_platform::paths::AppPaths::from_env();
                return super::node_tree::render_declared_tree(&paths, false, output.is_json());
            }
            CoreCmd::ListNodes
        }
        ShowCoreCmd::Groups => CoreCmd::ProxyGroups,
        ShowCoreCmd::Connections => CoreCmd::ListConnections,
        ShowCoreCmd::Flow { watch } => return super::flow::run(options, output, watch),
        ShowCoreCmd::FlowTrace { id, watch } => {
            return super::flow::trace(options, output, &id, watch);
        }
        ShowCoreCmd::Traffic => CoreCmd::Traffic,
        ShowCoreCmd::Mode => CoreCmd::Mode("rule".to_owned()), // read-only; results printed
        ShowCoreCmd::Rules { r#match: None } => CoreCmd::Rules,
        ShowCoreCmd::Rules { r#match: Some(t) } => CoreCmd::RuleMatch(t),
        ShowCoreCmd::Health => return health(options, output),
    };
    crate::client::run_client(crate::ClientCommand::Core(mapped), options)
}

fn health(_options: crate::cli::CliOptions, output: CliOutput) -> ExitCode {
    // Round 16: read the daemon snapshot directly via the
    // UDS path the daemon publishes. The snapshot carries
    // every field the operator wants at a glance (uptime,
    // restart count, applied core, observed rates, node
    // count). If the daemon is unreachable, surface a
    // typed `core.daemon_unreachable` error.
    //
    // Round 22: connect + handshake + snapshot boilerplate
    // moved to `client::execute::with_uds_snapshot` so
    // every read-only `show` leaf reuses one error envelope.
    crate::client::execute::with_uds_snapshot(|_client, snapshot| {
        let core_label = snapshot
            .applied
            .core_kind
            .map_or_else(|| "none".to_owned(), |c| format!("{c:?}"));
        let summary = format!(
            "daemon: {}\n  revision: {}\n  applied core: {}\n  nodes: {}\n  up: {} B/s\n  down: {} B/s\n  restarts: {}\n  restart backoff: {} ms",
            hex(snapshot.daemon_instance_id),
            snapshot.revision,
            core_label,
            snapshot.nodes.len(),
            snapshot.observed.upload_bytes_per_second,
            snapshot.observed.download_bytes_per_second,
            snapshot.observed.core_restart_count,
            snapshot.observed.core_restart_backoff_ms,
        );
        if output.is_json() {
            let payload = serde_json::json!({
                "daemon_instance_id": hex(snapshot.daemon_instance_id),
                "revision": snapshot.revision,
                "applied_core": core_label,
                "nodes": snapshot.nodes.len(),
                "up_bps": snapshot.observed.upload_bytes_per_second,
                "down_bps": snapshot.observed.download_bytes_per_second,
                "restarts": snapshot.observed.core_restart_count,
                "restart_backoff_ms": snapshot.observed.core_restart_backoff_ms,
            });
            println!("{payload}");
        } else {
            println!("{summary}");
        }
        ExitCode::SUCCESS
    })
}

fn sub(c: ShowSubCmd, options: crate::cli::CliOptions, output: CliOutput) -> ExitCode {
    let json = options.json;
    match c {
        ShowSubCmd::Providers => crate::client::subscription::list_providers(json, options.format),
        ShowSubCmd::Parse {
            path,
            userinfo,
            apply,
            name,
        } => {
            if apply {
                // `--apply` registers the parsed file as a subscription
                // source — the same path as `sub add <path> --apply`, with
                // the name defaulting to the file stem so batch use stays
                // deterministic (no interactive naming prompt).
                let default_name = path
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned());
                return super::set::sub::dispatch(
                    SetSubCmd::Add {
                        url: path.display().to_string(),
                        name: name.or(default_name),
                        refresh_every_minutes: None,
                        apply: true,
                        dry_run: false,
                    },
                    options,
                    output,
                );
            }
            crate::client::subscription::check_subscription(&path, userinfo.as_deref(), json)
        }
    }
}

fn profile(c: ShowProfileCmd, options: crate::cli::CliOptions, output: CliOutput) -> ExitCode {
    // Round 15: directly call `client::profile`
    // functions, bypassing the `bridge` shim.
    let paths = crate::client::profile::resolve_paths();
    match c {
        ShowProfileCmd::List => {
            // Render declared profiles in human or JSON mode.
            let config = match crate::client::profile::load_declared_profiles(&paths) {
                Ok((c, _)) => c,
                Err(e) => {
                    eprintln!("profile list failed: {e}");
                    return ExitCode::from(1);
                }
            };
            let profiles = crate::client::profile::list_declared(&config);
            if output.is_json() {
                let items: Vec<serde_json::Value> = profiles
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "id": p.id,
                            "name": p.name,
                            "description": p.description,
                            "source": format!("{:?}", p.source).to_lowercase(),
                        })
                    })
                    .collect();
                let payload = serde_json::json!({
                    "profiles": items,
                    "count": items.len(),
                });
                println!("{payload}");
                ExitCode::SUCCESS
            } else {
                // W2 (cli-v3-design.md §5.1, Q6): adaptive
                // table/TSV replaces the v1 `profile: <id> …`
                // lines and the trailing count line.
                let mode = crate::output::table_mode(options.format);
                let rows: Vec<Vec<String>> = profiles
                    .iter()
                    .map(|p| {
                        vec![
                            p.id.clone(),
                            p.name.clone().unwrap_or_else(|| "-".to_owned()),
                            format!("{:?}", p.source).to_lowercase(),
                        ]
                    })
                    .collect();
                let headers: &[&str] = match mode {
                    crate::output::TableMode::Tsv => &["id", "name", "source"],
                    crate::output::TableMode::Table => &["ID", "NAME", "SOURCE"],
                };
                crate::output::print_table(mode, headers, &rows);
                ExitCode::SUCCESS
            }
        }
        ShowProfileCmd::Show { id } => {
            let (config, _store) = match crate::client::profile::load_declared_profiles(&paths) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("profile show failed: {e}");
                    return ExitCode::from(1);
                }
            };
            if let Some(p) = crate::client::profile::find_declared(&config, &id) {
                println!(
                    "profile: {} ({})",
                    p.id,
                    format!("{:?}", p.source).to_lowercase()
                );
                ExitCode::SUCCESS
            } else {
                eprintln!("profile `{id}` is not declared");
                ExitCode::from(1)
            }
        }
    }
}

fn proxy(c: ShowProxyCmd, _options: crate::cli::CliOptions, output: CliOutput) -> ExitCode {
    // W1-β: the only remaining variant is the online single-entry
    // detail (`node show <id>`); the v1 list/groups leaves moved
    // to the node domain (`node list` / `node groups`).
    show_proxy_id(&c, output)
}

fn show_proxy_id(c: &ShowProxyCmd, output: CliOutput) -> ExitCode {
    // Round 16: read the snapshot, find the node by 32-hex
    // id, print its protocol + latency + name. Falls back
    // to a name-prefix match when the id isn't a 32-hex
    // string (mirrors `core select` matching).
    //
    // Round 22: connect + handshake + snapshot boilerplate
    // moved to `client::execute::with_uds_snapshot` so
    // every read-only `show` leaf reuses one error envelope.
    let id = match c {
        ShowProxyCmd::Show { id } => id.clone(),
    };
    crate::client::execute::with_uds_snapshot(|_client, snapshot| {
        let node = snapshot.nodes.iter().find(|n| hex(n.node_id) == id);
        let resolved = if let Some(n) = node {
            n.clone()
        } else {
            // Fall back to name prefix match.
            let prefix_matches: Vec<_> = snapshot
                .nodes
                .iter()
                .filter(|n| n.name.to_lowercase().starts_with(&id.to_lowercase()) || n.name == id)
                .collect();
            match prefix_matches.len() {
                0 => {
                    eprintln!("error: no node matches `{id}`");
                    return ExitCode::from(1);
                }
                1 => prefix_matches[0].clone(),
                _ => {
                    let ids: Vec<String> = prefix_matches
                        .iter()
                        .map(|n| format!("  {} ({})", hex(n.node_id), n.name.as_str()))
                        .collect();
                    eprintln!(
                        "error: {} nodes match `{id}`; use a longer prefix or a 32-hex id:\n{}",
                        prefix_matches.len(),
                        ids.join("\n"),
                    );
                    return ExitCode::from(1);
                }
            }
        };
        let latency = resolved
            .latency_ms
            .map_or_else(|| "-".to_owned(), |ms| format!("{ms} ms"));
        let availability = if resolved.available { "yes" } else { "no" };
        if output.is_json() {
            let payload = serde_json::json!({
                "id": hex(resolved.node_id),
                "name": resolved.name.as_str(),
                "protocol": resolved.protocol,
                "available": resolved.available,
                "latency_ms": resolved.latency_ms,
            });
            println!("{payload}");
        } else {
            println!("{:<11}{}", "node:", hex(resolved.node_id));
            println!("{:<11}{}", "name:", resolved.name.as_str());
            println!("{:<11}{}", "protocol:", resolved.protocol);
            println!("{:<11}{}", "available:", availability);
            println!("{:<11}{}", "latency:", latency);
        }
        ExitCode::SUCCESS
    })
}

fn config(c: ShowConfigCmd, options: crate::cli::CliOptions, _output: CliOutput) -> ExitCode {
    let json = options.json;
    match c {
        ShowConfigCmd::Path => crate::client::config_generate::config_path(json),
        ShowConfigCmd::Files => crate::client::config_generate::config_files(json),
        ShowConfigCmd::Validate { file: None } => {
            crate::client::config_generate::validate_active_config(json)
        }
        ShowConfigCmd::Validate { file: Some(p) } => {
            crate::client::execute::check_single_config_file(&p, json)
        }
    }
}

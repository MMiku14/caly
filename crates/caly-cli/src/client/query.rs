//! Offline Clash API queries against the running core's controller.
//!
//! These are read-only queries (proxy groups, connections, traffic) that the
//! CLI executes directly against the controller address, without going through
//! a daemon mutation operation. The core kind is resolved from `--core` or
//! `CALY_CORE`, and the controller address from `CALY_MIHOMO_CONTROLLER` /
//! `CALY_SINGBOX_CONTROLLER` (defaulting to the standard loopback ports).

use std::process::ExitCode;

use caly_corectl::contract::{ConnectionSummary, KernelControl, ProxyGroup};

/// The controller timeout for read-only queries.
pub(crate) const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

mod probe;

pub(crate) use probe::{DelayProbe, resolve_samples};
pub(crate) use probe::{median, probe_delay};

pub fn run_query(core: &str, query: Query, json: bool) -> ExitCode {
    let mut control = match build_control(core) {
        Ok(control) => control,
        Err(error) => {
            return crate::client::output::report_failure(&format!("query failed: {error}"), json);
        }
    };
    match query {
        Query::ProxyGroups => match control.proxy_groups(QUERY_TIMEOUT) {
            Ok(groups) => print_groups(&groups, json),
            Err(error) => report_query_error(&format!("{error}"), json),
        },
        Query::Connections => match control.connections(QUERY_TIMEOUT) {
            Ok(summary) => print_connections(&summary, json),
            Err(error) => report_query_error(&format!("{error}"), json),
        },
        Query::Traffic => match control.traffic(QUERY_TIMEOUT) {
            Ok((download, upload)) => print_traffic(download, upload, json),
            Err(error) => report_query_error(&format!("{error}"), json),
        },
        Query::UrlTest { name, url, samples } => {
            match probe_delay(&mut *control, &name, url.as_deref(), QUERY_TIMEOUT, samples) {
                Ok(probe) => print_delay(&name, &probe, json),
                Err(error) => report_query_error(&format!("{error}"), json),
            }
        }
    }
}

/// A read-only Clash API query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Query {
    ProxyGroups,
    Connections,
    Traffic,
    /// Latency test for a named proxy (`GET /proxies/{name}/delay`).
    UrlTest {
        name: String,
        /// Probe target URL override (default: Google generate_204).
        url: Option<String>,
        /// Sample count (1..=5). Defaults to
        /// [`DEFAULT_SAMPLES`] when the operator
        /// doesn't pass `--samples`.
        samples: u32,
    },
}

/// Builds the core controller from the resolved core kind.
pub(crate) fn build_control(core: &str) -> Result<Box<dyn KernelControl + Send>, String> {
    // The daemon persists its generated controller secret to an owner-only file
    // so offline queries authenticate; `CALY_CONTROLLER_SECRET` overrides it.
    let secret = std::env::var("CALY_CONTROLLER_SECRET")
        .ok()
        .or_else(read_daemon_secret)
        .and_then(|value| caly_domain::BoundedText::new(value).ok());
    // Controller endpoints come from the config file (then env, then defaults).
    let controllers = crate::config::controllers().map_err(|e| format!("{e:?}"))?;
    match core {
        "mihomo" => caly_corectl::mihomo::MihomoHttpControl::new(controllers.mihomo, secret)
            .map(|control| Box::new(control) as Box<dyn KernelControl + Send>)
            .map_err(|error| format!("{error:?}")),
        "sing-box" => caly_corectl::sing_box::SingBoxHttpControl::new(controllers.sing_box, secret)
            .map(|control| Box::new(control) as Box<dyn KernelControl + Send>)
            .map_err(|error| format!("{error:?}")),
        other => Err(format!("unknown core `{other}`; use mihomo or sing-box")),
    }
}

/// Reads the controller secret the daemon persisted under the XDG runtime root.
fn read_daemon_secret() -> Option<String> {
    let path = caly_platform::paths::AppPaths::from_env().controller_secret_path();
    std::fs::read_to_string(&path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn print_groups(groups: &[ProxyGroup], json: bool) -> ExitCode {
    if json {
        let value: Vec<serde_json::Value> = groups
            .iter()
            .map(|group| {
                serde_json::json!({
                    "name": group.name,
                    "selected": group.selected,
                })
            })
            .collect();
        println!("{}", serde_json::json!({ "proxy_groups": value }));
    } else {
        println!("proxy groups:");
        for group in groups {
            let selected = group.selected.as_deref().unwrap_or("-");
            println!("  {} (selected: {selected})", group.name);
        }
    }
    ExitCode::SUCCESS
}

fn print_connections(summary: &ConnectionSummary, json: bool) -> ExitCode {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "active": summary.active,
                "download_bytes": summary.download_bytes,
                "upload_bytes": summary.upload_bytes,
            })
        );
    } else {
        println!(
            "connections: {} active | download {} B | upload {} B",
            summary.active, summary.download_bytes, summary.upload_bytes
        );
    }
    ExitCode::SUCCESS
}

fn print_traffic(download: u64, upload: u64, json: bool) -> ExitCode {
    if json {
        println!(
            "{}",
            serde_json::json!({ "download_bytes": download, "upload_bytes": upload })
        );
    } else {
        println!("traffic: download {download} B | upload {upload} B");
    }
    ExitCode::SUCCESS
}

/// Renders one node's probe result. The human form
/// always shows the median and the spread so the
/// operator can see jitter at a glance; the JSON
/// form carries the full sample list for scripts
/// that want the distribution.
fn print_delay(name: &str, probe: &DelayProbe, json: bool) -> ExitCode {
    if json {
        let mut payload = serde_json::json!({
            "name": name,
            "samples": probe.samples(),
            "median_ms": probe.median_ms(),
            "min_ms": probe.min_ms(),
            "max_ms": probe.max_ms(),
            "jitter_ms": probe.jitter_ms(),
            "stdev_ms": probe.stdev_ms(),
            "url": probe.url(),
        });
        // The legacy `delay_ms` field stays in the
        // envelope so the historical `jq .delay_ms`
        // queries keep working — it is the median,
        // the same value the human form prints.
        if let Some(ms) = probe.median_ms()
            && let serde_json::Value::Object(ref mut map) = payload
        {
            map.insert("delay_ms".to_owned(), serde_json::Value::from(ms));
        }
        println!("{payload}");
    } else {
        match probe.median_ms() {
            Some(median) => {
                let jitter = probe
                    .jitter_ms()
                    .map(|j| format!(" (± {j} ms)"))
                    .unwrap_or_default();
                println!("{name}: {median} ms{jitter}");
            }
            None => println!("{name}: unreachable / no latency"),
        }
    }
    ExitCode::SUCCESS
}

fn report_query_error(message: &str, json: bool) -> ExitCode {
    crate::client::output::report_failure(&format!("query failed: {message}"), json)
}

#[cfg(test)]
mod probe_tests;

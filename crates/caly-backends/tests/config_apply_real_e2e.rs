//! E2E: the full Mihomo subscription → render → validate → apply loop against
//! the real kernel binary. Feeds a subscription body through the cached
//! subscription backend (which indexes the shared proxy registry), then drives
//! the ConfigActor backend to parse+render+validate and commit, and finally
//! re-validates the committed file with `mihomo -t`. Skips when the pinned
//! binary is absent.

use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use caly_backends::{CachedSubscriptionBackend, MihomoConfigBackend, MihomoNodeRegistry};
use caly_domain::SubscriptionId;
use caly_platform::paths::test_helpers::unique_path_under;
use caly_ports::{ConfigActorPort, ConfigCandidate, SubscriptionCommandBackend};

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn mihomo_binary() -> PathBuf {
    if let Some(value) = std::env::var_os("CALY_MIHOMO_BIN") {
        PathBuf::from(value)
    } else {
        manifest().join("../../vendor/bin/mihomo")
    }
}

fn unique_dir(tag: &str) -> PathBuf {
    unique_path_under("caly-apply-e2e", tag)
}

/// Ensures a usable `geoip.metadb` exists inside `dir` for `mihomo -t`.
///
/// Mihomo downloads this database when the validated config references GEOIP
/// data (e.g. a DNS `fallback-filter` with `geoip`); its Go HTTP stack can be
/// blocked where the system stack (curl/wget) still works, and the download is
/// large enough to blow bounded validator timeouts. We therefore pre-provision
/// the database: a system copy (`/etc/mihomo/geoip.metadb`) or the shared
/// download cache, each validated with a real `mihomo -t` probe before reuse
/// so a stale or truncated file cannot silently trigger a re-download hang.
/// Returns `Err` when no usable database is available; callers must turn that
/// into an explicit skip.
/// Candidates for a known-good `geoip.metadb`, tried in order: the system
/// Mihomo data dir first (a proper, complete database), then the shared
/// download cache. Each candidate is validated by running the real `mihomo
/// -t` against a probe config that references GEOIP (the same fallback-filter
/// shape the DNS e2e renders), so a stale or truncated file is never reused:
/// that would otherwise make Mihomo silently re-download the database during
/// validation and hang past the validator timeout when the network is blocked.
fn ensure_geoip_database(dir: &std::path::Path) -> Result<(), String> {
    let target = dir.join("geoip.metadb");
    if target.is_file() {
        return Ok(());
    }
    let cache_dir = std::env::temp_dir().join("caly-geoip-cache");
    let cached = cache_dir.join("geoip.metadb");
    let mut candidates: Vec<PathBuf> = vec![PathBuf::from("/etc/mihomo/geoip.metadb")];
    if cached.is_file() {
        candidates.push(cached.clone());
    }
    for source in candidates {
        if source_geoip_metadb_usable(dir, &source) {
            std::fs::copy(&source, &target).map_err(|e| format!("copy: {e}"))?;
            return Ok(());
        }
    }
    // Both the system database and any cached copy were absent or unusable;
    // attempt a fresh download into the cache, then validate it before use.
    std::fs::create_dir_all(&cache_dir).map_err(|e| format!("cache dir: {e}"))?;
    let _ = std::fs::remove_file(&cached);
    let url = "https://github.com/MetaCubeX/meta-rules-dat/releases/download/latest/geoip.metadb";
    let downloaded = Command::new("curl")
        .args(["-sSL", "--max-time", "120", "-o"])
        .arg(&cached)
        .arg(url)
        .status()
        .map_err(|_| "curl is not available".to_owned())?
        .success()
        || Command::new("wget")
            .args(["-q", "-O"])
            .arg(&cached)
            .arg(url)
            .status()
            .map_err(|_| "wget is not available".to_owned())?
            .success();
    if !downloaded || !source_geoip_metadb_usable(dir, &cached) {
        let _ = std::fs::remove_file(&cached);
        return Err(
            "no usable geoip.metadb: system database absent and download failed/invalid".to_owned(),
        );
    }
    std::fs::copy(&cached, &target).map_err(|e| format!("copy: {e}"))?;
    Ok(())
}

/// Runs `mihomo -t` with a minimal config whose `fallback-filter` references
/// GEOIP data, returning whether the given database file is usable. This
/// exercises the exact code path the DNS e2e renders, so a truncated or stale
/// database is detected before it can hang a real validate with a re-download.
fn source_geoip_metadb_usable(dir: &std::path::Path, source: &std::path::Path) -> bool {
    if source.metadata().map_or(0, |m| m.len()) < 1_000_000 {
        return false;
    }
    let _ = std::fs::create_dir_all(dir);
    if std::fs::copy(source, dir.join("geoip.metadb")).is_err() {
        return false;
    }
    let probe = dir.join("geoip.probe.yaml");
    if std::fs::write(
        &probe,
        "mixed-port: 7890\nmode: rule\ndns:\n  enable: true\n  enhanced-mode: fake-ip\n  fallback:\n    - 1.1.1.1\n  fallback-filter:\n    geoip: true\n    geoip-code: CN\n",
    )
    .is_err()
    {
        return false;
    }
    let binary = mihomo_binary();
    let status = if binary.is_file() {
        Command::new(&binary)
            .args(["-d"])
            .arg(dir)
            .args(["-f"])
            .arg(&probe)
            .arg("-t")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    } else {
        return false;
    };
    let _ = std::fs::remove_file(&probe);
    matches!(status, Ok(status) if status.success())
}

#[test]
fn subscription_refresh_drives_validated_config_apply() -> Result<(), String> {
    let binary = mihomo_binary();
    if !binary.is_file() {
        eprintln!("skipping: no mihomo binary at {}", binary.display());
        return Ok(());
    }
    let dir = unique_dir("loop");
    let workdir = dir.join("work");
    std::fs::create_dir_all(&workdir).map_err(|e| e.to_string())?;
    let destination = dir.join("mihomo.yaml");

    // 1. Build a real fixture body (base64 URI-line subscription).
    let body = std::fs::read(manifest().join("../../fixtures/subscription-20260803.txt"))
        .map_err(|_| "fixture not found")?;
    let id = SubscriptionId::from_bytes([21; 16]);
    let registry: MihomoNodeRegistry = Arc::new(Mutex::new(std::collections::BTreeMap::new()));

    // 2. Feed + refresh the subscription so the registry indexes proxy material.
    let mut subscription = CachedSubscriptionBackend::new().with_node_registry(registry.clone());
    subscription.put_source(id, body);
    let nodes = subscription
        .refresh(id, caly_ports::RefreshMode::default())
        .map_err(|e| format!("subscription refresh failed: {e:?}"))?
        .nodes;
    assert!(!nodes.is_empty(), "fixture must project nodes");
    assert!(
        registry.lock().map_err(|_| "poisoned")?.len() >= nodes.len(),
        "every projected node should be indexed for rendering"
    );

    // 3. Apply: render → validate (in parse_and_render) → commit.
    let mut config = MihomoConfigBackend::new(destination.clone())
        .with_registry(registry)
        .with_validation(binary.clone(), workdir, Duration::from_secs(20));
    let prepared = config
        .parse_and_render(ConfigCandidate { id: [9; 16] })
        .map_err(|e| format!("parse_and_render failed: {e:?}"))?;
    let committed = config
        .commit_candidate(prepared)
        .map_err(|e| format!("commit failed: {e:?}"))?;
    assert_eq!(committed.generation, 1);

    // 4. The committed file must be owner-only and accepted by `mihomo -t`.
    let mode = std::fs::metadata(&destination)
        .map_err(|e| e.to_string())?
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "committed config must be owner-only");
    let check = Command::new(&binary)
        .args(["-t", "-f"])
        .arg(&destination)
        .output()
        .map_err(|e| format!("cannot run mihomo: {e}"))?;
    assert!(
        check.status.success(),
        "committed config rejected by mihomo -t:\n{}",
        String::from_utf8_lossy(&check.stdout)
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn sniffer_block_is_accepted_by_real_kernel() -> Result<(), String> {
    let binary = mihomo_binary();
    if !binary.is_file() {
        eprintln!("skipping: no mihomo binary at {}", binary.display());
        return Ok(());
    }
    let dir = unique_dir("sniffer");
    let workdir = dir.join("work");
    std::fs::create_dir_all(&workdir).map_err(|e| e.to_string())?;
    let destination = dir.join("mihomo.yaml");

    let sniffer = caly_profile::schema::SnifferConfig {
        enabled: true,
        ..caly_profile::schema::SnifferConfig::default()
    };
    let mut config = MihomoConfigBackend::new(destination.clone())
        .with_sniffer(sniffer)
        .with_validation(binary.clone(), workdir, Duration::from_secs(20));
    let prepared = config
        .parse_and_render(ConfigCandidate { id: [11; 16] })
        .map_err(|e| format!("parse_and_render failed: {e:?}"))?;
    config
        .commit_candidate(prepared)
        .map_err(|e| format!("commit failed: {e:?}"))?;
    let text = std::fs::read_to_string(&destination).map_err(|e| e.to_string())?;
    assert!(text.contains("sniffer:"), "sniffer block must be rendered");
    assert!(text.contains("override-destination: true"));
    let check = Command::new(&binary)
        .args(["-t", "-f"])
        .arg(&destination)
        .output()
        .map_err(|e| format!("cannot run mihomo: {e}"))?;
    assert!(
        check.status.success(),
        "sniffer config rejected by mihomo -t:\n{}",
        String::from_utf8_lossy(&check.stdout)
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn dns_block_with_filters_is_accepted_by_real_kernel() -> Result<(), String> {
    use caly_dns::{DnsMode, DnsSettingsBuilder, FallbackFilter};
    let binary = mihomo_binary();
    if !binary.is_file() {
        eprintln!("skipping: no mihomo binary at {}", binary.display());
        return Ok(());
    }
    let dir = unique_dir("dns-filters");
    let workdir = dir.join("work");
    std::fs::create_dir_all(&workdir).map_err(|e| e.to_string())?;
    // The fallback-filter uses geoip: CN; pre-provision the database so the
    // validator never attempts its own download (which can hang or be blocked).
    if let Err(reason) = ensure_geoip_database(&workdir) {
        eprintln!("skipping: {reason}");
        let _ = std::fs::remove_dir_all(&dir);
        return Ok(());
    }
    let destination = dir.join("mihomo.yaml");

    let filter = FallbackFilter::new(
        true,
        Some("CN".to_owned()),
        vec!["240.0.0.0/4".to_owned(), "0.0.0.0/32".to_owned()],
        vec!["+.google.com".to_owned(), "+.github.com".to_owned()],
    )
    .map_err(|e| format!("{e:?}"))?;
    let dns = DnsSettingsBuilder::new()
        .enabled(true)
        .mode(DnsMode::FakeIp)
        .push_nameserver("8.8.8.8")
        .map_err(|e| format!("{e:?}"))?
        .push_fallback("1.1.1.1")
        .map_err(|e| format!("{e:?}"))?
        .fake_ip_range("198.18.0.1/16")
        .map_err(|e| format!("{e:?}"))?
        .fake_ip_filter(&["+.local".to_owned(), "+.stun.*".to_owned()])
        .map_err(|e| format!("{e:?}"))?
        .fallback_filter(filter)
        .ipv6(false)
        .listen("127.0.0.1:1053")
        .map_err(|e| format!("{e:?}"))?
        .build()
        .map_err(|e| format!("{e:?}"))?
        .ok_or_else(|| "dns disabled".to_owned())?;

    let mut config = MihomoConfigBackend::new(destination.clone())
        .with_kernel(
            7890,
            false,
            "*".to_owned(),
            "error".to_owned(),
            9090,
            Some(dns),
        )
        .with_validation(binary.clone(), workdir.clone(), Duration::from_secs(20));
    let prepared = config
        .parse_and_render(ConfigCandidate { id: [12; 16] })
        .map_err(|e| format!("parse_and_render failed: {e:?}"))?;
    config
        .commit_candidate(prepared)
        .map_err(|e| format!("commit failed: {e:?}"))?;
    let text = std::fs::read_to_string(&destination).map_err(|e| e.to_string())?;
    assert!(text.contains("fake-ip-filter:"), "filter block must render");
    assert!(
        text.contains("fallback-filter:"),
        "fallback-filter must render"
    );
    let check = Command::new(&binary)
        .args(["-t", "-d"])
        .arg(&workdir)
        .arg("-f")
        .arg(&destination)
        .output()
        .map_err(|e| format!("cannot run mihomo: {e}"))?;
    assert!(
        check.status.success(),
        "dns-filter config rejected by mihomo -t:\n{}",
        String::from_utf8_lossy(&check.stdout)
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

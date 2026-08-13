//! Declarative CLI mapping tests, W1-β edition.
//!
//! The v3 resource-domain grammar (cli-v3-design.md §4) is the
//! single source of truth; the v1 5-namespace paths (`show` /
//! `set` / v1 `tool` leaves) keep parsing through the alias
//! expansion layer (`aliases.rs`), so most v1-era assertions below
//! still run against the deprecated spellings AND the canonical
//! v3 spellings — the final block asserts the two are equal.

use super::*;

fn parse(values: &[&str]) -> Invocation {
    match parse_args(values.iter().map(|value| (*value).to_owned())) {
        Ok(invocation) => invocation,
        Err(error) => {
            panic!("parse failed for {values:?}: {error}")
        }
    }
}

fn parse_err(values: &[&str]) -> clap::Error {
    match parse_args(values.iter().map(|value| (*value).to_owned())) {
        Ok(_) => {
            panic!("expected parse failure for {values:?}")
        }
        Err(error) => error,
    }
}

// ── Global options ──────────────────────────────────────────────

#[test]
fn global_options_are_mapped_to_all_namespaces() {
    let invocation = parse(&["--json", "show", "status"]);
    assert!(invocation.options.json);
    assert_eq!(invocation.command, Command::Show(ShowCmd::Status));

    let invocation = parse(&["--core", "mihomo", "show", "core", "nodes"]);
    assert_eq!(invocation.options.core.as_deref(), Some("mihomo"));
    assert_eq!(
        invocation.command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Nodes))
    );

    let invocation = parse(&[
        "daemon",
        "--mihomo-bin=/opt/mihomo",
        "--sing-box-bin",
        "/opt/sing-box",
    ]);
    assert_eq!(invocation.command, Command::Daemon);
    assert_eq!(
        invocation.options.mihomo_bin,
        Some(PathBuf::from("/opt/mihomo"))
    );
    assert_eq!(
        invocation.options.sing_box_bin,
        Some(PathBuf::from("/opt/sing-box"))
    );
}

// ── tool namespace (5 leaves) ──────────────────────────────────

#[test]
fn tool_namespace_parses_all_four_leaves() {
    assert_eq!(
        parse(&["tool", "help"]).command,
        Command::Tool(ToolCmd::Help(None))
    );
    assert_eq!(
        parse(&["tool", "help", "set"]).command,
        Command::Tool(ToolCmd::Help(Some("set".to_owned())))
    );
    assert_eq!(
        parse(&["tool", "version"]).command,
        Command::Tool(ToolCmd::Version)
    );
    assert_eq!(
        parse(&["tool", "doctor"]).command,
        Command::Tool(ToolCmd::Doctor { fix: false })
    );
    assert_eq!(
        parse(&["tool", "doctor", "--fix"]).command,
        Command::Tool(ToolCmd::Doctor { fix: true })
    );
    assert_eq!(
        parse(&["tool", "dns", "example.com"]).command,
        Command::Tool(ToolCmd::Dns(Some("example.com".to_owned())))
    );
    assert_eq!(
        parse(&["tool", "dns"]).command,
        Command::Tool(ToolCmd::Dns(None))
    );
}

// ── show namespace (6 resources) ───────────────────────────────

#[test]
fn show_status_is_a_top_level_subcommand() {
    assert_eq!(
        parse(&["show", "status"]).command,
        Command::Show(ShowCmd::Status)
    );
}

#[test]
fn show_core_parses_seven_leaves() {
    assert_eq!(
        parse(&["show", "core", "nodes"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Nodes))
    );
    assert_eq!(
        parse(&["show", "core", "groups"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Groups))
    );
    assert_eq!(
        parse(&["show", "core", "connections"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Connections))
    );
    assert_eq!(
        parse(&["show", "core", "traffic"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Traffic))
    );
    assert_eq!(
        parse(&["show", "core", "mode"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Mode))
    );
    assert_eq!(
        parse(&["show", "core", "rules"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Rules { r#match: None }))
    );
    assert_eq!(
        parse(&["show", "core", "rules", "--match", "example.com"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Rules {
            r#match: Some("example.com".to_owned())
        }))
    );
    // New Round 11 capability: `show core health`
    assert_eq!(
        parse(&["show", "core", "health"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Health))
    );
}

#[test]
fn show_sub_parses_three_leaves_including_renamed_parse() {
    assert_eq!(
        parse(&["show", "sub", "providers"]).command,
        Command::Show(ShowCmd::Sub(ShowSubCmd::Providers))
    );
    assert_eq!(
        parse(&["show", "sub", "parse", "/tmp/sub.txt"]).command,
        Command::Show(ShowCmd::Sub(ShowSubCmd::Parse {
            path: PathBuf::from("/tmp/sub.txt"),
            userinfo: None,
            apply: false,
            name: None,
        }))
    );
    assert_eq!(
        parse(&[
            "show",
            "sub",
            "parse",
            "/tmp/sub.txt",
            "--userinfo",
            "u=100"
        ])
        .command,
        Command::Show(ShowCmd::Sub(ShowSubCmd::Parse {
            path: PathBuf::from("/tmp/sub.txt"),
            userinfo: Some("u=100".to_owned()),
            apply: false,
            name: None,
        }))
    );
    // W1-β: the v1 preview leaf is aliased onto `sub import`,
    // whose dry-run default IS the same preview (cli.rs note).
    assert_eq!(
        parse(&["show", "sub", "import"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Import {
            path: None,
            clipboard: false,
            apply: false,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["show", "sub", "import", "--clipboard"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Import {
            path: None,
            clipboard: true,
            apply: false,
            dry_run: false,
        }))
    );
}

#[test]
fn show_profile_parses_list_and_show() {
    assert_eq!(
        parse(&["show", "profile", "list"]).command,
        Command::Show(ShowCmd::Profile(ShowProfileCmd::List))
    );
    assert_eq!(
        parse(&["show", "profile", "show", "team"]).command,
        Command::Show(ShowCmd::Profile(ShowProfileCmd::Show {
            id: "team".to_owned()
        }))
    );
}

#[test]
fn show_proxy_parses_three_leaves() {
    // W1-β: the deprecated v1 leaves rewrite onto the node
    // domain — `node list` (online) / `node groups` (T-1).
    assert_eq!(
        parse(&["show", "proxy", "list"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Nodes))
    );
    assert_eq!(
        parse(&["show", "proxy", "show", "abc"]).command,
        Command::Show(ShowCmd::Proxy(ShowProxyCmd::Show {
            id: "abc".to_owned()
        }))
    );
    assert_eq!(
        parse(&["show", "proxy", "groups"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Groups))
    );
}

#[test]
fn show_config_parses_three_leaves() {
    assert_eq!(
        parse(&["show", "config", "path"]).command,
        Command::Show(ShowCmd::Config(ShowConfigCmd::Path))
    );
    assert_eq!(
        parse(&["show", "config", "files"]).command,
        Command::Show(ShowCmd::Config(ShowConfigCmd::Files))
    );
    assert_eq!(
        parse(&["show", "config", "validate"]).command,
        Command::Show(ShowCmd::Config(ShowConfigCmd::Validate { file: None }))
    );
    assert_eq!(
        parse(&["show", "config", "validate", "--file", "/tmp/x.yaml"]).command,
        Command::Show(ShowCmd::Config(ShowConfigCmd::Validate {
            file: Some(PathBuf::from("/tmp/x.yaml")),
        }))
    );
}

// ── set namespace (7 resources) ────────────────────────────────

#[test]
fn set_core_parses_eight_leaves_including_url_test() {
    assert_eq!(
        parse(&["set", "core", "start"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Start))
    );
    assert_eq!(
        parse(&["set", "core", "stop"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Stop))
    );
    assert_eq!(
        parse(&["set", "core", "restart"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Restart))
    );
    assert_eq!(
        parse(&["set", "core", "switch", "mihomo"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Switch("mihomo".to_owned())))
    );
    assert_eq!(
        parse(&["set", "core", "switch", "sing-box"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Switch("sing-box".to_owned())))
    );
    assert_eq!(
        parse(&["set", "core", "select"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Select {
            node: None,
            delay: false,
            poll: false,
        }))
    );
    assert_eq!(
        parse(&["set", "core", "select", "node1", "--delay"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Select {
            node: Some("node1".to_owned()),
            delay: true,
            poll: false,
        }))
    );
    // Round 24 (debug): the `--group Auto` flag on
    // `set core select` was silently dropped by the
    // dispatch (the wire `SelectProxy { node_id }` has
    // no group field). The right contract is to refuse
    // the flag at parse time so the operator sees a
    // clap error instead of a no-op success. Lock the
    // rejection: `--group` is now an unknown flag.
    for removed in [
        vec!["set", "core", "select", "--group", "Auto"],
        vec!["set", "core", "select", "node1", "--group", "Auto"],
    ] {
        let result = parse_args(removed.iter().map(|s| (*s).to_owned()));
        assert!(
            result.is_err(),
            "expected --group on select to be rejected: {removed:?} got {result:?}"
        );
    }
    assert_eq!(
        parse(&["set", "core", "mode", "global"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Mode("global".to_owned())))
    );
    assert_eq!(
        parse(&["set", "core", "close-connections"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::CloseConnections))
    );
    assert_eq!(
        parse(&["set", "core", "delay", "--all"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Delay {
            name: None,
            all: true,
            url: None,
            samples: None,
        }))
    );
    // New Round 11 capability: `set core url-test`
    assert_eq!(
        parse(&["set", "core", "url-test", "node1"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::UrlTest {
            name: "node1".to_owned(),
            url: None,
            samples: None,
            apply: false
        }))
    );
    assert_eq!(
        parse(&["set", "core", "url-test", "node1", "--url", "http://g"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::UrlTest {
            name: "node1".to_owned(),
            url: Some("http://g".to_owned()),
            samples: None,
            apply: false
        }))
    );
    // Round 24 (debug): the `--timeout` flag was
    // silently dropped by the dispatch (the offline
    // `Query::UrlTest` resolver has a hard-coded
    // `QUERY_TIMEOUT`, no per-call override). Drop the
    // flag from the grammar so the operator sees a
    // clap error instead of a no-op success. Lock the
    // rejection here.
    for removed in [
        vec!["set", "core", "url-test", "node1", "--timeout", "3000"],
        vec![
            "set",
            "core",
            "url-test",
            "node1",
            "--url",
            "http://g",
            "--timeout",
            "3000",
        ],
    ] {
        let result = parse_args(removed.iter().map(|s| (*s).to_owned()));
        assert!(
            result.is_err(),
            "expected --timeout on url-test to be rejected: {removed:?} got {result:?}"
        );
    }
}

#[test]
fn set_core_delay_samples_flag_is_optional_and_parses() {
    // Round 20: `--samples N` lets the operator
    // pick the per-URL sample count for jitter-
    // sensitive probes. The default (None) is
    // resolved at the dispatch boundary; the
    // CLI just preserves whatever the user passed.
    assert_eq!(
        parse(&["set", "core", "delay", "node1", "--samples", "1"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Delay {
            name: Some("node1".to_owned()),
            all: false,
            url: None,
            samples: Some(1),
        }))
    );
    assert_eq!(
        parse(&["set", "core", "delay", "--all", "--samples", "5"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Delay {
            name: None,
            all: true,
            url: None,
            samples: Some(5),
        }))
    );
    assert_eq!(
        parse(&["set", "core", "url-test", "node1", "--samples", "3"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::UrlTest {
            name: "node1".to_owned(),
            url: None,
            samples: Some(3),
            apply: false
        }))
    );
}

#[test]
fn set_proxy_parses_four_crud_leaves_plus_on_off() {
    // New Round 11 capabilities: add / edit / remove / import
    assert_eq!(
        parse(&["set", "proxy", "add", "ss://abc"]).command,
        Command::Set(SetCmd::Proxy(SetProxyCmd::Add {
            uri: "ss://abc".to_owned(),
            group: None,
            apply: false,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["set", "proxy", "add", "ss://abc", "--group", "G", "--apply"]).command,
        Command::Set(SetCmd::Proxy(SetProxyCmd::Add {
            uri: "ss://abc".to_owned(),
            group: Some("G".to_owned()),
            apply: true,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["set", "proxy", "edit", "id1", "--apply"]).command,
        Command::Set(SetCmd::Proxy(SetProxyCmd::Edit {
            id: "id1".to_owned(),
            apply: true,
            dry_run: false,
        }))
    );
    // W1-β (C-M): remove routes through the Entry seam — the v1
    // node path and the proxy-group path share the v3 leaf.
    assert_eq!(
        parse(&["set", "proxy", "remove", "id1"]).command,
        Command::Set(SetCmd::Entry(EntryWriteCmd::Remove {
            id: "id1".to_owned(),
            apply: false,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["set", "proxy", "import", "/tmp/uri.list", "--apply"]).command,
        Command::Set(SetCmd::Proxy(SetProxyCmd::Import {
            path: PathBuf::from("/tmp/uri.list"),
            apply: true,
            dry_run: false,
        }))
    );
    // Old `set sys proxy on/off` is now `set proxy on/off`
    assert_eq!(
        parse(&["set", "proxy", "on"]).command,
        Command::Set(SetCmd::Proxy(SetProxyCmd::On))
    );
    assert_eq!(
        parse(&["set", "proxy", "off"]).command,
        Command::Set(SetCmd::Proxy(SetProxyCmd::Off))
    );
}

#[test]
fn set_tun_parses_on_off() {
    assert_eq!(
        parse(&["set", "tun", "on"]).command,
        Command::Set(SetCmd::Tun(true))
    );
    assert_eq!(
        parse(&["set", "tun", "off"]).command,
        Command::Set(SetCmd::Tun(false))
    );
}

#[test]
fn set_sub_refresh_leaf_parses_beta2b_flags() {
    // W2-β2b (§4.3): [name-or-url] [--force] [--async].
    assert_eq!(
        parse(&["sub", "refresh"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Refresh {
            target: None,
            force: false,
            asynchronous: false,
        }))
    );
    assert_eq!(
        parse(&["sub", "refresh", "airport", "--force", "--async"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Refresh {
            target: Some("airport".to_owned()),
            force: true,
            asynchronous: true,
        }))
    );
    // The legacy spelling still expands onto the same leaf.
    assert_eq!(
        parse(&["set", "sub", "refresh"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Refresh {
            target: None,
            force: false,
            asynchronous: false,
        }))
    );
}

#[test]
fn set_sub_set_leaf_parses_change_flags() {
    // W2-β2b (§4.3): the three change flags each map through;
    // `--every` hours scale to minutes like `sub add`.
    assert_eq!(
        parse(&["sub", "set", "airport", "--url", "https://n", "--apply"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Set {
            target: "airport".to_owned(),
            url: Some("https://n".to_owned()),
            name: None,
            refresh_every_minutes: None,
            apply: true,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["sub", "set", "airport", "--every", "12"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Set {
            target: "airport".to_owned(),
            url: None,
            name: None,
            refresh_every_minutes: Some(720),
            apply: false,
            dry_run: false,
        }))
    );
    // The required ArgGroup rejects a bare `set` with no change.
    let result = parse_args(["sub", "set", "airport"].iter().map(|v| (*v).to_owned()));
    assert!(result.is_err(), "sub set without a change flag must fail");
    // --purge reaches the remove leaf.
    assert_eq!(
        parse(&["sub", "remove", "https://x", "--purge", "--apply"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Remove {
            url: "https://x".to_owned(),
            purge: true,
            apply: true,
            dry_run: false,
        }))
    );
}

#[test]
fn set_sub_parses_five_crud_leaves() {
    assert_eq!(
        parse(&["set", "sub", "refresh"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Refresh {
            target: None,
            force: false,
            asynchronous: false,
        }))
    );
    assert_eq!(
        parse(&["set", "sub", "add", "https://x"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Add {
            url: "https://x".to_owned(),
            name: None,
            refresh_every_minutes: None,
            apply: false,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&[
            "set",
            "sub",
            "add",
            "https://x",
            "--name",
            "team",
            "--apply"
        ])
        .command,
        Command::Set(SetCmd::Sub(SetSubCmd::Add {
            url: "https://x".to_owned(),
            name: Some("team".to_owned()),
            refresh_every_minutes: None,
            apply: true,
            dry_run: false,
        }))
    );
    // W2-β2a (Q5): `--every <hours>` scales to minutes in the
    // convert step; explicit 0 means "static".
    assert_eq!(
        parse(&["set", "sub", "add", "https://x", "--every", "24"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Add {
            url: "https://x".to_owned(),
            name: None,
            refresh_every_minutes: Some(1_440),
            apply: false,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["set", "sub", "add", "https://x", "--every", "0"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Add {
            url: "https://x".to_owned(),
            name: None,
            refresh_every_minutes: Some(0),
            apply: false,
            dry_run: false,
        }))
    );
    // The hours range is capped so the ×60 scale can never overflow.
    let result = parse_args(
        ["set", "sub", "add", "https://x", "--every", "1000001"]
            .iter()
            .map(|value| (*value).to_owned()),
    );
    assert!(result.is_err(), "--every beyond the cap must fail to parse");
    assert_eq!(
        parse(&["set", "sub", "remove", "https://x", "--apply"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Remove {
            url: "https://x".to_owned(),
            purge: false,
            apply: true,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["set", "sub", "enable", "https://x", "--apply"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Enable {
            url: "https://x".to_owned(),
            apply: true,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["set", "sub", "disable", "https://x", "--apply"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Disable {
            url: "https://x".to_owned(),
            apply: true,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["set", "sub", "import", "/tmp/u", "--clipboard", "--apply"]).command,
        Command::Set(SetCmd::Sub(SetSubCmd::Import {
            path: Some(PathBuf::from("/tmp/u")),
            clipboard: true,
            apply: true,
            dry_run: false,
        }))
    );
}

#[test]
fn set_profile_parses_seven_leaves() {
    assert_eq!(
        parse(&["set", "profile", "add", "team", "remote:https://x"]).command,
        Command::Set(SetCmd::Profile(SetProfileCmd::Add {
            id: "team".to_owned(),
            source: "remote:https://x".to_owned(),
            apply: false,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["set", "profile", "remove", "team", "--apply"]).command,
        Command::Set(SetCmd::Profile(SetProfileCmd::Remove {
            id: "team".to_owned(),
            apply: true,
            dry_run: false,
        }))
    );
    // New Round 11: edit / export / enable / disable
    assert_eq!(
        parse(&["set", "profile", "edit", "team", "--apply"]).command,
        Command::Set(SetCmd::Profile(SetProfileCmd::Edit {
            id: "team".to_owned(),
            apply: true,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["set", "profile", "export", "team", "--out", "/tmp/p.yaml"]).command,
        Command::Set(SetCmd::Profile(SetProfileCmd::Export {
            id: "team".to_owned(),
            out: PathBuf::from("/tmp/p.yaml"),
        }))
    );
    assert_eq!(
        parse(&["set", "profile", "enable", "team", "--apply"]).command,
        Command::Set(SetCmd::Profile(SetProfileCmd::Enable {
            id: "team".to_owned(),
            apply: true,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["set", "profile", "disable", "team", "--apply"]).command,
        Command::Set(SetCmd::Profile(SetProfileCmd::Disable {
            id: "team".to_owned(),
            apply: true,
            dry_run: false,
        }))
    );
    // Refresh stays (was Round 10); now under `set` namespace.
    // Round 12: refresh always writes (the body IS the cache),
    // so `--apply` / `--dry-run` flags are not accepted.
    assert_eq!(
        parse(&["set", "profile", "refresh"]).command,
        Command::Set(SetCmd::Profile(SetProfileCmd::Refresh { id: None }))
    );
    assert_eq!(
        parse(&["set", "profile", "refresh", "team"]).command,
        Command::Set(SetCmd::Profile(SetProfileCmd::Refresh {
            id: Some("team".to_owned())
        }))
    );
}

#[test]
fn set_profile_refresh_rejects_apply_and_dry_run_flags() {
    // Round 12: refresh always writes. A `--apply` / `--dry-run`
    // flag is rejected at parse time, not silently coerced
    // (the previous behavior was a no-op that confused
    // operators about whether a refresh actually wrote).
    for removed in [
        vec!["set", "profile", "refresh", "--apply"],
        vec!["set", "profile", "refresh", "--dry-run"],
        vec!["set", "profile", "refresh", "team", "--apply"],
    ] {
        let result = parse_args(removed.iter().map(|s| (*s).to_owned()));
        assert!(
            result.is_err(),
            "expected refresh flag to be rejected: {removed:?}"
        );
    }
}

#[test]
fn set_config_parses_six_leaves_including_diff() {
    assert_eq!(
        parse(&["set", "config", "apply"]).command,
        Command::Set(SetCmd::Config(SetConfigCmd::Apply))
    );
    assert_eq!(
        parse(&["set", "config", "generate"]).command,
        Command::Set(SetCmd::Config(SetConfigCmd::Generate))
    );
    assert_eq!(
        parse(&["set", "config", "default"]).command,
        Command::Set(SetCmd::Config(SetConfigCmd::Default))
    );
    // New Round 11 capability: `set config diff`
    assert_eq!(
        parse(&["set", "config", "diff"]).command,
        Command::Set(SetCmd::Config(SetConfigCmd::Diff { file: None }))
    );
    assert_eq!(
        parse(&["set", "config", "diff", "--file", "/tmp/c.yaml"]).command,
        Command::Set(SetCmd::Config(SetConfigCmd::Diff {
            file: Some(PathBuf::from("/tmp/c.yaml")),
        }))
    );
    assert_eq!(
        parse(&["set", "config", "edit", "vim"]).command,
        Command::Set(SetCmd::Config(SetConfigCmd::Edit(Editor::Vim)))
    );
    assert_eq!(
        parse(&["set", "config", "edit", "nvim"]).command,
        Command::Set(SetCmd::Config(SetConfigCmd::Edit(Editor::Nvim)))
    );
}

#[test]
fn set_daemon_parses_four_lifecycle_leaves() {
    // New Round 11 capability: `set daemon stop/reload/restart/status`
    assert_eq!(
        parse(&["set", "daemon", "stop"]).command,
        Command::Set(SetCmd::Daemon(SetDaemonCmd::Stop))
    );
    assert_eq!(
        parse(&["set", "daemon", "reload"]).command,
        Command::Set(SetCmd::Daemon(SetDaemonCmd::Reload))
    );
    assert_eq!(
        parse(&["set", "daemon", "restart"]).command,
        Command::Set(SetCmd::Daemon(SetDaemonCmd::Restart))
    );
    // W1-β (C-J): the v1 leaf delegated to the health snapshot;
    // the alias preserves that exactly via `status --verbose`.
    assert_eq!(
        parse(&["set", "daemon", "status"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Health))
    );
}

#[test]
fn set_rule_provider_parses_six_leaves() {
    // Round 12: declarative CRUD for `rule_providers:`. The
    // grammar uses three sub-commands (`add-http` /
    // `add-file` / `add-inline`) so each source kind has its
    // own typed argument shape (no `kind <KIND> <body>` slot).
    use crate::cli::RuleProviderSourceSpec;
    assert_eq!(
        parse(&[
            "set",
            "rule-provider",
            "add-http",
            "geosite-cn",
            "https://x/y"
        ])
        .command,
        Command::Set(SetCmd::RuleProvider(SetRuleProviderCmd::Add {
            name: "geosite-cn".to_owned(),
            source: RuleProviderSourceSpec::Http {
                url: "https://x/y".to_owned(),
                interval_ms: 86_400_000
            },
            apply: false,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["set", "rule-provider", "add-file", "adblock", "/etc/r.yaml"]).command,
        Command::Set(SetCmd::RuleProvider(SetRuleProviderCmd::Add {
            name: "adblock".to_owned(),
            source: RuleProviderSourceSpec::File {
                path: PathBuf::from("/etc/r.yaml")
            },
            apply: false,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&[
            "set",
            "rule-provider",
            "add-inline",
            "small",
            "--payload",
            "a\nb"
        ])
        .command,
        Command::Set(SetCmd::RuleProvider(SetRuleProviderCmd::Add {
            name: "small".to_owned(),
            source: RuleProviderSourceSpec::Inline {
                payload: "a\nb".to_owned()
            },
            apply: false,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["set", "rule-provider", "remove", "geosite-cn"]).command,
        Command::Set(SetCmd::RuleProvider(SetRuleProviderCmd::Remove {
            name: "geosite-cn".to_owned(),
            apply: false,
            dry_run: false
        }))
    );
    assert_eq!(
        parse(&["set", "rule-provider", "enable", "geosite-cn"]).command,
        Command::Set(SetCmd::RuleProvider(SetRuleProviderCmd::Enable {
            name: "geosite-cn".to_owned(),
            apply: false,
            dry_run: false
        }))
    );
    assert_eq!(
        parse(&["set", "rule-provider", "disable", "geosite-cn"]).command,
        Command::Set(SetCmd::RuleProvider(SetRuleProviderCmd::Disable {
            name: "geosite-cn".to_owned(),
            apply: false,
            dry_run: false
        }))
    );
    assert_eq!(
        parse(&["set", "rule-provider", "refresh"]).command,
        Command::Set(SetCmd::RuleProvider(SetRuleProviderCmd::Refresh {
            name: None
        }))
    );
    assert_eq!(
        parse(&["set", "rule-provider", "refresh", "geosite-cn"]).command,
        Command::Set(SetCmd::RuleProvider(SetRuleProviderCmd::Refresh {
            name: Some("geosite-cn".to_owned())
        }))
    );
    assert_eq!(
        parse(&["set", "rule-provider", "list"]).command,
        Command::Set(SetCmd::RuleProvider(SetRuleProviderCmd::List))
    );
}

#[test]
fn set_rule_provider_dry_run_policy_matches_sibling_resources() {
    // The CRUD leaves take the same `--apply` / `--dry-run`
    // policy as the other `set` resources: default dry-run,
    // `--apply` writes, the two flags are mutually exclusive.
    for args in [
        vec![
            "set",
            "rule-provider",
            "add-http",
            "n",
            "https://x",
            "--apply",
        ],
        vec!["set", "rule-provider", "add-file", "n", "/etc/r", "--apply"],
        vec![
            "set",
            "rule-provider",
            "add-inline",
            "n",
            "--payload",
            "x",
            "--apply",
        ],
        vec!["set", "rule-provider", "remove", "n", "--apply"],
        vec!["set", "rule-provider", "enable", "n", "--apply"],
        vec!["set", "rule-provider", "disable", "n", "--apply"],
    ] {
        let result = parse_args(args.iter().map(|s| (*s).to_owned()));
        assert!(
            result.is_ok(),
            "expected apply leaf to parse: {args:?} -> {result:?}"
        );
    }
    for args in [
        vec![
            "set",
            "rule-provider",
            "remove",
            "n",
            "--apply",
            "--dry-run",
        ],
        vec![
            "set",
            "rule-provider",
            "add-http",
            "n",
            "https://x",
            "--apply",
            "--dry-run",
        ],
    ] {
        let result = parse_args(args.iter().map(|s| (*s).to_owned()));
        assert!(
            result.is_err(),
            "expected conflict to be rejected: {args:?}"
        );
    }
}

#[test]
fn set_rule_provider_refresh_rejects_apply_and_dry_run_flags() {
    // Like `set profile refresh`, this leaf always writes.
    // A `--apply` / `--dry-run` flag is rejected at parse time.
    for removed in [
        vec!["set", "rule-provider", "refresh", "--apply"],
        vec!["set", "rule-provider", "refresh", "--dry-run"],
    ] {
        let result = parse_args(removed.iter().map(|s| (*s).to_owned()));
        assert!(
            result.is_err(),
            "expected refresh flag to be rejected: {removed:?}"
        );
    }
}

// ── set proxy-group (Round 20) ─────────────────────────────────

#[test]
fn set_proxy_group_parses_six_leaves() {
    use crate::cli::{ProxyGroupMemberSpec, ProxyGroupTypeSpec};
    // `add` with no members parses to an empty members list;
    // the writer rejects a `select` group with zero members
    // at the schema layer.
    assert_eq!(
        parse(&["set", "proxy-group", "add", "Proxy", "--type", "select"]).command,
        Command::Set(SetCmd::ProxyGroup(SetProxyGroupCmd::Add {
            name: "Proxy".to_owned(),
            group_type: ProxyGroupTypeSpec::Select,
            members: Ok(Vec::new()),
            url: None,
            interval_seconds: None,
            tolerance_ms: None,
            apply: false,
            dry_run: false,
        }))
    );
    // `add` with comma-separated `kind:value` members.
    assert_eq!(
        parse(&[
            "set",
            "proxy-group",
            "add",
            "Auto",
            "--type",
            "url-test",
            "--members",
            "node:hk-1,group:Proxy,direct,reject",
            "--url",
            "http://www.gstatic.com/generate_204",
            "--interval-seconds",
            "300",
            "--tolerance-ms",
            "50",
        ])
        .command,
        Command::Set(SetCmd::ProxyGroup(SetProxyGroupCmd::Add {
            name: "Auto".to_owned(),
            group_type: ProxyGroupTypeSpec::UrlTest,
            members: Ok(vec![
                ProxyGroupMemberSpec::Node {
                    tag: "hk-1".to_owned()
                },
                ProxyGroupMemberSpec::Group {
                    name: "Proxy".to_owned()
                },
                ProxyGroupMemberSpec::Direct,
                ProxyGroupMemberSpec::Reject,
            ]),
            url: Some("http://www.gstatic.com/generate_204".to_owned()),
            interval_seconds: Some(300),
            tolerance_ms: Some(50),
            apply: false,
            dry_run: false,
        }))
    );
    // W1-β (C-M): the proxy-group lifecycle leaves rewrite onto
    // the same `node …` Entry seam as the node ones.
    assert_eq!(
        parse(&["set", "proxy-group", "remove", "Proxy"]).command,
        Command::Set(SetCmd::Entry(EntryWriteCmd::Remove {
            id: "Proxy".to_owned(),
            apply: false,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["set", "proxy-group", "enable", "Proxy"]).command,
        Command::Set(SetCmd::Entry(EntryWriteCmd::Enable {
            id: "Proxy".to_owned(),
            apply: false,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["set", "proxy-group", "disable", "Proxy"]).command,
        Command::Set(SetCmd::Entry(EntryWriteCmd::Disable {
            id: "Proxy".to_owned(),
            apply: false,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["set", "proxy-group", "list"]).command,
        Command::Set(SetCmd::ProxyGroup(SetProxyGroupCmd::List {
            enabled_only: false
        }))
    );
    // `--enabled-only` parses to `List { enabled_only: true }` so the
    // dispatcher's `list(paths, true, output)` filters the in-memory
    // projection. The flag default (`false`) keeps the
    // pre-Round 31 behaviour (`list` shows every declared group).
    assert_eq!(
        parse(&["set", "proxy-group", "list", "--enabled-only"]).command,
        Command::Set(SetCmd::ProxyGroup(SetProxyGroupCmd::List {
            enabled_only: true
        }))
    );
}

#[test]
fn set_proxy_group_dry_run_policy_matches_sibling_resources() {
    // The CRUD leaves take the same `--apply` / `--dry-run`
    // policy as the other `set` resources: default dry-run,
    // `--apply` writes, the two flags are mutually exclusive.
    for args in [
        vec![
            "set",
            "proxy-group",
            "add",
            "n",
            "--type",
            "select",
            "--apply",
        ],
        vec!["set", "proxy-group", "remove", "n", "--apply"],
        vec!["set", "proxy-group", "enable", "n", "--apply"],
        vec!["set", "proxy-group", "disable", "n", "--apply"],
    ] {
        let result = parse_args(args.iter().map(|s| (*s).to_owned()));
        assert!(
            result.is_ok(),
            "expected parse to succeed: {args:?} got {result:?}"
        );
    }
    // The `--apply` / `--dry-run` flags are mutually exclusive
    // (clap enforces it).
    let result = parse_args(
        [
            "set",
            "proxy-group",
            "add",
            "n",
            "--type",
            "select",
            "--apply",
            "--dry-run",
        ]
        .iter()
        .map(|s| (*s).to_owned()),
    );
    assert!(result.is_err(), "expected mutual exclusion: {result:?}");
}

#[test]
fn set_proxy_group_add_rejects_unknown_type() {
    // clap's ValueEnum rejects unknown `--type` values
    // before the dispatch ever runs.
    let result = parse_args(
        ["set", "proxy-group", "add", "n", "--type", "round-robin"]
            .iter()
            .map(|s| (*s).to_owned()),
    );
    assert!(result.is_err(), "expected unknown type to be rejected");
}

#[test]
fn set_proxy_group_add_rejects_unknown_member_kinds() {
    // Round 24 (debug): the grammar's `value_parser`
    // rejects unknown `kind:` prefixes (`ndoe:hk-1`) and
    // unknown bare tokens (`rejct`) at parse time, so a
    // typo surfaces a clear clap error instead of the
    // pre-Round-24 silent drop that produced an empty
    // `members:` list and a misleading "select group with
    // zero members" schema rejection. Lock the new
    // contract: any unrecognised member token is a hard
    // parse error.
    for removed in [
        // Typo in the `node:` prefix
        vec![
            "set",
            "proxy-group",
            "add",
            "n",
            "--type",
            "select",
            "--members",
            "ndoe:hk-1",
        ],
        // Typo in the bare `direct` / `reject` tokens
        vec![
            "set",
            "proxy-group",
            "add",
            "n",
            "--type",
            "select",
            "--members",
            "rejct",
        ],
        // Typo in the `group:` prefix
        vec![
            "set",
            "proxy-group",
            "add",
            "n",
            "--type",
            "select",
            "--members",
            "grop:Auto",
        ],
    ] {
        let result = parse_args(removed.iter().map(|s| (*s).to_owned()));
        assert!(
            result.is_err(),
            "expected unknown member to be rejected: {removed:?} got {result:?}"
        );
    }
    // The four known forms still parse cleanly.
    for accepted in [
        vec![
            "set",
            "proxy-group",
            "add",
            "n",
            "--type",
            "select",
            "--members",
            "node:hk-1",
        ],
        vec![
            "set",
            "proxy-group",
            "add",
            "n",
            "--type",
            "select",
            "--members",
            "group:Auto",
        ],
        vec![
            "set",
            "proxy-group",
            "add",
            "n",
            "--type",
            "select",
            "--members",
            "direct",
        ],
        vec![
            "set",
            "proxy-group",
            "add",
            "n",
            "--type",
            "select",
            "--members",
            "reject",
        ],
    ] {
        let result = parse_args(accepted.iter().map(|s| (*s).to_owned()));
        assert!(
            result.is_ok(),
            "expected known member to parse: {accepted:?} got {result:?}"
        );
    }
}

// ── Dry-run semantics ──────────────────────────────────────────

fn expect_set_proxy_add(command: Command) -> (bool, bool) {
    match command {
        Command::Set(SetCmd::Proxy(SetProxyCmd::Add { apply, dry_run, .. })) => (apply, dry_run),
        other => {
            panic!("expected set proxy add, got {other:?}")
        }
    }
}

fn expect_set_proxy_remove(command: Command) -> (bool, bool) {
    match command {
        // W1-β: remove routes through the Entry seam (C-M).
        Command::Set(SetCmd::Entry(EntryWriteCmd::Remove { apply, dry_run, .. })) => {
            (apply, dry_run)
        }
        other => {
            panic!("expected entry remove, got {other:?}")
        }
    }
}

fn expect_set_proxy_import(command: Command) -> (bool, bool) {
    match command {
        Command::Set(SetCmd::Proxy(SetProxyCmd::Import { apply, dry_run, .. })) => (apply, dry_run),
        other => {
            panic!("expected set proxy import, got {other:?}")
        }
    }
}

fn expect_set_sub_add(command: Command) -> (bool, bool) {
    match command {
        Command::Set(SetCmd::Sub(SetSubCmd::Add { apply, dry_run, .. })) => (apply, dry_run),
        other => {
            panic!("expected set sub add, got {other:?}")
        }
    }
}

fn expect_set_sub_remove(command: Command) -> (bool, bool) {
    match command {
        Command::Set(SetCmd::Sub(SetSubCmd::Remove { apply, dry_run, .. })) => (apply, dry_run),
        other => {
            panic!("expected set sub remove, got {other:?}")
        }
    }
}

fn expect_set_profile_remove(command: Command) -> (bool, bool) {
    match command {
        Command::Set(SetCmd::Profile(SetProfileCmd::Remove { apply, dry_run, .. })) => {
            (apply, dry_run)
        }
        other => {
            panic!("expected set profile remove, got {other:?}")
        }
    }
}

#[test]
fn dry_run_defaults_to_false_on_writes() {
    // Every write command should default to `apply: false, dry_run: false`
    // per the Round 11 spec (dry-run is the SAFE default; the user must
    // explicitly opt in to writing by adding `--apply`).
    let (apply, dry_run) =
        expect_set_proxy_add(parse(&["set", "proxy", "add", "ss://abc"]).command);
    assert!(!apply);
    assert!(!dry_run);

    let (apply, dry_run) =
        expect_set_proxy_remove(parse(&["set", "proxy", "remove", "id1"]).command);
    assert!(!apply);
    assert!(!dry_run);

    let (apply, dry_run) =
        expect_set_proxy_import(parse(&["set", "proxy", "import", "/tmp/u"]).command);
    assert!(!apply);
    assert!(!dry_run);

    let (apply, dry_run) = expect_set_sub_add(parse(&["set", "sub", "add", "https://x"]).command);
    assert!(!apply);
    assert!(!dry_run);
}

#[test]
fn apply_flag_sets_apply_true() {
    let (apply, dry_run) =
        expect_set_proxy_add(parse(&["set", "proxy", "add", "ss://abc", "--apply"]).command);
    assert!(apply);
    assert!(!dry_run);

    let (apply, dry_run) =
        expect_set_sub_remove(parse(&["set", "sub", "remove", "https://x", "--apply"]).command);
    assert!(apply);
    assert!(!dry_run);

    let (apply, dry_run) =
        expect_set_profile_remove(parse(&["set", "profile", "remove", "team", "--apply"]).command);
    assert!(apply);
    assert!(!dry_run);
}

#[test]
fn dry_run_flag_is_still_accepted_explicitly() {
    // Round 10 had --dry-run as the opt-in. Round 11 makes it the default
    // via `--apply`, but the legacy `--dry-run` flag must still parse and
    // be honored for backwards compatibility.
    let (apply, dry_run) =
        expect_set_proxy_remove(parse(&["set", "proxy", "remove", "id1", "--dry-run"]).command);
    assert!(!apply);
    assert!(dry_run);

    let (apply, dry_run) =
        expect_set_sub_add(parse(&["set", "sub", "add", "https://x", "--dry-run"]).command);
    assert!(!apply);
    assert!(dry_run);
}

#[test]
fn apply_and_dry_run_must_not_both_be_true() {
    // Clap should reject the contradictory combination; a parse error
    // is the expected outcome.
    let err = parse_err(&["set", "proxy", "add", "ss://abc", "--apply", "--dry-run"]);
    let message = err.to_string();
    assert!(
        message.contains("cannot be used with") || message.contains("conflict"),
        "expected conflict error, got: {message}"
    );
}

// ── Retired spellings are rejected ─────────────────────────────

#[test]
fn old_top_level_spellings_are_rejected() {
    // W1-β: the earlier generations of this test asserted that
    // the pre-Round-11 spellings were gone. v3 inverts part of
    // that on purpose (bare `status` / `doctor` / `dns` ARE the
    // canonical v3 shapes now), so the contract becomes: the v1
    // namespace roots and mid-layers only parse through the
    // complete alias paths — never bare — and never-landed
    // spellings stay rejected.
    for removed in [
        vec!["show".to_owned()],
        vec!["set".to_owned()],
        vec!["show".to_owned(), "core".to_owned()],
        vec!["set".to_owned(), "core".to_owned()],
        vec!["set".to_owned(), "proxy".to_owned()],
        vec!["set".to_owned(), "proxy-group".to_owned()],
        // Q3/D3: `daemon run` was explicitly not adopted.
        vec!["daemon".to_owned(), "run".to_owned()],
        // v3 `core` owns only the kernel lifecycle.
        vec!["core".to_owned(), "nodes".to_owned()],
        // The v0 `sys` namespace stays retired.
        vec!["sys".to_owned(), "tun".to_owned(), "on".to_owned()],
        // Retired leaf names from earlier generations.
        vec!["sub".to_owned(), "check".to_owned(), "/tmp/x".to_owned()],
        vec!["config".to_owned(), "check".to_owned(), "/tmp/x".to_owned()],
        vec!["config".to_owned(), "vim".to_owned()],
        // `help` / `version` live under `tool` only.
        vec!["help".to_owned()],
        vec!["version".to_owned()],
        // Unknown leaves under a new domain (never a bare word).
        vec!["node".to_owned(), "bogus".to_owned()],
    ] {
        assert!(
            parse_args(removed.clone()).is_err(),
            "expected retired spelling to be rejected: {removed:?}"
        );
    }
}

#[test]
fn old_short_flag_aliases_are_rejected() {
    for removed in [
        vec!["-t".to_owned()],
        vec!["-d".to_owned()],
        vec!["-c".to_owned(), "start".to_owned()],
        vec!["-s".to_owned(), "refresh".to_owned()],
        vec!["-p".to_owned(), "proxy".to_owned(), "on".to_owned()],
    ] {
        assert!(
            parse_args(removed.clone()).is_err(),
            "expected removed short flag to be rejected: {removed:?}"
        );
    }
}

// ── Completions (top-level) ────────────────────────────────────

#[test]
fn completions_stays_as_a_top_level_subcommand() {
    assert_eq!(
        parse(&["completions", "zsh"]).command,
        Command::Completions(CompletionShell::Zsh)
    );
    assert_eq!(
        parse(&["completions", "bash"]).command,
        Command::Completions(CompletionShell::Bash)
    );
    assert_eq!(
        parse(&["completions", "fish"]).command,
        Command::Completions(CompletionShell::Fish)
    );
    assert_eq!(
        parse(&["completions", "elvish"]).command,
        Command::Completions(CompletionShell::Elvish)
    );
    assert_eq!(
        parse(&["completions", "powershell"]).command,
        Command::Completions(CompletionShell::PowerShell)
    );
}

// ── Help-text contract ─────────────────────────────────────────

#[test]
fn top_level_help_lists_all_four_namespaces() {
    // W1-β: the v3 resource-domain tree. The test name is
    // historical (Round 11); the contract now covers every v3
    // head so a missing domain fails loudly.
    let mut cmd = <grammar::ClapCli as clap::CommandFactory>::command();
    let mut buffer = Vec::new();
    cmd.write_long_help(&mut buffer)
        .unwrap_or_else(|error| panic!("help render failed: {error}"));
    let text =
        String::from_utf8(buffer).unwrap_or_else(|error| panic!("help text is not UTF-8: {error}"));
    for head in [
        "daemon",
        "stop",
        "reload",
        "restart",
        "status",
        "node",
        "sub",
        "profile",
        "config",
        "core",
        "rule-provider",
        "provider",
        "rules",
        "connections",
        "traffic",
        "mode",
        "sysproxy",
        "tun",
        "dns",
        "doctor",
        "tool",
        "completions",
    ] {
        assert!(text.contains(head), "top-level help must list `{head}`");
    }
    assert!(
        text.contains("Common workflows"),
        "after-help must point at the common workflows section"
    );
    assert!(
        text.contains("caly config apply"),
        "after-help must include an example write command"
    );
    assert!(
        text.contains("--apply"),
        "after-help must mention the --apply flag (dry-run policy)"
    );
}

#[test]
fn node_subcommand_help_lists_every_entry_verb() {
    // W1-β: the v3 node domain (nodes + groups, one resource).
    let mut cmd = <grammar::ClapCli as clap::CommandFactory>::command();
    let Some(node_cmd) = cmd.find_subcommand_mut("node") else {
        panic!("node subcommand must exist")
    };
    let mut buffer = Vec::new();
    node_cmd
        .write_long_help(&mut buffer)
        .unwrap_or_else(|error| panic!("help render failed: {error}"));
    let text =
        String::from_utf8(buffer).unwrap_or_else(|error| panic!("help text is not UTF-8: {error}"));
    for leaf in [
        "list", "groups", "show", "select", "ping", "test", "add", "edit", "remove", "enable",
        "disable", "import",
    ] {
        assert!(text.contains(leaf), "node help must list leaf `{leaf}`");
    }
}

#[test]
fn config_subcommand_help_lists_every_leaf() {
    // W1-β: the v3 config domain owns both the read leaves
    // (path/files/validate) and the write leaves (apply/…).
    let mut cmd = <grammar::ClapCli as clap::CommandFactory>::command();
    let Some(config_cmd) = cmd.find_subcommand_mut("config") else {
        panic!("config subcommand must exist")
    };
    let mut buffer = Vec::new();
    config_cmd
        .write_long_help(&mut buffer)
        .unwrap_or_else(|error| panic!("help render failed: {error}"));
    let text =
        String::from_utf8(buffer).unwrap_or_else(|error| panic!("help text is not UTF-8: {error}"));
    for leaf in [
        "show", "path", "files", "validate", "apply", "generate", "default", "diff", "edit",
    ] {
        assert!(text.contains(leaf), "config help must list leaf `{leaf}`");
    }
}

#[test]
fn tool_subcommand_help_lists_the_three_retained_leaves() {
    // W1-β: dns/doctor are top-level verbs in v3; `tool` keeps
    // help/version only.
    let mut cmd = <grammar::ClapCli as clap::CommandFactory>::command();
    let Some(tool_cmd) = cmd.find_subcommand_mut("tool") else {
        panic!("tool subcommand must exist")
    };
    let mut buffer = Vec::new();
    tool_cmd
        .write_long_help(&mut buffer)
        .unwrap_or_else(|error| panic!("help render failed: {error}"));
    let text =
        String::from_utf8(buffer).unwrap_or_else(|error| panic!("help text is not UTF-8: {error}"));
    for leaf in ["help", "version"] {
        assert!(text.contains(leaf), "tool help must list leaf `{leaf}`");
    }
}

// ── CliOptions default works (the original SIGABRT) ────────────

#[test]
fn cli_options_default_is_constructible() {
    // Regression: CliOptions lost its Default derive during the rewrite.
    // Every parse-level test that compares against `CliOptions::default()`
    // depends on this.
    let default = CliOptions::default();
    assert!(default.socket.is_none());
    assert!(!default.json);
    assert!(default.core.is_none());
    assert!(default.mihomo_bin.is_none());
    assert!(default.sing_box_bin.is_none());
}

// ── Round 13: `set` module split + `Refreshable` trait wiring ──

#[test]
fn set_dispatch_routes_to_every_resource_module() {
    // Round 13 regression: every `SetCmd` variant must be
    // dispatchable. The resource families (core / proxy /
    // tun / sub / profile / config / daemon / rule-provider /
    // proxy-group / provider) are the public dispatch surface; the
    // Round 12 monolithic `commands::set` was split into
    // them, and a typo'd `super::super::bridge` path would
    // otherwise only surface in `cargo build`.
    use crate::cli::{
        SetCmd, SetConfigCmd, SetCoreCmd, SetDaemonCmd, SetProfileCmd, SetProxyCmd,
        SetRuleProviderCmd, SetSubCmd,
    };
    use crate::output::CliOutput;

    fn empty_output() -> CliOutput {
        CliOutput::Human
    }
    fn empty_options() -> crate::cli::CliOptions {
        crate::cli::CliOptions::default()
    }

    let cases: Vec<(&str, SetCmd)> = vec![
        ("set core start", SetCmd::Core(SetCoreCmd::Start)),
        ("set proxy on", SetCmd::Proxy(SetProxyCmd::On)),
        ("set proxy off", SetCmd::Proxy(SetProxyCmd::Off)),
        ("set tun on", SetCmd::Tun(true)),
        ("set tun off", SetCmd::Tun(false)),
        (
            "set sub refresh",
            SetCmd::Sub(SetSubCmd::Refresh {
                target: None,
                force: false,
                asynchronous: false,
            }),
        ),
        (
            "set sub add https://x",
            SetCmd::Sub(SetSubCmd::Add {
                url: "https://x".to_owned(),
                name: None,
                refresh_every_minutes: None,
                apply: false,
                dry_run: false,
            }),
        ),
        (
            "set profile add x remote:y",
            SetCmd::Profile(SetProfileCmd::Add {
                id: "x".to_owned(),
                source: "remote:y".to_owned(),
                apply: false,
                dry_run: false,
            }),
        ),
        ("set config apply", SetCmd::Config(SetConfigCmd::Apply)),
        (
            "set config diff",
            SetCmd::Config(SetConfigCmd::Diff { file: None }),
        ),
        ("set daemon stop", SetCmd::Daemon(SetDaemonCmd::Stop)),
        (
            "set rule-provider list",
            SetCmd::RuleProvider(SetRuleProviderCmd::List),
        ),
        (
            "set rule-provider refresh",
            SetCmd::RuleProvider(SetRuleProviderCmd::Refresh { name: None }),
        ),
    ];
    for (label, cmd) in cases {
        // The dispatch must not panic on construction; the
        // exit code is `SUCCESS` for `planned_ok` stubs and
        // any failure code for the daemon-routed ones (no
        // daemon running in tests). The point is the path
        // through `set::run` — every variant must reach its
        // resource module.
        let _ = crate::commands::set::run(cmd, empty_options(), empty_output());
        // Suppress the unused `label` warning while keeping
        // it for human inspection if the test ever panics.
        let _ = label;
    }
}

#[test]
fn set_mod_exposes_six_sub_modules() {
    // Round 13: the `commands::set` module tree was
    // originally 8 resource sub-modules (one per
    // resource). Round 29: the `proxy_group` and
    // `rule_provider` sub-modules were removed —
    // they were 1-line passthroughs to
    // `commands::proxy_group::dispatch` /
    // `commands::rule_provider::dispatch`, and the
    // `commands::set::run` match now calls those
    // canonical dispatchers directly. The
    // `set/<resource>.rs` files that stayed are the
    // ones that own dispatch logic: the `core` /
    // `tun` / `daemon` / `config` / `profile` /
    // `proxy` / `sub` resources each have
    // resource-specific grammar mapping (the
    // `proxy` resource has `add` / `edit` /
    // `remove` / `import` / `on` / `off`; the
    // `profile` resource has `add` / `remove` /
    // `edit` / `export` / `enable` / `disable` /
    // `refresh`; the `sub` resource has the
    // bespoke `Import` path that drives the
    // `import_preview` CLI; the `common` module
    // is the shared `run_writer` / `run_standard_writer`
    // envelope). The two removed sub-modules
    // were pure indirection: the `set/proxy_group.rs`
    // and `set/rule_provider.rs` files contained
    // 1-line `dispatch` functions that just
    // delegated to the canonical
    // `commands::proxy_group::dispatch` /
    // `commands::rule_provider::dispatch`. The
    // `set::run` match now uses
    // `super::proxy_group::dispatch` /
    // `super::rule_provider::dispatch` directly,
    // saving two `.rs` files and two `mod
    // <resource>` declarations in `set/mod.rs`.
    // Round 33: `tun.rs` was inlined into
    // `set::run`'s `SetCmd::Tun` arm (the file
    // was a 9-line one-call-site shim). The
    // expected-file list dropped the entry.
    let set_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/set");
    for resource in &[
        "common.rs",
        "config.rs",
        "core.rs",
        "daemon.rs",
        "profile.rs",
        "proxy.rs",
        "sub.rs",
    ] {
        let path = set_dir.join(resource);
        assert!(
            path.is_file(),
            "commands/set/{resource} must exist (set/proxy_group.rs and set/rule_provider.rs were removed in Round 29 — pure-indirection wrappers)"
        );
    }
    // The three removed sub-modules must NOT exist
    // (the test catches a future re-introduction
    // of the indirection).
    for removed in &["proxy_group.rs", "rule_provider.rs", "tun.rs"] {
        let path = set_dir.join(removed);
        assert!(
            !path.is_file(),
            "commands/set/{removed} must NOT exist (Round 29: indirection wrappers removed for proxy_group/rule_provider; Round 33: tun.rs inlined into set::run's `SetCmd::Tun` arm)"
        );
    }
}

#[test]
fn refreshable_trait_is_wired_into_set_dispatch() {
    // Round 13: the `Refreshable` trait is the unified
    // shape for "declare a remote source + refresh it".
    // Round 28: the third impl (`SubscriptionRefresh`)
    // was removed because `set sub refresh` still drives
    // the daemon RPC directly through `run_client` (the
    // trait's `refresh` only forwards `CliOptions::default()`
    // to the daemon RPC, losing the operator's `--json`
    // / `--core` flags). The two live impls are wired
    // through the `set profile refresh` / `set
    // rule-provider refresh` dispatchers; a future Round
    // that threads the operator's options through the
    // trait will re-add the sub impl.
    use crate::commands::refresh::Refreshable;
    fn assert_impl<T: Refreshable>() {}
    assert_impl::<crate::commands::refresh::ProfileRefresh>();
    assert_impl::<crate::commands::refresh::RuleProviderRefresh>();
}

// ── v3 resource-domain tree (cli-v3-design.md, W1-β) ──────────

#[test]
fn bare_invocation_runs_the_daemon() {
    // Q3: a bare `caly` is the foreground daemon.
    assert_eq!(parse(&[]).command, Command::Daemon);
    assert_eq!(parse(&["daemon"]).command, Command::Daemon);
}

#[test]
fn top_level_lifecycle_and_status() {
    assert_eq!(
        parse(&["stop"]).command,
        Command::Set(SetCmd::Daemon(SetDaemonCmd::Stop))
    );
    assert_eq!(
        parse(&["reload"]).command,
        Command::Set(SetCmd::Daemon(SetDaemonCmd::Reload))
    );
    assert_eq!(
        parse(&["restart"]).command,
        Command::Set(SetCmd::Daemon(SetDaemonCmd::Restart))
    );
    assert_eq!(parse(&["status"]).command, Command::Show(ShowCmd::Status));
    // C-J: --verbose is the health snapshot (v1 `show core health`
    // and `set daemon status` both delegated here).
    assert_eq!(
        parse(&["status", "--verbose"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Health))
    );
}

#[test]
fn node_domain_picks_by_default() {
    // A bare `caly node` / `caly n` opens the interactive live picker
    // (node select without an argument); listing stays explicit.
    assert_eq!(
        parse(&["node"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Select {
            node: None,
            delay: false,
            poll: false,
        }))
    );
    assert_eq!(
        parse(&["node", "list"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Nodes))
    );
    // T-1 transitional leaf.
    assert_eq!(
        parse(&["node", "groups"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Groups))
    );
    // C-O W1 compromise: the only offline projection is the
    // declared `proxy_groups:` list.
    assert_eq!(
        parse(&["node", "list", "--offline"]).command,
        Command::Set(SetCmd::ProxyGroup(SetProxyGroupCmd::List {
            enabled_only: false
        }))
    );
    assert_eq!(
        parse(&["node", "list", "--offline", "--enabled"]).command,
        Command::Set(SetCmd::ProxyGroup(SetProxyGroupCmd::List {
            enabled_only: true
        }))
    );
    assert_eq!(
        parse(&["node", "show", "hk-01"]).command,
        Command::Show(ShowCmd::Proxy(ShowProxyCmd::Show {
            id: "hk-01".to_owned()
        }))
    );
}

#[test]
fn node_select_ping_test_parse() {
    assert_eq!(
        parse(&["node", "select", "hk-01"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Select {
            node: Some("hk-01".to_owned()),
            delay: false,
            poll: false,
        }))
    );
    assert_eq!(
        parse(&["node", "select", "--delay"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Select {
            node: None,
            delay: true,
            poll: false,
        }))
    );
    assert_eq!(
        parse(&["node", "ping", "hk-01", "--samples", "5"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Delay {
            name: Some("hk-01".to_owned()),
            all: false,
            url: None,
            samples: Some(5),
        }))
    );
    assert_eq!(
        parse(&["node", "test", "hk-01", "--url", "https://x/"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::UrlTest {
            name: "hk-01".to_owned(),
            url: Some("https://x/".to_owned()),
            samples: None,
            apply: false
        }))
    );
    // W4 group face: `--apply` commits the re-test.
    assert_eq!(
        parse(&["node", "test", "auto-test", "--apply"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::UrlTest {
            name: "auto-test".to_owned(),
            url: None,
            samples: None,
            apply: true
        }))
    );
    // W4 (`node pick`): dry-run by default; `--apply` commits.
    assert_eq!(
        parse(&["node", "pick", "selector-main", "DIRECT"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Pick {
            group: "selector-main".to_owned(),
            member: Some("DIRECT".to_owned()),
            apply: false,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["node", "pick", "selector-main", "--apply"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Pick {
            group: "selector-main".to_owned(),
            member: None,
            apply: true,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&["node", "pick", "selector-main", "DIRECT", "--dry-run"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Pick {
            group: "selector-main".to_owned(),
            member: Some("DIRECT".to_owned()),
            apply: false,
            dry_run: true,
        }))
    );
}

#[test]
fn node_add_dual_form_parses() {
    // URI form (protocol entry).
    assert_eq!(
        parse(&["node", "add", "vmess://x", "--group", "节点选择"]).command,
        Command::Set(SetCmd::Proxy(SetProxyCmd::Add {
            uri: "vmess://x".to_owned(),
            group: Some("节点选择".to_owned()),
            apply: false,
            dry_run: false,
        }))
    );
    // Group form, positional name slot (the v1 alias shape).
    assert_eq!(
        parse(&[
            "node",
            "add",
            "自动选择",
            "--type",
            "url-test",
            "--members",
            "node:hk,direct",
            "--url",
            "https://x/",
        ])
        .command,
        Command::Set(SetCmd::ProxyGroup(SetProxyGroupCmd::Add {
            name: "自动选择".to_owned(),
            group_type: ProxyGroupTypeSpec::UrlTest,
            members: Ok(vec![
                ProxyGroupMemberSpec::Node {
                    tag: "hk".to_owned()
                },
                ProxyGroupMemberSpec::Direct,
            ]),
            url: Some("https://x/".to_owned()),
            interval_seconds: None,
            tolerance_ms: None,
            apply: false,
            dry_run: false,
        }))
    );
    // Group form, `--name` slot + CLI spellings selector/urltest.
    assert_eq!(
        parse(&[
            "node",
            "add",
            "--type",
            "selector",
            "--name",
            "节点选择",
            "--members",
            "direct",
        ])
        .command,
        Command::Set(SetCmd::ProxyGroup(SetProxyGroupCmd::Add {
            name: "节点选择".to_owned(),
            group_type: ProxyGroupTypeSpec::Select,
            members: Ok(vec![ProxyGroupMemberSpec::Direct]),
            url: None,
            interval_seconds: None,
            tolerance_ms: None,
            apply: false,
            dry_run: false,
        }))
    );
    assert_eq!(
        parse(&[
            "node",
            "add",
            "--type",
            "urltest",
            "--name",
            "自动选择",
            "--members",
            "direct",
        ])
        .command,
        parse(&[
            "node",
            "add",
            "--type",
            "url-test",
            "--name",
            "自动选择",
            "--members",
            "direct",
        ])
        .command,
    );
    // The target slot is required either way.
    let err = parse_err(&["node", "add"]);
    assert!(err.to_string().contains("required"));
    // --group (join) conflicts with --type (group form).
    let _err = parse_err(&[
        "node",
        "add",
        "vmess://x",
        "--group",
        "g",
        "--type",
        "select",
    ]);
}

#[test]
fn mode_connections_rules_traffic_top_level() {
    assert_eq!(
        parse(&["mode"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Mode))
    );
    assert_eq!(
        parse(&["mode", "global"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Mode("global".to_owned())))
    );
    assert_eq!(
        parse(&["connections"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Connections))
    );
    assert_eq!(
        parse(&["connections", "close"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::CloseConnections))
    );
    assert_eq!(
        parse(&["traffic"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Traffic))
    );
    assert_eq!(
        parse(&["rules", "--match", "example.com"]).command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Rules {
            r#match: Some("example.com".to_owned())
        }))
    );
}

#[test]
fn sysproxy_tun_doctor_dns_top_level() {
    assert_eq!(
        parse(&["sysproxy", "on"]).command,
        Command::Set(SetCmd::Proxy(SetProxyCmd::On))
    );
    assert_eq!(
        parse(&["sysproxy", "off"]).command,
        Command::Set(SetCmd::Proxy(SetProxyCmd::Off))
    );
    // W-PAC (2026-08-12): `sysproxy pac [url]`.
    assert_eq!(
        parse(&["sysproxy", "pac"]).command,
        Command::Set(SetCmd::Proxy(SetProxyCmd::Pac { url: None }))
    );
    assert_eq!(
        parse(&["sysproxy", "pac", "file:///tmp/x.pac"]).command,
        Command::Set(SetCmd::Proxy(SetProxyCmd::Pac {
            url: Some("file:///tmp/x.pac".to_owned())
        }))
    );
    assert_eq!(
        parse(&["tun", "on"]).command,
        Command::Set(SetCmd::Tun(true))
    );
    assert_eq!(
        parse(&["doctor"]).command,
        Command::Tool(ToolCmd::Doctor { fix: false })
    );
    assert_eq!(
        parse(&["doctor", "--fix"]).command,
        Command::Tool(ToolCmd::Doctor { fix: true })
    );
    assert_eq!(
        parse(&["dns", "example.com"]).command,
        Command::Tool(ToolCmd::Dns(Some("example.com".to_owned())))
    );
}

#[test]
fn core_domain_keeps_kernel_lifecycle() {
    assert_eq!(
        parse(&["core", "start"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Start))
    );
    assert_eq!(
        parse(&["core", "stop"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Stop))
    );
    assert_eq!(
        parse(&["core", "restart"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Restart))
    );
    assert_eq!(
        parse(&["core", "switch", "sing-box"]).command,
        Command::Set(SetCmd::Core(SetCoreCmd::Switch("sing-box".to_owned())))
    );
}

#[test]
fn profile_use_and_bare_domains() {
    assert_eq!(
        parse(&["profile", "use", "home"]).command,
        Command::Set(SetCmd::Profile(SetProfileCmd::Use {
            id: "home".to_owned()
        }))
    );
    assert_eq!(
        parse(&["profile"]).command,
        Command::Show(ShowCmd::Profile(ShowProfileCmd::List))
    );
    assert_eq!(
        parse(&["sub"]).command,
        Command::Show(ShowCmd::Sub(ShowSubCmd::Providers))
    );
    assert_eq!(
        parse(&["config"]).command,
        Command::Show(ShowCmd::Config(ShowConfigCmd::Files))
    );
    assert_eq!(
        parse(&["config", "show"]).command,
        Command::Show(ShowCmd::Config(ShowConfigCmd::Files))
    );
}

#[test]
fn deprecated_paths_parse_identically_to_canonical() {
    // Every v1 path parses through the expansion layer to the
    // exact Invocation the canonical v3 path produces.
    let pairs: [(&[&str], &[&str]); 12] = [
        (&["show", "core", "nodes"], &["node", "list"]),
        (&["show", "core", "groups"], &["node", "groups"]),
        (
            &["show", "sub", "parse", "f.yaml"],
            &["sub", "parse", "f.yaml"],
        ),
        (&["show", "sub", "import"], &["sub", "import"]),
        (&["show", "config", "validate"], &["config", "validate"]),
        (&["set", "core", "select", "hk"], &["node", "select", "hk"]),
        (
            &["set", "core", "delay", "--all"],
            &["node", "ping", "--all"],
        ),
        (&["set", "proxy", "on"], &["sysproxy", "on"]),
        (&["set", "sub", "refresh"], &["sub", "refresh"]),
        (
            &["set", "proxy-group", "list"],
            &["node", "list", "--offline"],
        ),
        (
            &["set", "rule-provider", "list"],
            &["rule-provider", "list"],
        ),
        (&["tool", "doctor", "--fix"], &["doctor", "--fix"]),
    ];
    for (old, new) in pairs {
        assert_eq!(
            parse(old),
            parse(new),
            "deprecated {old:?} != canonical {new:?}",
        );
    }
}

#[test]
fn builtin_shortcuts_expand_to_canonical_paths() {
    // W3a: `t` entered the table with `--format=tree` (§9.1 终值 10 条).
    let pairs: [(&[&str], &[&str]); 10] = [
        (&["st"], &["status"]),
        // Bare `n` opens the live picker (`node` with no subcommand).
        (&["n"], &["node"]),
        (&["n", "s", "hk"], &["node", "select", "hk"]),
        (&["n", "p", "hk"], &["node", "ping", "hk"]),
        (&["n", "t", "hk"], &["node", "test", "hk"]),
        (&["t"], &["node", "list", "--format=tree"]),
        (&["s"], &["sub", "list"]),
        (&["s", "r"], &["sub", "refresh"]),
        (&["d"], &["doctor"]),
        (&["c"], &["config", "diff"]),
    ];
    for (shortcut, canonical) in pairs {
        assert_eq!(
            parse(shortcut),
            parse(canonical),
            "shortcut {shortcut:?} != canonical {canonical:?}",
        );
    }
}

#[test]
fn expansion_preserves_leading_global_flags() {
    let invocation = parse(&["--json", "show", "core", "nodes"]);
    assert!(invocation.options.json);
    assert_eq!(
        invocation.command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Nodes))
    );
}

#[test]
fn format_flag_pins_the_list_output_mode() {
    // §5.1: both spellings (`--format=tsv`, `--format table`)
    // parse; the flag rides through the alias layer untouched.
    let invocation = parse(&["node", "list", "--format=tsv"]);
    assert_eq!(invocation.options.format, Some(OutputFormat::Tsv));
    let invocation = parse(&["--format", "table", "node", "list"]);
    assert_eq!(invocation.options.format, Some(OutputFormat::Table));
    let invocation = parse(&["n", "--format=tsv"]);
    assert_eq!(invocation.options.format, Some(OutputFormat::Tsv));
    // The alias engine must skip the flag's value token (W2 adds
    // `--format` to VALUE_FLAGS): `table` here is the value, not
    // a (nonexistent) command head.
    let invocation = parse(&["--format", "tsv", "show", "proxy", "list"]);
    assert_eq!(invocation.options.format, Some(OutputFormat::Tsv));
    assert_eq!(
        invocation.command,
        Command::Show(ShowCmd::Core(ShowCoreCmd::Nodes))
    );
}

#[test]
fn c_l_prime_status_leaves_parse() {
    // C-L′: `sysproxy status` / `tun status` are offline
    // projections; `on`/`off` keep their v1 behaviour.
    let invocation = parse(&["sysproxy", "status"]);
    assert_eq!(invocation.command, Command::Show(ShowCmd::SysproxyStatus));
    let invocation = parse(&["tun", "status"]);
    assert_eq!(invocation.command, Command::Show(ShowCmd::TunStatus));
    let invocation = parse(&["sysproxy", "on"]);
    assert_eq!(
        invocation.command,
        Command::Set(SetCmd::Proxy(SetProxyCmd::On))
    );
    let invocation = parse(&["set", "proxy", "off"]);
    assert_eq!(
        invocation.command,
        Command::Set(SetCmd::Proxy(SetProxyCmd::Off))
    );
    let invocation = parse(&["tun", "off"]);
    assert_eq!(invocation.command, Command::Set(SetCmd::Tun(false)));
}

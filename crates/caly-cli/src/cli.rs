//! Public command model decoupled from clap derive types.

use std::path::{Path, PathBuf};

use clap::{CommandFactory, Parser};

use caly_profile::loader::{ConfigFileError, load_config_file};

mod aliases;
mod grammar;
mod user_aliases;

pub use grammar::Editor;
use grammar::{ClapCli, CompletionShell};

// Round 16: the legacy enums (`CoreCmd` / `SubCmd` /
// `SysCmd` / `ConfigCmd` / `ProfileCmd`) used to live
// here as the internal input shape of
// `commands::bridge::old_*`. Round 16 moved them to
// `client::legacy` (private client RPC types). The public
// surface of this module is now only the 5-namespace
// grammar (`Tool` / `Show` / `Set` / `Completions` /
// `Daemon`).

// ── Public enums (one variant per leaf the runtime cares about) ──

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolCmd {
    Help(Option<String>),
    Version,
    Doctor { fix: bool },
    Dns(Option<String>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShowCmd {
    Status,
    Core(ShowCoreCmd),
    Sub(ShowSubCmd),
    Profile(ShowProfileCmd),
    Proxy(ShowProxyCmd),
    Config(ShowConfigCmd),
    /// W2/C-L′: offline projection of the declared system-proxy
    /// intent plus the durable recovery record's presence.
    SysproxyStatus,
    /// W2/C-L′: same for TUN.
    TunStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShowCoreCmd {
    Nodes,
    Groups,
    Connections,
    Flow { watch: bool },
    FlowTrace { id: String, watch: bool },
    Traffic,
    Mode,
    Rules { r#match: Option<String> },
    Health,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShowSubCmd {
    Providers,
    Parse {
        path: PathBuf,
        userinfo: Option<String>,
        apply: bool,
        name: Option<String>,
    },
    // W1-β: the v1 preview-only `Import` variant is gone — v3
    // `sub import` folds preview into the dry-run default of
    // `SetSubCmd::Import` (cli-v3-design.md §4.3).
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShowProfileCmd {
    List,
    Show { id: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShowProxyCmd {
    // W1-β: `List` / `Groups` folded into the node domain
    // (`node list` / `node groups` — transitional T-1); only the
    // online single-entry detail remains here.
    Show { id: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShowConfigCmd {
    Path,
    Files,
    Validate { file: Option<PathBuf> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetCmd {
    Core(SetCoreCmd),
    Proxy(SetProxyCmd),
    Tun(bool),
    Sub(SetSubCmd),
    Profile(SetProfileCmd),
    Config(SetConfigCmd),
    Daemon(SetDaemonCmd),
    /// Round 12: declarative `rule_providers:` CRUD + refresh,
    /// the third "remote source" resource alongside `sub` and
    /// `profile`. See `commands::rule_provider::dispatch`.
    RuleProvider(SetRuleProviderCmd),
    /// Round 20: declarative `proxy_groups:` CRUD. The
    /// schema has a `proxy_groups:` segment with five
    /// `type:` kinds (`select` / `url-test` / `fallback` /
    /// `load-balance` / `relay`); this command family
    /// round-trips that list through `config.yaml`. See
    /// `commands::proxy_group::dispatch`.
    ProxyGroup(SetProxyGroupCmd),
    /// Audit #61: declarative CRUD for `providers:` (the
    /// named proxy-content containers). The schema and the
    /// resolved view existed but had no CLI management
    /// plane — `sub import` is preview-only and
    /// `InlineNodes` providers needed hand-written YAML.
    /// W1: `node remove|enable|disable` seam (C-M). See
    /// [`EntryWriteCmd`].
    Entry(EntryWriteCmd),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetCoreCmd {
    Start,
    Stop,
    Restart,
    Switch(String),
    Select {
        node: Option<String>,
        delay: bool,
        poll: bool,
    },
    Mode(String),
    CloseConnections,
    Delay {
        name: Option<String>,
        all: bool,
        url: Option<String>,
        /// Per-URL sample count (1..=5); `None`
        /// means "use the default". Surfaced in
        /// the offline `Query::UrlTest` path.
        samples: Option<u32>,
    },
    UrlTest {
        name: String,
        url: Option<String>,
        /// Per-URL sample count (1..=5); `None`
        /// means "use the default". Forwarded
        /// through the typed client command so
        /// the offline path applies it.
        samples: Option<u32>,
        /// W4 group face: `--apply` commits the group re-test
        /// (default is a dry-run preview). Ignored for the
        /// single-entry face.
        apply: bool,
    },
    /// W4 (`node pick`): pick a member inside a selector group
    /// (cli-v3-design.md §4.2 C-B). Dry-run by default; `--apply`
    /// commits through the daemon.
    Pick {
        group: String,
        member: Option<String>,
        apply: bool,
        dry_run: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetProxyCmd {
    /// `sysproxy pac [url]`: desktop proxy auto mode with a PAC URL.
    Pac {
        url: Option<String>,
    },
    Add {
        uri: String,
        group: Option<String>,
        apply: bool,
        dry_run: bool,
    },
    Edit {
        id: String,
        apply: bool,
        dry_run: bool,
    },
    Remove {
        id: String,
        apply: bool,
        dry_run: bool,
    },
    Import {
        path: PathBuf,
        apply: bool,
        dry_run: bool,
    },
    On,
    Off,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetSubCmd {
    /// W2-β2b (§4.3): `sub refresh [name-or-url] [--force] [--async]`.
    /// No target = every enabled source (the daemon's all-zero id
    /// batch path).
    Refresh {
        target: Option<String>,
        force: bool,
        asynchronous: bool,
    },
    Add {
        url: String,
        name: Option<String>,
        /// W2-β2 (Q5): per-source refresh period in minutes, already
        /// scaled from the `--every <hours>` flag. `None` = flag not
        /// given; the command layer then applies the 24h URL default
        /// or the static file pin.
        refresh_every_minutes: Option<u64>,
        apply: bool,
        dry_run: bool,
    },
    Remove {
        url: String,
        /// W2-β2b (`--purge`): also delete the daemon-owned cached
        /// body (`$state/subscriptions/<hex id>`). Default keeps it.
        purge: bool,
        apply: bool,
        dry_run: bool,
    },
    /// W2-β2b (`caly sub set`): edit one declared source — at least
    /// one of the three change flags is required (clap ArgGroup).
    Set {
        target: String,
        url: Option<String>,
        name: Option<String>,
        refresh_every_minutes: Option<u64>,
        apply: bool,
        dry_run: bool,
    },
    Enable {
        url: String,
        apply: bool,
        dry_run: bool,
    },
    Disable {
        url: String,
        apply: bool,
        dry_run: bool,
    },
    Import {
        path: Option<PathBuf>,
        clipboard: bool,
        apply: bool,
        dry_run: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetProfileCmd {
    /// W1 (cli-v3-design.md §10): switch the current working
    /// context. Writes `~/.local/state/caly/context.json`
    /// immediately (no dry-run — the context record IS the
    /// state, like the Round-12 refresh exception).
    Use {
        id: String,
    },
    Add {
        id: String,
        source: String,
        apply: bool,
        dry_run: bool,
    },
    Remove {
        id: String,
        apply: bool,
        dry_run: bool,
    },
    Edit {
        id: String,
        apply: bool,
        dry_run: bool,
    },
    /// Round 12: refresh always writes the body (the body IS the
    /// cache). A `NotModified` outcome (HTTP 304) is a real
    /// outcome, not a dry-run. The `--apply` / `--dry-run` flags
    /// were removed from `set profile refresh` because the
    /// distinction was meaningless.
    Refresh {
        id: Option<String>,
    },
    Export {
        id: String,
        out: PathBuf,
    },
    Enable {
        id: String,
        apply: bool,
        dry_run: bool,
    },
    Disable {
        id: String,
        apply: bool,
        dry_run: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetConfigCmd {
    Apply,
    Generate,
    Default,
    Diff { file: Option<PathBuf> },
    Edit(Editor),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetDaemonCmd {
    Stop,
    Reload,
    Restart,
    // W1-β: `Status` retired — v3 top-level `caly status` is the
    // single status surface (C-J); the v1 leaf delegated to the
    // same handler, so the alias is output-identical.
}

/// Round 12: declarative CRUD for `rule_providers:`. Each
/// variant is a separate grammar leaf so the help text can name
/// the resource being acted on. `--apply` / `--dry-run` policy
/// is the same as the other `set` resources (default dry-run,
/// explicit `--apply` writes, mutually exclusive).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetRuleProviderCmd {
    /// Add a new provider to `rule_providers:`. Source kind is
    /// one of `http <url>`, `file <path>`, or `inline --payload <body>`.
    Add {
        name: String,
        source: RuleProviderSourceSpec,
        apply: bool,
        dry_run: bool,
    },
    /// Remove a provider from `rule_providers:`.
    Remove {
        name: String,
        apply: bool,
        dry_run: bool,
    },
    /// Enable a disabled provider.
    Enable {
        name: String,
        apply: bool,
        dry_run: bool,
    },
    /// Disable a provider (the daemon boot skips it).
    Disable {
        name: String,
        apply: bool,
        dry_run: bool,
    },
    /// Refresh the provider's materialised body under
    /// `<workdir>/rule-providers/<name>.yaml`. Always writes
    /// (the materialised file IS the body the kernel reads);
    /// `--apply` / `--dry-run` are not exposed here for the
    /// same reason as `set profile refresh`.
    Refresh { name: Option<String> },
    /// List all declared rule providers (and their enable
    /// state). Pure read; the JSON output is the source of
    /// truth for scripts.
    List,
}

/// Round 12: public source-kind spec for the `set rule-provider
/// add` leaf. Mirrors the `type: http | file | inline` shape
/// of `caly_profile::schema::RuleProviderSourceConfig` but
/// erases the bounded text (the CLI does not enforce size
/// diagnostics at the grammar level).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuleProviderSourceSpec {
    Http { url: String, interval_ms: u64 },
    File { path: PathBuf },
    Inline { payload: String },
}

/// Round 20: declarative CRUD for `proxy_groups:`. Each
/// variant is a separate grammar leaf so the help text can
/// name the resource being acted on. `--apply` / `--dry-run`
/// policy is the same as the other `set` resources
/// (default dry-run, explicit `--apply` writes, mutually
/// exclusive).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SetProxyGroupCmd {
    /// Add a new group to `proxy_groups:`. The `type:`
    /// field is one of `select` / `url-test` / `fallback` /
    /// `load-balance` / `relay`. The `members:` list is
    /// supplied as a sequence of `kind:…` shapes; a probe
    /// `url:` is required for `url-test` / `fallback` /
    /// `load-balance`.
    Add {
        name: String,
        group_type: ProxyGroupTypeSpec,
        /// Member specs, or the re-parse failure clap's
        /// `value_parser` should already have rejected.
        /// Carrying the failure (instead of degrading
        /// to an empty list) means an internal parser
        /// drift surfaces as a command error rather
        /// than a silently empty group (#55).
        members: Result<Vec<ProxyGroupMemberSpec>, String>,
        /// Probe URL; required for probe-driven groups,
        /// ignored for `select` / `relay`. The schema
        /// validator rejects a probe-driven group without
        /// one and a non-probe group with one.
        url: Option<String>,
        /// Probe cadence in seconds; defaults to 300.
        interval_seconds: Option<u32>,
        /// Probe tolerance in milliseconds; defaults to 50.
        tolerance_ms: Option<u32>,
        apply: bool,
        dry_run: bool,
    },
    /// Remove a group from `proxy_groups:`.
    Remove {
        name: String,
        apply: bool,
        dry_run: bool,
    },
    /// Enable a disabled group.
    Enable {
        name: String,
        apply: bool,
        dry_run: bool,
    },
    /// Disable a group (the daemon boot skips it).
    Disable {
        name: String,
        apply: bool,
        dry_run: bool,
    },
    /// List all declared proxy groups (and their enable
    /// state). Pure read; the JSON output is the source of
    /// truth for scripts. `enabled_only` filters
    /// the in-memory projection (the layered config
    /// already carries every declared group, so
    /// no server round-trip is needed for the
    /// filter).
    List { enabled_only: bool },
}

/// Public type-kind spec for the `set proxy-group add` leaf.
/// Mirrors the kebab-case `type:` field the rendered Mihomo
/// config uses; the dispatch converts to the schema enum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProxyGroupTypeSpec {
    Select,
    UrlTest,
    Fallback,
    LoadBalance,
    Relay,
}

/// Public member spec for the `set proxy-group add` leaf.
/// Mirrors the `kind: node|group|direct|reject` shape the
/// schema uses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProxyGroupMemberSpec {
    Node { tag: String },
    Group { name: String },
    Direct,
    Reject,
}

/// W1 (cli-v3-design.md C-M, seam S-W1-1): the v3 `node` domain
/// merges nodes and groups, so `node remove|enable|disable` can
/// target either kind. The grammar cannot know which — the
/// dispatch (`commands::node_dispatch`) reads the layered config
/// and forwards to `SetProxyCmd` or `SetProxyGroupCmd`. Entry
/// names share one closed set (domain-level uniqueness), so the
/// split is always unambiguous; a missing name exits 1 with the
/// available candidates in the message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EntryWriteCmd {
    Remove {
        id: String,
        apply: bool,
        dry_run: bool,
    },
    Enable {
        id: String,
        apply: bool,
        dry_run: bool,
    },
    Disable {
        id: String,
        apply: bool,
        dry_run: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HistoryCmd {
    List { limit: Option<usize> },
    Replay { target: String },
    Clear,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Daemon,
    // New (Round 11)
    Tool(ToolCmd),
    Show(ShowCmd),
    Set(SetCmd),
    History(HistoryCmd),
    Completions(CompletionShell),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CliOptions {
    pub socket: Option<PathBuf>,
    pub json: bool,
    pub core: Option<String>,
    pub mihomo_bin: Option<PathBuf>,
    pub sing_box_bin: Option<PathBuf>,
    pub format: Option<OutputFormat>,
}

/// W2 (cli-v3-design.md §5.1): user-pinned list output mode.
/// Unpinned means adaptive — TTY table, piped TSV (Q6). W3a adds
/// `tree` (offline entry tree, §5.3) and `diff` (config diff's
/// terraform-plan view, §5.5 — T-W3a).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputFormat {
    Table,
    Tsv,
    Tree,
    Diff,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Invocation {
    pub command: Command,
    pub options: CliOptions,
}

// ── Parsing ──

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Invocation, clap::Error> {
    // W1: pre-parse alias expansion. The built-in tables are empty
    // in W1-α, so this is the identity; W1-β fills the v1→v3 map
    // and swaps the grammar in the same batch (cli/aliases.rs).
    // W3a (cli-v3-design.md §9.2): the user alias table from
    // `<config>/aliases.yaml` joins the expansion (missing file =
    // empty table; a malformed file warns and degrades to empty).
    // Deprecation notices go to stderr so scripted stdout
    // consumers are never polluted.
    let user_rules = user_aliases::load_from(
        &caly_platform::paths::AppPaths::from_env()
            .config
            .join("aliases.yaml"),
    );
    let expansion = aliases::expand_with_user(args.into_iter().collect(), &user_rules);
    for notice in &expansion.notices {
        eprintln!("{notice}");
    }
    // §9.2: an expansion whose head is not a built-in command is a
    // usage error (exit 2) with the R1 back-link; clap would reject
    // the unknown head anyway, this message names the rule. The head
    // is located the same way the expansion layer finds it (global
    // value flags consume their value tokens).
    let head_index = aliases::command_head(&expansion.argv);
    if let Some(head) = expansion.argv.get(head_index)
        && !aliases::is_builtin_head(head)
    {
        let mut message = format!(
            "unrecognized subcommand `{head}`\n\n  tip: an alias must expand to a built-in caly command"
        );
        if expansion.typed != expansion.argv {
            let _ = std::fmt::Write::write_fmt(
                &mut message,
                format_args!(
                    "\nnote: you typed: caly {} → caly {}",
                    expansion.typed.join(" "),
                    expansion.argv.join(" "),
                ),
            );
        }
        return Err(clap::Error::raw(
            clap::error::ErrorKind::InvalidSubcommand,
            message,
        ));
    }
    ClapCli::try_parse_from(
        std::iter::once("caly").chain(expansion.argv.iter().map(String::as_str)),
    )
    .inspect_err(|_error| {
        // R1: when the pre-parse layer rewrote the spelling,
        // back-link the follow-on clap error to what the
        // operator actually typed so the mismatch is
        // attributable at a glance.
        if expansion.typed != expansion.argv {
            eprintln!(
                "note: you typed: caly {} → caly {}",
                expansion.typed.join(" "),
                expansion.argv.join(" "),
            );
        }
    })
    .map(Into::into)
}

mod convert;

pub fn check_config_file(path: &Path) -> Result<(), ConfigFileError> {
    load_config_file(path).map(|_| ())
}

pub fn generate_completions(shell: CompletionShell) {
    let shell = match shell {
        CompletionShell::Bash => clap_complete::Shell::Bash,
        CompletionShell::Elvish => clap_complete::Shell::Elvish,
        CompletionShell::Fish => clap_complete::Shell::Fish,
        CompletionShell::PowerShell => clap_complete::Shell::PowerShell,
        CompletionShell::Zsh => clap_complete::Shell::Zsh,
    };
    let mut command = ClapCli::command();
    clap_complete::generate(shell, &mut command, "caly", &mut std::io::stdout());
}

#[cfg(test)]
mod cli_tests;

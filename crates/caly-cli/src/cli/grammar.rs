//! Declarative clap grammar: v3 single-domain tree (cli-v3-design.md §4).
//!
//! W1-β replaces the v1 5-namespace tree (`daemon/tool/show/set/
//! completions`) with the resource-domain tree: every entry (protocol
//! node AND proxy group) lives under `node` (EntryKind at the command
//! surface — D17); `sub` / `profile` / `config` are their own domains;
//! diagnostics are top-level verbs (`doctor` / `dns`, no `diag`
//! namespace — D2). A bare `caly` runs the daemon foreground (Q3).
//!
//! v1 paths keep working through the pre-parse alias expansion layer
//! (`cli/aliases.rs`): this tree is the single source of truth and the
//! expansion table rewrites every deprecated path into it.
//!
//! Routing-only period: every leaf below maps 1:1 onto the public
//! command model that already backs the v1 handlers. Leaves whose
//! semantics are new (group-level `pick`/group `test`, sub `--every` /
//! `sub set`, `sysproxy|tun status`, `--offline` unified views, tree
//! format) are intentionally absent; they arrive with their owning
//! phase (W2/W3a/W4 — cli-v3-design.md §12).

use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};

#[derive(Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Elvish,
    Fish,
    #[value(name = "powershell")]
    PowerShell,
    Zsh,
}

#[derive(Parser, Debug)]
#[command(
    name = "caly",
    version,
    about = "Daemon-first proxy core manager (Mihomo + sing-box)",
    long_about = "caly manages Mihomo and sing-box through one daemon, with a shared\n\
                 config tree, a profile system, and a typed control plane.\n\
                 Resource domains: `node` (protocol entries AND proxy groups),\n\
                 `sub`, `profile`, `config`, plus top-level verbs `doctor`,\n\
                 `dns`, `rules`, `connections`, `traffic`, `mode`, `sysproxy`,\n\
                 `tun`. A bare `caly` runs the daemon in the foreground.\n\
                 Use `caly tool help <command>` for per-domain help.",
    after_help = "Common workflows:\n\
                  \n\
                   caly                        # run the daemon (foreground)\n\
                   caly status                 # daemon state summary\n\
                   caly node                   # pick an entry (live picker)\n\
                   caly node select <id>       # select the active entry\n\
                   caly sub refresh            # fetch subscriptions\n\
                   caly profile add <id> remote:<url>  # declare a profile\n\
                   caly config apply           # push config to daemon\n\
                   caly doctor                 # offline diagnostics\n\
                  \n\
                  Config write leaves default to `--dry-run`; pass `--apply`\n\
                  to actually write. v1 paths (show/set/...) still work via\n\
                  the deprecation alias layer for one version window. Use\n\
                  `caly completions <shell>` for completion scripts."
)]
#[command(disable_help_subcommand = true)]
pub(super) struct ClapCli {
    #[command(flatten)]
    pub(super) global: GlobalOpts,
    #[command(subcommand)]
    pub(super) command: Option<ClapCommand>,
}

#[derive(Args, Debug, Clone)]
pub(super) struct GlobalOpts {
    #[arg(long, global = true, value_name = "PATH")]
    pub(super) socket: Option<PathBuf>,
    #[arg(long, global = true)]
    pub(super) json: bool,
    #[arg(long, global = true, value_enum, value_name = "CORE")]
    pub(super) core: Option<CoreTarget>,
    #[arg(long, global = true, value_name = "PATH")]
    pub(super) mihomo_bin: Option<PathBuf>,
    #[arg(long, global = true, value_name = "PATH")]
    pub(super) sing_box_bin: Option<PathBuf>,
    /// Pin the list output mode: `table` forces the aligned table even
    /// when piped, `tsv` forces tab-separated output even on a TTY.
    /// Unpinned: TTY → table, piped → TSV.
    #[arg(long, global = true, value_enum, value_name = "MODE")]
    pub(super) format: Option<FormatFlag>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub(super) enum FormatFlag {
    Table,
    Tsv,
    /// Offline entry tree; `diff` is the terraform-plan view for
    /// `config diff`.
    Tree,
    Diff,
}

#[derive(Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(super) enum CoreTarget {
    Mihomo,
    #[value(name = "sing-box")]
    SingBox,
}

impl CoreTarget {
    pub(super) const fn as_str(&self) -> &'static str {
        match self {
            Self::Mihomo => "mihomo",
            Self::SingBox => "sing-box",
        }
    }
}

#[derive(Subcommand, Debug)]
pub(super) enum ClapCommand {
    /// Run the daemon (foreground). Same as a bare `caly`.
    Daemon,
    /// Stop the daemon gracefully.
    Stop,
    /// Reload config without restart.
    Reload,
    /// Restart the daemon.
    Restart,
    /// Show the daemon status summary (--verbose for the full snapshot).
    Status {
        #[arg(long)]
        verbose: bool,
    },
    /// Entries: protocol nodes and proxy groups (one domain).
    Node {
        #[command(subcommand)]
        command: Option<ClapNodeCmd>,
    },
    /// List and manage subscriptions.
    Sub {
        #[command(subcommand)]
        command: Option<ClapSubCmd>,
    },
    /// List and manage profiles.
    Profile {
        #[command(subcommand)]
        command: Option<ClapProfileCmd>,
    },
    /// Manage layered configuration.
    Config {
        #[command(subcommand)]
        command: Option<ClapConfigCmd>,
    },
    /// Manage the running kernel core (not the daemon).
    Core {
        #[command(subcommand)]
        command: ClapCoreCmd,
    },
    /// Manage rule providers (declarative `rule_providers:` CRUD + refresh).
    #[command(name = "rule-provider")]
    RuleProvider {
        #[command(subcommand)]
        command: ClapSetRuleProviderCmd,
    },
    /// List rules; with --match, evaluate a target.
    Rules {
        #[arg(long)]
        r#match: Option<String>,
    },
    /// List active connections; `close` drops them all.
    Connections {
        #[command(subcommand)]
        command: Option<ClapConnectionsCmd>,
    },
    /// Live traffic processing flow: every connection with the rule and
    /// outbound chain it is routed through.
    Flow {
        #[command(subcommand)]
        command: Option<ClapFlowCmd>,
        /// Redraw every second (TTY clears between frames).
        #[arg(long)]
        watch: bool,
    },
    /// Show traffic totals.
    Traffic,
    /// Recall and replay previously executed operations.
    History {
        #[command(subcommand)]
        command: Option<ClapHistoryCmd>,
    },
    /// Get the routing mode, or set it with an argument.
    Mode { mode: Option<RoutingMode> },
    /// Control the desktop system proxy (on/off) or show the offline
    /// declared-state projection.
    Sysproxy {
        #[command(subcommand)]
        action: ClapSysproxyCmd,
    },
    /// TUN on/off, or the offline declared-state projection.
    Tun {
        #[command(subcommand)]
        action: ClapTunCmd,
    },
    /// Resolve a domain through the configured nameservers.
    Dns { domain: Option<String> },
    /// Offline diagnostics.
    Doctor {
        #[arg(long)]
        fix: bool,
    },
    /// Run one-shot tools and diagnostics.
    Tool {
        #[command(subcommand)]
        command: ClapToolCmd,
    },
    /// Generate shell completion scripts.
    #[command(arg_required_else_help = true)]
    Completions { shell: CompletionShell },
}

// ── node ────────────────────────────────────────────────────────
#[derive(Subcommand, Debug)]
#[command(disable_help_subcommand = true)]
pub(super) enum ClapNodeCmd {
    /// List entries (online snapshot; --offline reads the declared
    /// groups).
    List {
        #[arg(long)]
        offline: bool,
        /// Only meaningful with --offline: skip disabled groups.
        /// (`--enabled-only` is the v1 spelling, kept as an alias
        /// so the deprecated `set proxy-group list --enabled-only`
        /// path keeps parsing.)
        #[arg(long, alias = "enabled-only")]
        enabled: bool,
    },
    /// List proxy groups (folds into `node list` once the corectl
    /// enrichment lands).
    Groups,
    /// Show one entry (online snapshot detail).
    Show { id: String },
    /// Select the active entry (id, name, or --delay for fastest).
    Select {
        node: Option<String>,
        #[arg(long)]
        delay: bool,
        /// Live polling: refresh latencies/status from the daemon while
        /// the picker is open (node select -p).
        #[arg(short = 'p', long)]
        poll: bool,
    },
    /// Latency-sweep one entry, or every entry with --all.
    Ping {
        name: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        url: Option<String>,
        /// Per-entry sample count (1..=5). Default 3.
        /// Larger counts reduce jitter at the cost
        /// of `ping --all` wall-clock time.
        #[arg(long, value_name = "N")]
        samples: Option<u32>,
    },
    /// URL-test a single entry, or the members of a urltest/fallback
    /// group (group-level re-test).
    Test {
        name: String,
        #[arg(long)]
        url: Option<String>,
        /// Per-URL sample count (1..=5). Default 3.
        #[arg(long, value_name = "N")]
        samples: Option<u32>,
        /// W4 group face: commit the re-test (default is a dry-run
        /// preview listing the group's members). No effect on the
        /// single-entry face.
        #[arg(long)]
        apply: bool,
    },
    /// Pick a member inside a selector group. Dry-run by default;
    /// `--apply` commits the selection through the daemon. Omitting the
    /// member on a live terminal opens the interactive member picker.
    Pick {
        /// The selector group name (offline declared tree).
        group: String,
        /// The member to select; omitted on a TTY opens a picker.
        member: Option<String>,
        /// Commit the selection (default is a dry-run preview).
        #[arg(long)]
        apply: bool,
        /// Explicit dry-run preview (default behaviour).
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Add an entry: a protocol URI, or a group with --type.
    ///
    /// Dual shape (cli-v3-design.md C-P): the positional is the
    /// node URI by default and the group name when --type is
    /// given (so the v1 `set proxy-group add <name> --type …`
    /// path rewrites onto one leaf); `--name` is the flag form
    /// of the same slot.
    #[command(group(
        ArgGroup::new("target").args(["value", "name"]).multiple(false).required(true)
    ))]
    Add {
        /// Node URI (`vmess://…`); with --type, the group name.
        value: Option<String>,
        /// Join this group (node URI form only).
        #[arg(long, value_name = "GROUP", conflicts_with = "type")]
        group: Option<String>,
        /// Group kind; turns the leaf into the group form.
        #[arg(long, value_enum, value_name = "KIND", requires = "target")]
        r#type: Option<ProxyGroupKind>,
        /// Group name (flag form of the positional name slot).
        #[arg(long, value_name = "NAME", requires = "type")]
        name: Option<String>,
        /// Group members: `node:<tag>`, `group:<name>`, `direct`, `reject`.
        #[arg(
            long,
            value_delimiter = ',',
            value_parser = parse_member_token,
            requires = "type"
        )]
        members: Vec<String>,
        /// Probe URL (url-test / fallback / load-balance groups).
        #[arg(long, requires = "type")]
        url: Option<String>,
        /// Probe cadence in seconds; defaults to 300.
        #[arg(long, requires = "type")]
        interval_seconds: Option<u32>,
        /// Probe tolerance in milliseconds; defaults to 50.
        #[arg(long, requires = "type")]
        tolerance_ms: Option<u32>,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Edit an inline node in your editor. The `id` is the offline
    /// declared name (`node list --offline`), not the online 32-hex id.
    Edit {
        id: String,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Remove an entry (node or group; dispatch resolves the kind,
    /// see cli-v3-design.md C-M seam S-W1-1). The `id` is the offline
    /// declared name (`node list --offline`), not the online 32-hex id.
    Remove {
        id: String,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Enable a group (nodes carry no enable state).
    Enable {
        id: String,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Disable a group (nodes carry no enable state).
    Disable {
        id: String,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Import a URI list file.
    Import {
        path: PathBuf,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
}

// ── sub ─────────────────────────────────────────────────────────
#[derive(Subcommand, Debug)]
#[command(disable_help_subcommand = true)]
pub(super) enum ClapSubCmd {
    /// List configured subscription sources.
    List,
    /// Parse a subscription file (offline preview by default).
    /// With `--apply`, register the file as a subscription source
    /// (same effect as `sub add <path> --apply`).
    Parse {
        path: PathBuf,
        #[arg(long)]
        userinfo: Option<String>,
        /// Register the parsed file as a subscription source.
        #[arg(long)]
        apply: bool,
        /// Source name used with `--apply` (defaults to the file name).
        #[arg(long)]
        name: Option<String>,
    },
    /// Add a subscription source: an HTTP(S) URL or a local file
    /// (auto-detected). W2-β2 lands the three-segment v3 shape
    /// `<url-or-file> [--name <n>] [--every <hours>]` (C-G/Q5).
    ///
    /// `--every 0` pins the source as static (never refreshed);
    /// omitting it defaults to 24h for URLs. File sources are
    /// always static — a non-zero `--every` with a file is a
    /// usage error.
    Add {
        url: String,
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        #[arg(long, value_name = "HOURS", value_parser = clap::value_parser!(u64).range(0..=1_000_000))]
        every: Option<u64>,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Refresh configured subscription sources (W2-β2b, §4.3):
    /// no target = every enabled source; `<name-or-url>` pins one;
    /// `--force` ignores cached validators; `--async` submits and
    /// returns without waiting for completion.
    Refresh {
        target: Option<String>,
        #[arg(long)]
        force: bool,
        #[arg(long = "async")]
        asynchronous: bool,
    },
    /// Remove a subscription source (name or URL, W2-β2b).
    /// `--purge` also deletes the cached body; the default keeps it.
    Remove {
        url: String,
        #[arg(long)]
        purge: bool,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Edit one declared subscription source (W2-β2b, §4.3):
    /// at least one of `--url` / `--name` / `--every` is required.
    #[command(group(
        clap::ArgGroup::new("change")
            .args(["url", "name", "every"])
            .required(true)
            .multiple(true)
    ))]
    Set {
        /// The source to edit: its display name, or its URL.
        target: String,
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        #[arg(long, value_name = "HOURS", value_parser = clap::value_parser!(u64).range(0..=1_000_000))]
        every: Option<u64>,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Enable a subscription source.
    Enable {
        url: String,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Disable a subscription source.
    Disable {
        url: String,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Import a URI list / clipboard; default dry-run previews,
    /// --apply saves inline.
    Import {
        path: Option<PathBuf>,
        #[arg(long)]
        clipboard: bool,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
}

// ── profile ─────────────────────────────────────────────────────
#[derive(Subcommand, Debug)]
#[command(disable_help_subcommand = true)]
pub(super) enum ClapProfileCmd {
    /// List declared profiles.
    List,
    /// Show one profile.
    Show { id: String },
    /// Switch the current working context to a profile (cli-v3-design
    /// §10; writes ~/.local/state/caly/context.json).
    Use { id: String },
    /// Declare a new profile.
    Add {
        id: String,
        source: String,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Remove a profile.
    Remove {
        id: String,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Edit a profile body in your editor.
    Edit {
        id: String,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Refresh a profile body (always writes; the body IS the cache).
    Refresh { id: Option<String> },
    /// Export a profile body to a file.
    Export {
        id: String,
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
    /// Enable a profile.
    Enable {
        id: String,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Disable a profile.
    Disable {
        id: String,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
}

// ── config ──────────────────────────────────────────────────────
#[derive(Subcommand, Debug)]
#[command(disable_help_subcommand = true)]
pub(super) enum ClapConfigCmd {
    /// Show the base + fragments overview (also the bare `config`).
    Show,
    /// Print the active config dir.
    Path,
    /// List base + fragments.
    Files,
    /// Validate merged config or a single file.
    Validate {
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Apply current config to the daemon.
    Apply,
    /// Generate a documented default config.
    Generate,
    /// Reset to a default config (backup first).
    Default,
    /// Diff current config against default.
    Diff {
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Open config dir in editor.
    Edit { editor: Editor },
}

// ── core (kernel lifecycle) ─────────────────────────────────────
#[derive(Subcommand, Debug)]
#[command(disable_help_subcommand = true)]
pub(super) enum ClapCoreCmd {
    /// Start the active core.
    Start,
    /// Stop the active core.
    Stop,
    /// Restart the active core.
    Restart,
    /// Switch active core (mihomo ↔ sing-box).
    Switch {
        #[arg(value_enum)]
        core: CoreTarget,
    },
}

// ── connections ─────────────────────────────────────────────────
#[derive(Subcommand, Debug)]
pub(super) enum ClapConnectionsCmd {
    /// Close all active connections.
    Close,
}

/// `history` subcommands (operation memory).
#[derive(Subcommand, Debug, Clone)]
pub(super) enum ClapHistoryCmd {
    /// List the recorded operations (newest last).
    List {
        /// Show at most N entries (default 20).
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Replay a recorded operation (`latest` or the list index).
    Replay { target: String },
    /// Clear all recorded operations.
    Clear,
}

/// `flow` subcommands (traffic processing flow inspection).
#[derive(Subcommand, Debug)]
pub(super) enum ClapFlowCmd {
    /// Render one connection's full processing chain (inbound → sniff →
    /// dns → rule → group → node), matched by connection id or host:port.
    Trace {
        /// Connection id or `host:port` selector.
        id: String,
    },
}

// ── tool ────────────────────────────────────────────────────────
#[derive(Subcommand, Debug)]
#[command(disable_help_subcommand = true)]
pub(super) enum ClapToolCmd {
    /// Print help text.
    Help { command: Option<String> },
    /// Print version.
    Version,
}

mod rule_providers;

pub(in crate::cli) use rule_providers::ClapSetRuleProviderCmd;

#[derive(Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(super) enum RoutingMode {
    Rule,
    Global,
    Direct,
}

impl RoutingMode {
    pub(super) const fn as_str(&self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Global => "global",
            Self::Direct => "direct",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Editor {
    Vim,
    Nvim,
}

/// W2/C-L′: `caly sysproxy (on|off|status)`; W-PAC (2026-08-12):
/// `caly sysproxy pac [url]` switches the desktop to auto mode with a
/// PAC file.
#[derive(Subcommand, Debug)]
pub(super) enum ClapSysproxyCmd {
    /// Enable the desktop system proxy.
    On,
    /// Disable it (the daemon restores the captured settings).
    Off,
    /// Switch to PAC mode: without a URL, caly writes a generated PAC
    /// (local subnets direct, everything else via the local proxy) into
    /// the state directory and points the desktop at it; with a URL
    /// (`http(s)://` or `file://`) that URL is used verbatim.
    Pac {
        /// PAC URL; omit to use the caly-generated file.
        url: Option<String>,
    },
    /// Offline projection: declared intent + recovery record (C-L′).
    Status,
}

/// W2/C-L′: `caly tun (on|off|status)`.
#[derive(Subcommand, Debug)]
pub(super) enum ClapTunCmd {
    /// Enable the TUN device.
    On,
    /// Disable it.
    Off,
    /// Offline projection: declared intent + recovery record (C-L′).
    Status,
}

/// Round 20: the group `--type` discriminator. Renamed to
/// `kebab-case` in the rendered config, but clap's `ValueEnum`
/// derives `kebab-case` from the variant name; the `rename_all =
/// "kebab-case"` attribute on the schema enum is the matching
/// consumer. W1-β: the CLI-facing spellings `selector` / `urltest`
/// are accepted as aliases (cli-v3-design.md G-§3.1 vocabulary);
/// the rendered output still uses the file forms.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub(super) enum ProxyGroupKind {
    #[value(alias = "selector")]
    Select,
    #[value(alias = "urltest")]
    UrlTest,
    Fallback,
    LoadBalance,
    Relay,
}

impl ProxyGroupKind {
    /// Maps to the public [`crate::cli::ProxyGroupTypeSpec`]
    /// shape the dispatch and the writer use.
    pub(super) const fn to_public(self) -> crate::cli::ProxyGroupTypeSpec {
        match self {
            Self::Select => crate::cli::ProxyGroupTypeSpec::Select,
            Self::UrlTest => crate::cli::ProxyGroupTypeSpec::UrlTest,
            Self::Fallback => crate::cli::ProxyGroupTypeSpec::Fallback,
            Self::LoadBalance => crate::cli::ProxyGroupTypeSpec::LoadBalance,
            Self::Relay => crate::cli::ProxyGroupTypeSpec::Relay,
        }
    }
}

fn parse_member_token(token: &str) -> Result<String, String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("empty member spec".to_owned());
    }
    if let Some((kind, _value)) = token.split_once(':') {
        match kind {
            "node" | "group" => Ok(token.to_owned()),
            other => Err(format!(
                "unknown member kind `{other}` (use `node:<tag>`, `group:<name>`, `direct`, or `reject`)"
            )),
        }
    } else {
        match token {
            "direct" | "reject" => Ok(token.to_owned()),
            other => Err(format!(
                "unknown member `{other}` (use `node:<tag>`, `group:<name>`, `direct`, or `reject`)"
            )),
        }
    }
}

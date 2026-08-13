//! clap grammar → public command-model conversions.
//!
//! Split out of `cli.rs` (audit #70 file-length budget): one
//! mechanical `From<Clap*> for <public enum>` impl per command
//! enum, plus the `kind:value` member-token parser used by the
//! proxy-group grammar. The public enums themselves live in the
//! parent `cli` module.
//!
//! W1-β (cli-v3-design.md): the v3 resource-domain grammar maps
//! onto the SAME public command model the v1 handlers already
//! consume — this is the mechanical-routing seam. Notable
//! mappings:
//!
//! - bare `caly` → `Command::Daemon` (Q3);
//! - `status` / `status --verbose` → `Show(Status)` /
//!   `Show(Core(Health))` (C-J);
//! - `node list` → `Show(Core(Nodes))`; the transitional
//!   `node groups` leaf → `Show(Core(Groups))` (T-1);
//! - `node list --offline` → the offline `proxy_groups:`
//!   projection (W1 compromise; the unified offline entry tree
//!   arrives in W3a — C-K/C-O);
//! - `node add` dual form (C-P): URI positional →
//!   `Set(Proxy(Add))`; `--type` (+ positional/`--name` name
//!   slot) → `Set(ProxyGroup(Add))`;
//! - `node remove|enable|disable` → `Set(Entry(…))`, the C-M
//!   seam resolved by `commands::node_dispatch`;
//! - `mode` / `connections` / `rules` / `traffic` are get/set
//!   or read/write pairs split by optional argument/subcommand.

use super::grammar::{
    ClapCli, ClapCommand, ClapConfigCmd, ClapConnectionsCmd, ClapCoreCmd, ClapFlowCmd,
    ClapHistoryCmd, ClapNodeCmd, ClapProfileCmd, ClapSetRuleProviderCmd, ClapSubCmd,
    ClapSysproxyCmd, ClapToolCmd, ClapTunCmd, Editor as ClapEditor, FormatFlag,
};
use super::{
    CliOptions, Command, EntryWriteCmd, HistoryCmd, Invocation, OutputFormat, ProxyGroupMemberSpec,
    SetCmd, SetConfigCmd, SetCoreCmd, SetDaemonCmd, SetProfileCmd, SetProxyCmd, SetProxyGroupCmd,
    SetRuleProviderCmd, SetSubCmd, ShowCmd, ShowConfigCmd, ShowCoreCmd, ShowProfileCmd,
    ShowProxyCmd, ShowSubCmd, ToolCmd,
};
use super::{Editor, RuleProviderSourceSpec};

impl From<FormatFlag> for OutputFormat {
    fn from(flag: FormatFlag) -> Self {
        match flag {
            FormatFlag::Table => Self::Table,
            FormatFlag::Tsv => Self::Tsv,
            FormatFlag::Tree => Self::Tree,
            FormatFlag::Diff => Self::Diff,
        }
    }
}

impl From<ClapCli> for Invocation {
    fn from(cli: ClapCli) -> Self {
        let options = CliOptions {
            socket: cli.global.socket,
            json: cli.global.json,
            core: cli.global.core.map(|v| v.as_str().to_owned()),
            mihomo_bin: cli.global.mihomo_bin,
            sing_box_bin: cli.global.sing_box_bin,
            format: cli.global.format.map(OutputFormat::from),
        };
        let command = match cli.command {
            // Q3: a bare `caly` runs the daemon foreground.
            None => Command::Daemon,
            Some(ClapCommand::Daemon) => Command::Daemon,
            Some(ClapCommand::Stop) => Command::Set(SetCmd::Daemon(SetDaemonCmd::Stop)),
            Some(ClapCommand::Reload) => Command::Set(SetCmd::Daemon(SetDaemonCmd::Reload)),
            Some(ClapCommand::Restart) => Command::Set(SetCmd::Daemon(SetDaemonCmd::Restart)),
            Some(ClapCommand::Status { verbose }) => {
                if verbose {
                    Command::Show(ShowCmd::Core(ShowCoreCmd::Health))
                } else {
                    Command::Show(ShowCmd::Status)
                }
            }
            Some(ClapCommand::Node { command }) => node_command(command),
            Some(ClapCommand::Sub { command }) => sub_command(command),
            Some(ClapCommand::Profile { command }) => profile_command(command),
            Some(ClapCommand::Config { command }) => config_command(command),
            Some(ClapCommand::Core { command }) => Command::Set(SetCmd::Core(command.into())),
            Some(ClapCommand::RuleProvider { command }) => {
                Command::Set(SetCmd::RuleProvider(command.into()))
            }
            Some(ClapCommand::Rules { r#match }) => {
                Command::Show(ShowCmd::Core(ShowCoreCmd::Rules { r#match }))
            }
            Some(ClapCommand::Connections { command }) => match command {
                None => Command::Show(ShowCmd::Core(ShowCoreCmd::Connections)),
                Some(ClapConnectionsCmd::Close) => {
                    Command::Set(SetCmd::Core(SetCoreCmd::CloseConnections))
                }
            },
            Some(ClapCommand::Flow { watch, command }) => match command {
                None => Command::Show(ShowCmd::Core(ShowCoreCmd::Flow { watch })),
                Some(ClapFlowCmd::Trace { id }) => {
                    Command::Show(ShowCmd::Core(ShowCoreCmd::FlowTrace { id, watch }))
                }
            },
            Some(ClapCommand::Traffic) => Command::Show(ShowCmd::Core(ShowCoreCmd::Traffic)),
            Some(ClapCommand::Mode { mode }) => match mode {
                None => Command::Show(ShowCmd::Core(ShowCoreCmd::Mode)),
                Some(mode) => {
                    Command::Set(SetCmd::Core(SetCoreCmd::Mode(mode.as_str().to_owned())))
                }
            },
            Some(ClapCommand::Sysproxy { action }) => match action {
                ClapSysproxyCmd::On => Command::Set(SetCmd::Proxy(SetProxyCmd::On)),
                ClapSysproxyCmd::Off => Command::Set(SetCmd::Proxy(SetProxyCmd::Off)),
                ClapSysproxyCmd::Pac { url } => {
                    Command::Set(SetCmd::Proxy(SetProxyCmd::Pac { url }))
                }
                ClapSysproxyCmd::Status => Command::Show(ShowCmd::SysproxyStatus),
            },
            Some(ClapCommand::Tun { action }) => match action {
                ClapTunCmd::On => Command::Set(SetCmd::Tun(true)),
                ClapTunCmd::Off => Command::Set(SetCmd::Tun(false)),
                ClapTunCmd::Status => Command::Show(ShowCmd::TunStatus),
            },
            Some(ClapCommand::Dns { domain }) => Command::Tool(ToolCmd::Dns(domain)),
            Some(ClapCommand::Doctor { fix }) => Command::Tool(ToolCmd::Doctor { fix }),
            Some(ClapCommand::Tool { command }) => Command::Tool(command.into()),
            Some(ClapCommand::History { command }) => match command {
                None => Command::History(HistoryCmd::List { limit: None }),
                Some(ClapHistoryCmd::List { limit }) => {
                    Command::History(HistoryCmd::List { limit })
                }
                Some(ClapHistoryCmd::Replay { target }) => {
                    Command::History(HistoryCmd::Replay { target })
                }
                Some(ClapHistoryCmd::Clear) => Command::History(HistoryCmd::Clear),
            },
            Some(ClapCommand::Completions { shell }) => Command::Completions(shell),
        };
        Self { command, options }
    }
}

// ── node domain ─────────────────────────────────────────────────

fn node_command(command: Option<ClapNodeCmd>) -> Command {
    let Some(command) = command else {
        // A bare `caly node` / `caly n` opens the interactive live
        // picker (node select without an argument); on a non-TTY it
        // degrades to the same usage error as `node select`.
        return Command::Set(SetCmd::Core(SetCoreCmd::Select {
            node: None,
            delay: false,
            poll: false,
        }));
    };
    match command {
        ClapNodeCmd::List { offline, enabled } => {
            if offline {
                // W1 compromise (C-O): the only existing offline
                // projection is the declared `proxy_groups:` list.
                // The unified offline entry tree lands in W3a.
                Command::Set(SetCmd::ProxyGroup(SetProxyGroupCmd::List {
                    enabled_only: enabled,
                }))
            } else {
                Command::Show(ShowCmd::Core(ShowCoreCmd::Nodes))
            }
        }
        // T-1 transitional leaf (W3b folds groups into `node list`).
        ClapNodeCmd::Groups => Command::Show(ShowCmd::Core(ShowCoreCmd::Groups)),
        ClapNodeCmd::Show { id } => Command::Show(ShowCmd::Proxy(ShowProxyCmd::Show { id })),
        ClapNodeCmd::Select { node, delay, poll } => {
            Command::Set(SetCmd::Core(SetCoreCmd::Select { node, delay, poll }))
        }
        ClapNodeCmd::Ping {
            name,
            all,
            url,
            samples,
        } => Command::Set(SetCmd::Core(SetCoreCmd::Delay {
            name,
            all,
            url,
            samples,
        })),
        ClapNodeCmd::Test {
            name,
            url,
            samples,
            apply,
        } => Command::Set(SetCmd::Core(SetCoreCmd::UrlTest {
            name,
            url,
            samples,
            apply,
        })),
        // W4 (`node pick`): the group face is a daemon online write;
        // the member slot stays optional for the interactive picker.
        ClapNodeCmd::Pick {
            group,
            member,
            apply,
            dry_run,
        } => Command::Set(SetCmd::Core(SetCoreCmd::Pick {
            group,
            member,
            apply,
            dry_run,
        })),
        ClapNodeCmd::Add {
            value,
            group,
            r#type,
            name,
            members,
            url,
            interval_seconds,
            tolerance_ms,
            apply,
            dry_run,
        } => node_add(
            value,
            group,
            r#type,
            name,
            members,
            url,
            interval_seconds,
            tolerance_ms,
            apply,
            dry_run,
        ),
        ClapNodeCmd::Edit { id, apply, dry_run } => {
            Command::Set(SetCmd::Proxy(SetProxyCmd::Edit { id, apply, dry_run }))
        }
        // C-M seam S-W1-1: node vs group is resolved at dispatch.
        ClapNodeCmd::Remove { id, apply, dry_run } => {
            Command::Set(SetCmd::Entry(EntryWriteCmd::Remove { id, apply, dry_run }))
        }
        ClapNodeCmd::Enable { id, apply, dry_run } => {
            Command::Set(SetCmd::Entry(EntryWriteCmd::Enable { id, apply, dry_run }))
        }
        ClapNodeCmd::Disable { id, apply, dry_run } => {
            Command::Set(SetCmd::Entry(EntryWriteCmd::Disable { id, apply, dry_run }))
        }
        ClapNodeCmd::Import {
            path,
            apply,
            dry_run,
        } => Command::Set(SetCmd::Proxy(SetProxyCmd::Import {
            path,
            apply,
            dry_run,
        })),
    }
}

/// Dual-form `node add` (C-P). With `--type` the positional/name
/// slot is the group name and the probe flags apply; without it
/// the slot must be the node URI. The ArgGroup `target` makes the
/// slot clap-required either way (`--name` via `requires = "type"`
/// keeps the flag form legal).
#[allow(clippy::too_many_arguments)]
fn node_add(
    value: Option<String>,
    group: Option<String>,
    r#type: Option<super::grammar::ProxyGroupKind>,
    name: Option<String>,
    members: Vec<String>,
    url: Option<String>,
    interval_seconds: Option<u32>,
    tolerance_ms: Option<u32>,
    apply: bool,
    dry_run: bool,
) -> Command {
    if let Some(kind) = r#type {
        // The ArgGroup guarantees one of the two slots; an empty
        // fallback here is unreachable in practice, and the schema
        // validator rejects an empty group name before any write.
        let group_name = name.or(value).unwrap_or_default();
        let parsed_members = members
            .into_iter()
            .map(parse_member_spec)
            .collect::<Result<Vec<_>, String>>()
            .map_err(|error| {
                format!(
                    "proxy-group member parse failed after clap accepted \
                     the token: {error} (clap's value_parser should have \
                     rejected it at parse time; please report this bug)"
                )
            });
        Command::Set(SetCmd::ProxyGroup(SetProxyGroupCmd::Add {
            name: group_name,
            group_type: kind.to_public(),
            members: parsed_members,
            url,
            interval_seconds,
            tolerance_ms,
            apply,
            dry_run,
        }))
    } else {
        let node_uri = value.or(name).unwrap_or_default();
        Command::Set(SetCmd::Proxy(SetProxyCmd::Add {
            uri: node_uri,
            group,
            apply,
            dry_run,
        }))
    }
}

// ── sub domain ──────────────────────────────────────────────────

fn sub_command(command: Option<ClapSubCmd>) -> Command {
    let Some(command) = command else {
        return Command::Show(ShowCmd::Sub(ShowSubCmd::Providers));
    };
    match command {
        ClapSubCmd::List => Command::Show(ShowCmd::Sub(ShowSubCmd::Providers)),
        ClapSubCmd::Parse {
            path,
            userinfo,
            apply,
            name,
        } => Command::Show(ShowCmd::Sub(ShowSubCmd::Parse {
            path,
            userinfo,
            apply,
            name,
        })),
        ClapSubCmd::Add {
            url,
            name,
            every,
            apply,
            dry_run,
        } => Command::Set(SetCmd::Sub(SetSubCmd::Add {
            url,
            name,
            // Hours → minutes; the clap range cap (≤1_000_000h) keeps
            // this multiply far from u64 overflow.
            refresh_every_minutes: every.map(|hours| hours.saturating_mul(60)),
            apply,
            dry_run,
        })),
        ClapSubCmd::Refresh {
            target,
            force,
            asynchronous,
        } => Command::Set(SetCmd::Sub(SetSubCmd::Refresh {
            target,
            force,
            asynchronous,
        })),
        ClapSubCmd::Remove {
            url,
            purge,
            apply,
            dry_run,
        } => Command::Set(SetCmd::Sub(SetSubCmd::Remove {
            url,
            purge,
            apply,
            dry_run,
        })),
        ClapSubCmd::Set {
            target,
            url,
            name,
            every,
            apply,
            dry_run,
        } => Command::Set(SetCmd::Sub(SetSubCmd::Set {
            target,
            url,
            name,
            // Hours → minutes, same scale rule as `sub add` (Q5).
            refresh_every_minutes: every.map(|hours| hours.saturating_mul(60)),
            apply,
            dry_run,
        })),
        ClapSubCmd::Enable {
            url,
            apply,
            dry_run,
        } => Command::Set(SetCmd::Sub(SetSubCmd::Enable {
            url,
            apply,
            dry_run,
        })),
        ClapSubCmd::Disable {
            url,
            apply,
            dry_run,
        } => Command::Set(SetCmd::Sub(SetSubCmd::Disable {
            url,
            apply,
            dry_run,
        })),
        ClapSubCmd::Import {
            path,
            clipboard,
            apply,
            dry_run,
        } => Command::Set(SetCmd::Sub(SetSubCmd::Import {
            path,
            clipboard,
            apply,
            dry_run,
        })),
    }
}

// ── profile domain ──────────────────────────────────────────────

fn profile_command(command: Option<ClapProfileCmd>) -> Command {
    let Some(command) = command else {
        return Command::Show(ShowCmd::Profile(ShowProfileCmd::List));
    };
    match command {
        ClapProfileCmd::List => Command::Show(ShowCmd::Profile(ShowProfileCmd::List)),
        ClapProfileCmd::Show { id } => Command::Show(ShowCmd::Profile(ShowProfileCmd::Show { id })),
        ClapProfileCmd::Use { id } => Command::Set(SetCmd::Profile(SetProfileCmd::Use { id })),
        ClapProfileCmd::Add {
            id,
            source,
            apply,
            dry_run,
        } => Command::Set(SetCmd::Profile(SetProfileCmd::Add {
            id,
            source,
            apply,
            dry_run,
        })),
        ClapProfileCmd::Remove { id, apply, dry_run } => {
            Command::Set(SetCmd::Profile(SetProfileCmd::Remove {
                id,
                apply,
                dry_run,
            }))
        }
        ClapProfileCmd::Edit { id, apply, dry_run } => {
            Command::Set(SetCmd::Profile(SetProfileCmd::Edit { id, apply, dry_run }))
        }
        ClapProfileCmd::Refresh { id } => {
            Command::Set(SetCmd::Profile(SetProfileCmd::Refresh { id }))
        }
        ClapProfileCmd::Export { id, out } => {
            Command::Set(SetCmd::Profile(SetProfileCmd::Export { id, out }))
        }
        ClapProfileCmd::Enable { id, apply, dry_run } => {
            Command::Set(SetCmd::Profile(SetProfileCmd::Enable {
                id,
                apply,
                dry_run,
            }))
        }
        ClapProfileCmd::Disable { id, apply, dry_run } => {
            Command::Set(SetCmd::Profile(SetProfileCmd::Disable {
                id,
                apply,
                dry_run,
            }))
        }
    }
}

// ── config domain ───────────────────────────────────────────────

fn config_command(command: Option<ClapConfigCmd>) -> Command {
    let Some(command) = command else {
        // Bare `caly config` = the files overview (C-I extension).
        return Command::Show(ShowCmd::Config(ShowConfigCmd::Files));
    };
    match command {
        ClapConfigCmd::Show | ClapConfigCmd::Files => {
            Command::Show(ShowCmd::Config(ShowConfigCmd::Files))
        }
        ClapConfigCmd::Path => Command::Show(ShowCmd::Config(ShowConfigCmd::Path)),
        ClapConfigCmd::Validate { file } => {
            Command::Show(ShowCmd::Config(ShowConfigCmd::Validate { file }))
        }
        ClapConfigCmd::Apply => Command::Set(SetCmd::Config(SetConfigCmd::Apply)),
        ClapConfigCmd::Generate => Command::Set(SetCmd::Config(SetConfigCmd::Generate)),
        ClapConfigCmd::Default => Command::Set(SetCmd::Config(SetConfigCmd::Default)),
        ClapConfigCmd::Diff { file } => Command::Set(SetCmd::Config(SetConfigCmd::Diff { file })),
        ClapConfigCmd::Edit { editor } => {
            Command::Set(SetCmd::Config(SetConfigCmd::Edit(match editor {
                ClapEditor::Vim => Editor::Vim,
                ClapEditor::Nvim => Editor::Nvim,
            })))
        }
    }
}

// ── remaining From impls ────────────────────────────────────────

impl From<ClapToolCmd> for ToolCmd {
    fn from(c: ClapToolCmd) -> Self {
        match c {
            ClapToolCmd::Help { command } => Self::Help(command),
            ClapToolCmd::Version => Self::Version,
        }
    }
}

impl From<ClapCoreCmd> for SetCoreCmd {
    fn from(c: ClapCoreCmd) -> Self {
        match c {
            ClapCoreCmd::Start => Self::Start,
            ClapCoreCmd::Stop => Self::Stop,
            ClapCoreCmd::Restart => Self::Restart,
            ClapCoreCmd::Switch { core } => Self::Switch(core.as_str().to_owned()),
        }
    }
}

impl From<ClapSetRuleProviderCmd> for SetRuleProviderCmd {
    fn from(c: ClapSetRuleProviderCmd) -> Self {
        match c {
            ClapSetRuleProviderCmd::AddHttp {
                name,
                url,
                interval_ms,
                apply,
                dry_run,
            } => Self::Add {
                name,
                source: RuleProviderSourceSpec::Http { url, interval_ms },
                apply,
                dry_run,
            },
            ClapSetRuleProviderCmd::AddFile {
                name,
                path,
                apply,
                dry_run,
            } => Self::Add {
                name,
                source: RuleProviderSourceSpec::File { path },
                apply,
                dry_run,
            },
            ClapSetRuleProviderCmd::AddInline {
                name,
                payload,
                apply,
                dry_run,
            } => Self::Add {
                name,
                source: RuleProviderSourceSpec::Inline { payload },
                apply,
                dry_run,
            },
            ClapSetRuleProviderCmd::Remove {
                name,
                apply,
                dry_run,
            } => Self::Remove {
                name,
                apply,
                dry_run,
            },
            ClapSetRuleProviderCmd::Enable {
                name,
                apply,
                dry_run,
            } => Self::Enable {
                name,
                apply,
                dry_run,
            },
            ClapSetRuleProviderCmd::Disable {
                name,
                apply,
                dry_run,
            } => Self::Disable {
                name,
                apply,
                dry_run,
            },
            ClapSetRuleProviderCmd::Refresh { name } => Self::Refresh { name },
            ClapSetRuleProviderCmd::List => Self::List,
        }
    }
}

/// Parses one `kind:value` member token into a
/// [`ProxyGroupMemberSpec`]. The grammar shape is
/// `node:<tag>`, `group:<name>`, `direct`, `reject`. An
/// unrecognised `kind:` is silently dropped at the
/// grammar level; the schema validator is the strict
/// path so a typo is caught before the kernel starts.
fn parse_member_spec(token: String) -> Result<ProxyGroupMemberSpec, String> {
    let token = token.trim();
    if token.is_empty() {
        return Err("empty member spec".to_owned());
    }
    if let Some((kind, value)) = token.split_once(':') {
        match kind {
            "node" => Ok(ProxyGroupMemberSpec::Node {
                tag: value.to_owned(),
            }),
            "group" => Ok(ProxyGroupMemberSpec::Group {
                name: value.to_owned(),
            }),
            other => Err(format!(
                "unknown member kind `{other}` (use `node:<tag>`, `group:<name>`, `direct`, or `reject`)"
            )),
        }
    } else {
        match token {
            "direct" => Ok(ProxyGroupMemberSpec::Direct),
            "reject" => Ok(ProxyGroupMemberSpec::Reject),
            other => Err(format!(
                "unknown member `{other}` (use `node:<tag>`, `group:<name>`, `direct`, or `reject`)"
            )),
        }
    }
}

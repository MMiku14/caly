//! Round 16: legacy client RPC enums.
//!
//! Before Round 11, the CLI shipped a single `Command`
//! enum with `Core` / `Sub` / `Sys` / `Config` /
//! `Profile` variants that mapped 1-for-1 to the
//! `core` / `sub` / `sys` / `config` / `profile`
//! daemon RPCs. Round 11 replaced the public surface
//! with the 5-namespace grammar (`daemon` / `tool` /
//! `show` / `set` / `completions`); the legacy enums
//! stayed as the *internal* input shape of
//! `commands::bridge::old_*`. Round 16 retires
//! `commands::bridge` (its 5 `old_*` shims are dead
//! after Round 15 rewired the dispatch) and moves the
//! 5 legacy enums here as private client RPC types.
//!
//! `ClientCommand` (in `client::mod.rs`) carries the
//! `Core` / `Sys` / `Config` variants that the UDS
//! pipeline still needs. The `SubCmd` and `ProfileCmd`
//! enums (removed in Round 32) were the only legacy
//! variants with no live consumer — their dispatch
//! bridges were retired in Round 16 with no future
//! refactor slated to revive them.

use std::path::PathBuf;

use crate::cli::Editor;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigCmd {
    Apply,
    Generate,
    Default,
    Validate,
    Check(PathBuf),
    Path,
    Files,
    Edit(Editor),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SysCmd {
    Proxy(bool),
    /// `sysproxy pac <url>`: desktop proxy auto mode with a PAC URL.
    ProxyPac(String),
    Tun(bool),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoreCmd {
    Start,
    Stop,
    Restart,
    Switch(String),
    Select {
        node: Option<String>,
        delay: bool,
        poll: bool,
    },
    CloseConnections,
    Mode(String),
    ProxyGroups,
    ListConnections,
    ListNodes,
    Traffic,
    Delay {
        all: bool,
        name: Option<String>,
        url: Option<String>,
        /// Per-URL sample count (1..=5); `None`
        /// means "use the default". The offline
        /// `Query::UrlTest` path resolves the
        /// default at the dispatch boundary.
        samples: Option<u32>,
    },
    Rules,
    RuleMatch(String),
}

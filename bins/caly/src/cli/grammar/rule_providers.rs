//! Clap derive grammar for the `set rule-provider …` leaves.
//!
//! Split out of `cli/grammar.rs` (audit #70 file-length budget):
//! declarative CRUD for `rule_providers:` (Round 12); the public
//! command-model enum [`crate::cli::SetRuleProviderCmd`] mirrors
//! these variants 1:1.

use std::path::PathBuf;

use clap::Subcommand;

/// Round 12: declarative CRUD for `rule_providers:`. Each leaf
/// takes the same `--apply` / `--dry-run` policy as the other
/// `set` resources; `refresh` always writes (the materialised
/// file IS the body the kernel reads).
#[derive(Subcommand, Debug)]
#[command(disable_help_subcommand = true)]
pub(in crate::cli) enum ClapSetRuleProviderCmd {
    /// Add a new `http` rule provider. The body is fetched
    /// from `<URL>` on a `<MS>` cadence (default 24h).
    #[command(name = "add-http")]
    AddHttp {
        name: String,
        url: String,
        #[arg(long, value_name = "MS", default_value_t = 86_400_000)]
        interval_ms: u64,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Add a new `file` rule provider. The body is read from
    /// `<PATH>` at boot and on `mtime` change.
    #[command(name = "add-file")]
    AddFile {
        name: String,
        path: PathBuf,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Add a new `inline` rule provider. The body is a
    /// bounded inline payload (capped at 256 KiB).
    #[command(name = "add-inline")]
    AddInline {
        name: String,
        #[arg(long, value_name = "PAYLOAD")]
        payload: String,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Remove a provider from `rule_providers:`.
    Remove {
        name: String,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Enable a disabled provider.
    Enable {
        name: String,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Disable a provider.
    Disable {
        name: String,
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Refresh the materialised body of one or every provider.
    /// Always writes; no `--apply` / `--dry-run` flag.
    Refresh { name: Option<String> },
    /// List declared providers (and their enable state).
    List,
}

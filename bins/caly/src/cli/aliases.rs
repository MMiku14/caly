//! W1: pre-parse argv expansion layer (the alias engine).
//!
//! Two ordered rule sets, both shaped `from-prefix → to-prefix`:
//!
//! 1. [`DEPRECATED`] — v1 command paths rewritten to the v3 tree.
//!    Every hit emits one deprecation notice; the caller prints them
//!    to stderr so scripted stdout consumers never see them.
//! 2. [`SHORTCUTS`] — built-in abbreviations (`n s` → `node
//!    select`, …). Hard-coded per cli-v3-design.md §9.1.
//!
//! Design invariants (cli-v3-design.md D5 / R1):
//!
//! - **Prefix longest-match** against whole argv tokens (no regex,
//!   no string splitting of user values — a rule only ever rewrites
//!   the command head, never an argument tail).
//! - **Global flags are skipped**, not rewritten: `--json`,
//!   `--socket X` / `--socket=X`, `--core`, `--mihomo-bin`,
//!   `--sing-box-bin`, `--help`/`--version` appear before the
//!   command head and must ride through untouched.
//! - **Rewrite depth ≤ 3** so a future table cycle (a→b→a) can
//!   never hang the CLI.
//! - Expansion stops at `--` (everything after it is operand data),
//!   and an unrecognised dash token blocks expansion entirely
//!   (conservative: never rewrite downstream of a token we do not
//!   understand).
//! - Unknown command heads pass through untouched; clap owns the
//!   unknown-subcommand error. (R1: W1-β keeps the original argv
//!   alongside the expanded one for error back-references.)
//!
//! Staging: W1-α lands the engine with **empty built-in tables**
//! (identity expansion ⇒ zero behaviour change, the v1 grammar
//! stays wired). W1-β fills both tables with the full v1→v3 map
//! and swaps the grammar in the same batch, so every old path
//! keeps working through this layer for one deprecation window.

/// One rewrite rule: `from` token prefix → `to` token prefix.
/// `from` must never be empty and, within one table, two rules'
/// `from` prefixes must not overlap (cli_tests asserts the
/// built-in tables in W1-β).
type Rule = (&'static [&'static str], &'static [&'static str]);

/// v1 → v3 deprecated command paths (cli-v3-design.md §3, W1-β).
/// Prefix longest-match; every entry emits one stderr notice.
/// Invariants asserted in tests (D12):
/// - no two `from` prefixes (across both tables) overlap;
/// - every `to` head is a built-in v3 command word;
/// - no rule is a fixed point (`from == to` would shadow nothing
///   but waste a pass).
const DEPRECATED: &[Rule] = &[
    // show namespace
    (&["show", "status"], &["status"]),
    (&["show", "core", "health"], &["status", "--verbose"]),
    (&["show", "core", "nodes"], &["node", "list"]),
    (&["show", "core", "groups"], &["node", "groups"]),
    (&["show", "core", "connections"], &["connections"]),
    (&["show", "core", "traffic"], &["traffic"]),
    (&["show", "core", "mode"], &["mode"]),
    (&["show", "core", "rules"], &["rules"]),
    (&["show", "proxy", "list"], &["node", "list"]),
    (&["show", "proxy", "show"], &["node", "show"]),
    (&["show", "proxy", "groups"], &["node", "groups"]),
    (&["show", "sub", "providers"], &["sub", "list"]),
    (&["show", "sub", "parse"], &["sub", "parse"]),
    (&["show", "sub", "import"], &["sub", "import"]),
    (&["show", "profile", "list"], &["profile", "list"]),
    (&["show", "profile", "show"], &["profile", "show"]),
    (&["show", "config", "path"], &["config", "path"]),
    (&["show", "config", "files"], &["config", "files"]),
    (&["show", "config", "validate"], &["config", "validate"]),
    // set core
    (&["set", "core", "start"], &["core", "start"]),
    (&["set", "core", "stop"], &["core", "stop"]),
    (&["set", "core", "restart"], &["core", "restart"]),
    (&["set", "core", "switch"], &["core", "switch"]),
    (&["set", "core", "select"], &["node", "select"]),
    (&["set", "core", "mode"], &["mode"]),
    (
        &["set", "core", "close-connections"],
        &["connections", "close"],
    ),
    (&["set", "core", "delay"], &["node", "ping"]),
    (&["set", "core", "url-test"], &["node", "test"]),
    // set proxy (+ system proxy leaves)
    (&["set", "proxy", "add"], &["node", "add"]),
    (&["set", "proxy", "edit"], &["node", "edit"]),
    (&["set", "proxy", "remove"], &["node", "remove"]),
    (&["set", "proxy", "import"], &["node", "import"]),
    (&["set", "proxy", "on"], &["sysproxy", "on"]),
    (&["set", "proxy", "off"], &["sysproxy", "off"]),
    (&["set", "tun"], &["tun"]),
    // set sub / profile / config / daemon
    (&["set", "sub", "refresh"], &["sub", "refresh"]),
    (&["set", "sub", "add"], &["sub", "add"]),
    (&["set", "sub", "remove"], &["sub", "remove"]),
    (&["set", "sub", "enable"], &["sub", "enable"]),
    (&["set", "sub", "disable"], &["sub", "disable"]),
    (&["set", "sub", "import"], &["sub", "import"]),
    (&["set", "profile", "add"], &["profile", "add"]),
    (&["set", "profile", "remove"], &["profile", "remove"]),
    (&["set", "profile", "edit"], &["profile", "edit"]),
    (&["set", "profile", "refresh"], &["profile", "refresh"]),
    (&["set", "profile", "export"], &["profile", "export"]),
    (&["set", "profile", "enable"], &["profile", "enable"]),
    (&["set", "profile", "disable"], &["profile", "disable"]),
    (&["set", "config", "apply"], &["config", "apply"]),
    (&["set", "config", "generate"], &["config", "generate"]),
    (&["set", "config", "default"], &["config", "default"]),
    (&["set", "config", "diff"], &["config", "diff"]),
    (&["set", "config", "edit"], &["config", "edit"]),
    (&["set", "daemon", "stop"], &["stop"]),
    (&["set", "daemon", "reload"], &["reload"]),
    (&["set", "daemon", "restart"], &["restart"]),
    // v1's handler delegated to the health snapshot — preserve
    // the output exactly via the verbose surface (C-J).
    (&["set", "daemon", "status"], &["status", "--verbose"]),
    // rule-provider / proxy-group / provider
    (
        &["set", "rule-provider", "add-http"],
        &["rule-provider", "add-http"],
    ),
    (
        &["set", "rule-provider", "add-file"],
        &["rule-provider", "add-file"],
    ),
    (
        &["set", "rule-provider", "add-inline"],
        &["rule-provider", "add-inline"],
    ),
    (
        &["set", "rule-provider", "remove"],
        &["rule-provider", "remove"],
    ),
    (
        &["set", "rule-provider", "enable"],
        &["rule-provider", "enable"],
    ),
    (
        &["set", "rule-provider", "disable"],
        &["rule-provider", "disable"],
    ),
    (
        &["set", "rule-provider", "refresh"],
        &["rule-provider", "refresh"],
    ),
    (
        &["set", "rule-provider", "list"],
        &["rule-provider", "list"],
    ),
    // `node add`'s dual form (C-P) accepts the v1 positional
    // name + kebab --type spellings, so the prefix swap suffices.
    (&["set", "proxy-group", "add"], &["node", "add"]),
    (&["set", "proxy-group", "remove"], &["node", "remove"]),
    (&["set", "proxy-group", "enable"], &["node", "enable"]),
    (&["set", "proxy-group", "disable"], &["node", "disable"]),
    (
        &["set", "proxy-group", "list"],
        &["node", "list", "--offline"],
    ),
    (
        &["set", "provider", "add-sources"],
        &["provider", "add-sources"],
    ),
    (
        &["set", "provider", "add-nodes"],
        &["provider", "add-nodes"],
    ),
    (&["set", "provider", "remove"], &["provider", "remove"]),
    // tool: dns/doctor are top-level verbs in v3; version/help stay.
    (&["tool", "doctor"], &["doctor"]),
    (&["tool", "dns"], &["dns"]),
];

/// Built-in abbreviations (cli-v3-design.md §9.1). The 10th entry, `t`
/// (tree view), entered the table with W3a's `--format=tree`.
const SHORTCUTS: &[Rule] = &[
    (&["st"], &["status"]),
    (&["n"], &["node"]),
    (&["n", "s"], &["node", "select"]),
    (&["n", "p"], &["node", "ping"]),
    (&["n", "t"], &["node", "test"]),
    (&["t"], &["node", "list", "--format=tree"]),
    (&["s"], &["sub", "list"]),
    (&["s", "r"], &["sub", "refresh"]),
    (&["d"], &["doctor"]),
    (&["c"], &["config", "diff"]),
];

/// Built-in v3 command heads; every rule target must start with
/// one of these (D12 collision guard). Test-only.
/// Built-in v3 command heads; every rule target must start with
/// one of these (D12 collision guard). Also the validation set for
/// user aliases (cli-v3-design.md §9.2): an expansion whose head is
/// not built-in is rejected.
pub(crate) const BUILTIN_HEADS: &[&str] = &[
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
    "flow",
    "history",
    "traffic",
    "mode",
    "sysproxy",
    "tun",
    "dns",
    "doctor",
    "tool",
    "completions",
];

/// Whether `token` is one of the built-in command heads (the D12 set).
pub(crate) fn is_builtin_head(token: &str) -> bool {
    BUILTIN_HEADS.contains(&token)
}

/// Global flags taking a separate value token (`--socket PATH`).
const VALUE_FLAGS: &[&str] = &[
    "--socket",
    "--core",
    "--mihomo-bin",
    "--sing-box-bin",
    "--format",
];
/// Global boolean flags riding before the command head.
const BOOL_FLAGS: &[&str] = &["--json", "--help", "-h", "--version", "-V"];

/// Maximum rewrite passes; guards against a cyclic rule table.
const MAX_PASSES: usize = 3;

/// Result of one expansion: the rewritten argv (without argv[0],
/// mirroring [`crate::cli::parse_args`]'s input) plus the
/// deprecation notices to print on stderr.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Expansion {
    /// R1 (cli-v3-design.md §14): the argv exactly as typed,
    /// kept so a follow-on clap error can back-link the
    /// original spelling to its expansion.
    pub(crate) typed: Vec<String>,
    pub(crate) argv: Vec<String>,
    pub(crate) notices: Vec<String>,
}

/// Expand `args` with the built-in tables. Identity in W1-α. Test-only
/// since production entry is [`expand_with_user`].
#[cfg(test)]
pub(crate) fn expand(args: Vec<String>) -> Expansion {
    expand_with(args, DEPRECATED, SHORTCUTS, &[])
}

/// Expand `args` with the built-in tables plus the user alias table
/// (cli-v3-design.md §9.2). The user table wins over built-in
/// shortcuts on equal prefix length; deprecated paths still carry
/// their notice.
pub(crate) fn expand_with_user(
    args: Vec<String>,
    user: &[(Vec<String>, Vec<String>)],
) -> Expansion {
    expand_with(args, DEPRECATED, SHORTCUTS, user)
}

/// Separates the leading global-flag run from the command
/// remainder, returning the remainder's start index. A `--`
/// terminator or an unknown dash token yields `args.len()` (no
/// expansion). Shared with [`crate::cli::parse_args`]'s post-expansion
/// head validation.
pub(crate) fn command_head(args: &[String]) -> usize {
    let mut i = 0;
    while i < args.len() {
        let token = &args[i];
        if token == "--" {
            return args.len(); // everything after `--` is operand data
        }
        if !token.starts_with('-') {
            return i; // first positional = command head
        }
        let (name, inline_value) = match token.split_once('=') {
            Some((name, _value)) => (name, true),
            None => (token.as_str(), false),
        };
        if BOOL_FLAGS.contains(&name) {
            i += 1;
        } else if VALUE_FLAGS.contains(&name) {
            // `--socket=/x` consumes one token; `--socket /x` two.
            i += if inline_value { 1 } else { 2 };
        } else {
            return args.len(); // unknown dash token: refuse to expand
        }
    }
    args.len()
}

/// Longest matching prefix rule within one table; equal-length
/// ties keep the later table entry (tables must not overlap by
/// construction — see the type-level note on [`Rule`]).
fn longest<'a>(head: &[String], rules: &'a [Rule]) -> Option<(&'a Rule, usize)> {
    rules
        .iter()
        .filter_map(|rule| {
            let (from, _to) = rule;
            (from.len() <= head.len() && from.iter().zip(head).all(|(f, h)| *f == h))
                .then_some((rule, from.len()))
        })
        .max_by_key(|(_rule, len)| *len)
}

/// Longest match across all three tables; on equal prefix length the
/// user table wins (customization value), then the deprecated path
/// (it carries the notice the user must see). Returns the matched
/// from/to token pairs and whether the match came from the deprecated
/// table (cloned: rule tables are short, the hot loop is the parse).
fn match_rule<'a>(
    head: &[String],
    shortcuts: &'a [Rule],
    deprecated: &'a [Rule],
    user: &'a [(Vec<String>, Vec<String>)],
) -> Option<(Vec<String>, Vec<String>, bool)> {
    let builtin_best = match (longest(head, shortcuts), longest(head, deprecated)) {
        (Some((shortcut, slen)), Some((old, dlen))) => Some(if dlen >= slen {
            (owned(old), true)
        } else {
            (owned(shortcut), false)
        }),
        (Some((shortcut, _)), None) => Some((owned(shortcut), false)),
        (None, Some((old, _))) => Some((owned(old), true)),
        (None, None) => None,
    };
    let user_best: Option<(usize, Vec<String>, Vec<String>)> =
        user.iter()
            .filter_map(|(from, to)| {
                (from.len() <= head.len() && from.iter().zip(head).all(|(f, h)| f == h))
                    .then_some((from.len(), from.clone(), to.clone()))
            })
            .max_by_key(|(len, _, _)| *len);
    match (builtin_best, user_best) {
        (Some(((from, to), is_deprecated)), Some((ulen, ufrom, uto))) => {
            // The user table wins ties; a strictly longer built-in
            // match still wins (built-ins keep their full-prefix
            // reach).
            if ulen >= from.len() {
                Some((ufrom, uto, false))
            } else {
                Some((from, to, is_deprecated))
            }
        }
        (Some(((from, to), is_deprecated)), None) => Some((from, to, is_deprecated)),
        (None, Some((_, ufrom, uto))) => Some((ufrom, uto, false)),
        (None, None) => None,
    }
}

/// Owned clone of a built-in rule's from/to token pairs.
fn owned(rule: &Rule) -> (Vec<String>, Vec<String>) {
    (
        rule.0.iter().map(|token| (*token).to_owned()).collect(),
        rule.1.iter().map(|token| (*token).to_owned()).collect(),
    )
}

fn expand_with(
    args: Vec<String>,
    deprecated: &[Rule],
    shortcuts: &[Rule],
    user: &[(Vec<String>, Vec<String>)],
) -> Expansion {
    let typed = args; // R1: anchor for the error back-link.
    let split = command_head(&typed);
    let (globals, mut head) = (typed[..split].to_vec(), typed[split..].to_vec());
    let mut notices = Vec::new();
    let mut passes = 0;
    while passes < MAX_PASSES && !head.is_empty() && head[0] != "--" {
        passes += 1;
        let Some((from, to, is_deprecated)) = match_rule(&head, shortcuts, deprecated, user) else {
            break;
        };
        if is_deprecated {
            notices.push(format!(
                "[deprecated] `caly {}` is now `caly {}`; \
                 the old path will be removed after the deprecation window",
                from.join(" "),
                to.join(" "),
            ));
        }
        head = to
            .iter()
            .map(|token| (*token).clone())
            .chain(head.into_iter().skip(from.len()))
            .collect();
    }
    let expanded = globals.into_iter().chain(head).collect();
    Expansion {
        typed,
        argv: expanded,
        notices,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|token| (*token).to_owned()).collect()
    }

    #[test]
    fn empty_tables_are_identity() {
        for argv in [
            &["node", "list"][..],
            &["--json", "status"],
            &["--socket", "/tmp/x.sock", "daemon"],
            &["--socket=/tmp/x.sock", "node", "select", "hk-01"],
            &[],
        ] {
            let expansion = expand_with(strings(argv), &[], &[], &[]);
            assert_eq!(expansion.argv, strings(argv));
            assert!(expansion.notices.is_empty());
        }
    }

    #[test]
    fn canonical_v3_heads_pass_through_untouched() {
        // Heads that are already v3 must never be rewritten (and
        // never emit notices).
        for argv in [
            &["node", "list"][..],
            &["status", "--verbose"],
            &[
                "node",
                "add",
                "--type",
                "urltest",
                "--name",
                "g",
                "--members",
                "direct",
            ],
            &["sub", "parse", "f.yaml"],
            &["config", "diff"],
            &["connections", "close"],
        ] {
            let expansion = expand(strings(argv));
            assert_eq!(expansion.argv, strings(argv), "rewrote canonical {argv:?}");
            assert!(expansion.notices.is_empty());
        }
    }

    #[test]
    fn global_flags_ride_before_a_rewritten_head() {
        let deprecated: &[Rule] = &[(&["show", "status"], &["status"])];
        let expansion = expand_with(
            strings(&["--json", "--socket", "/x", "show", "status"]),
            deprecated,
            &[],
            &[],
        );
        assert_eq!(
            expansion.argv,
            strings(&["--json", "--socket", "/x", "status"])
        );
        assert_eq!(expansion.notices.len(), 1);
        assert!(expansion.notices[0].contains("show status"));
        assert!(expansion.notices[0].contains("caly status"));
    }

    #[test]
    fn equals_form_global_flag_still_finds_the_head() {
        let deprecated: &[Rule] = &[(&["show", "status"], &["status"])];
        let expansion = expand_with(
            strings(&["--socket=/x", "show", "status"]),
            deprecated,
            &[],
            &[],
        );
        assert_eq!(expansion.argv, strings(&["--socket=/x", "status"]));
    }

    #[test]
    fn unknown_dash_token_or_double_dash_blocks_expansion() {
        let deprecated: &[Rule] = &[(&["show", "status"], &["status"])];
        for argv in [&["--foo", "show", "status"][..], &["--", "show", "status"]] {
            let expansion = expand_with(strings(argv), deprecated, &[], &[]);
            assert_eq!(expansion.argv, strings(argv));
            assert!(expansion.notices.is_empty());
        }
    }

    #[test]
    fn longest_prefix_wins() {
        let shortcuts: &[Rule] = &[
            (&["show"], &["display"]),
            (&["show", "sub", "parse"], &["sub", "parse"]),
        ];
        let expansion = expand_with(
            strings(&["show", "sub", "parse", "f.yaml"]),
            &[],
            shortcuts,
            &[],
        );
        assert_eq!(expansion.argv, strings(&["sub", "parse", "f.yaml"]));
    }

    #[test]
    fn rewrite_chains_are_depth_limited() {
        // Pathological cyclic table: a→b, b→a. Must terminate with
        // one of the two heads, never hang.
        let shortcuts: &[Rule] = &[(&["a"], &["b"]), (&["b"], &["a"])];
        let expansion = expand_with(strings(&["a", "x"]), &[], shortcuts, &[]);
        assert_eq!(expansion.argv.len(), 2);
        assert!(expansion.argv[0] == "a" || expansion.argv[0] == "b");
    }

    #[test]
    fn shortcut_expansion_keeps_arguments() {
        let shortcuts: &[Rule] = &[(&["n"], &["node"]), (&["n", "s"], &["node", "select"])];
        let expansion = expand_with(strings(&["n", "s", "hk-01"]), &[], shortcuts, &[]);
        assert_eq!(expansion.argv, strings(&["node", "select", "hk-01"]));
        assert!(expansion.notices.is_empty());
    }

    #[test]
    fn user_aliases_join_the_expansion_and_win_ties() {
        // User table (cli-v3-design.md §9.2) participates in the same
        // loop; equal-prefix ties go to the user table.
        let user: &[(Vec<String>, Vec<String>)] = &[(
            vec!["gg".to_owned()],
            vec!["node".to_owned(), "list".to_owned()],
        )];
        let expansion = expand_with_user(strings(&["gg", "--offline"]), user);
        assert_eq!(expansion.argv, strings(&["node", "list", "--offline"]));
        // A user alias whose target is itself a user alias chains
        // through the same depth-limited loop.
        let chained: &[(Vec<String>, Vec<String>)] = &[
            (vec!["a".to_owned()], vec!["b".to_owned()]),
            (vec!["b".to_owned()], vec!["status".to_owned()]),
        ];
        let expansion = expand_with_user(strings(&["a"]), chained);
        assert_eq!(expansion.argv, strings(&["status"]));
        // Cyclic user aliases terminate (depth limit), never hang.
        let cyclic: &[(Vec<String>, Vec<String>)] = &[
            (vec!["x".to_owned()], vec!["y".to_owned()]),
            (vec!["y".to_owned()], vec!["x".to_owned()]),
        ];
        let expansion = expand_with_user(strings(&["x"]), cyclic);
        assert!(expansion.argv[0] == "x" || expansion.argv[0] == "y");
    }

    #[test]
    fn user_alias_shadows_builtin_shortcut_on_equal_prefix() {
        // `t` is the built-in tree shortcut; a user alias named `t`
        // must win (customization value), without a deprecation notice.
        let user: &[(Vec<String>, Vec<String>)] =
            &[(vec!["t".to_owned()], vec!["status".to_owned()])];
        let expansion = expand_with_user(strings(&["t"]), user);
        assert_eq!(expansion.argv, strings(&["status"]));
        assert!(expansion.notices.is_empty(), "user aliases carry no notice");
    }

    #[test]
    fn deprecated_chain_collects_each_notice() {
        let deprecated: &[Rule] = &[
            (&["show", "core"], &["core-state"]),
            (&["core-state"], &["status"]),
        ];
        let expansion = expand_with(strings(&["show", "core", "nodes"]), deprecated, &[], &[]);
        assert_eq!(expansion.argv, strings(&["status", "nodes"]));
        assert_eq!(expansion.notices.len(), 2);
    }

    #[test]
    fn unknown_command_passes_through_for_clap_to_reject() {
        let expansion = expand(strings(&["frobnicate", "--widget"]));
        assert_eq!(expansion.argv, strings(&["frobnicate", "--widget"]));
        assert!(expansion.notices.is_empty());
    }

    // ── W1-β: built-in table integrity (D12) ────────────────────

    #[test]
    fn builtin_tables_have_no_duplicate_prefixes() {
        // Extension rows (`n` and `n s`) are legal — longest match
        // is deterministic. Duplicate `from` prefixes (same table
        // or across tables) are not: one of the pair is dead.
        let all: Vec<&[&str]> = SHORTCUTS
            .iter()
            .chain(DEPRECATED)
            .map(|(from, _)| *from)
            .collect();
        for (index, from) in all.iter().enumerate() {
            assert!(!from.is_empty(), "empty from prefix at #{index}");
            for other in all.iter().skip(index + 1) {
                assert_ne!(
                    from, other,
                    "duplicate rule prefix {from:?} (one rule can never fire)",
                );
            }
        }
    }

    #[test]
    fn builtin_rule_targets_start_with_builtin_heads() {
        for (from, to) in SHORTCUTS.iter().chain(DEPRECATED) {
            assert!(
                BUILTIN_HEADS.contains(&to[0]),
                "rule {from:?} → {to:?}: head `{}` is not a built-in v3 command",
                to[0],
            );
            assert_ne!(from, to, "fixed-point rule {from:?} shadows nothing");
        }
    }

    #[test]
    fn shortcut_heads_do_not_collide_with_builtin_heads() {
        for (from, _) in SHORTCUTS {
            assert!(
                !BUILTIN_HEADS.contains(&from[0]),
                "shortcut `{from:?}` collides with a built-in command head",
            );
        }
    }

    #[test]
    fn deprecated_notices_name_both_paths() {
        let expansion = expand(strings(&["show", "sub", "parse", "f.yaml"]));
        assert_eq!(expansion.argv, strings(&["sub", "parse", "f.yaml"]));
        assert_eq!(expansion.notices.len(), 1);
        assert!(expansion.notices[0].contains("[deprecated]"));
        assert!(expansion.notices[0].contains("show sub parse"));
        assert!(expansion.notices[0].contains("caly sub parse"));
    }

    #[test]
    fn typed_keeps_the_original_spelling_for_r1_backlink() {
        let expansion = expand(strings(&["st"]));
        assert_eq!(expansion.typed, strings(&["st"]));
        assert_eq!(expansion.argv, strings(&["status"]));
        // Pass-through input keeps typed == argv, so parse_args
        // prints no back-link note for un-rewritten spellings.
        let plain = expand(strings(&["node", "list"]));
        assert_eq!(plain.typed, plain.argv);
    }
}

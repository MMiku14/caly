//! Generated `config.d/85-proxy-groups.yaml` fragment text.
//!
//! The default ships with `proxy_groups: []` so a fresh
//! install has zero declared groups; rules fall through to
//! the subscription-rendered `url-test` group when the
//! user has not declared their own. The fragment sorts at
//! position 85 (between the 80-observability and 90-routing
//! fragments) so user-declared groups reach the rendered
//! config *before* the routing rules that may reference
//! them; the loader still deep-merges on top of the base
//! config and the earlier `config.d/` fragments, so the
//! effective merge order is:
//!
//! base → 10..80 → 85 (proxy_groups) → 90 (routing) → 95
//! (rule_providers) → 98 (profiles).
//!
//! Every snippet in the fragment is commented by default.
//! Uncommenting produces a self-validating config
//! (validated at load time by the
//! `validate_proxy_groups` step in `schema/validate.rs`).
//! Five type kinds are documented; the same set Mihomo
//! and sing-box understand (`select` / `url-test` /
//! `fallback` / `load-balance` / `relay`).

use std::path::PathBuf;

/// The `config.d/85-proxy-groups.yaml` fragment. Empty by
/// default; every snippet is commented so uncommenting
/// produces a self-validating config (validated at load
/// time by the `validate_proxy_groups` step in
/// `schema/validate.rs`).
pub(super) fn proxy_groups_fragment() -> (PathBuf, String) {
    let body = String::from(
        "# User-declared proxy groups. Each entry renders as one\n\
         # Mihomo `proxy-groups:` block (or one sing-box `outbounds`\n\
         # selector/urltest/fallback/loadbalance/relay). `name` is\n\
         # the tag referenced from `rules:` and from other groups'\n\
         # `members:` lists. The five `type:` kinds map 1-for-1 to\n\
         # the Mihomo kernel vocabulary:\n\
         #\n\
         #   select        operator picks one member by hand (default)\n\
         #   url-test      kernel probes every member, picks fastest\n\
         #   fallback      try members in order, fall through on failure\n\
         #   load-balance  round-robin over healthy members\n\
         #   relay         chain members in declaration order\n\
         #\n\
         # Each `members:` entry is `kind: node|group|direct|reject`:\n\
         #\n\
         #   - kind: node    tag: <node tag>\n\
         #   - kind: group   name: <other group name>\n\
         #   - kind: direct  (renders as `DIRECT`)\n\
         #   - kind: reject  (renders as `REJECT`)\n\
         #\n\
         # Probe-driven groups (`url-test` / `fallback` /\n\
         # `load-balance`) need a `url_test:` block; non-probe\n\
         # groups (`select` / `relay`) must NOT have one.\n\
         proxy_groups: []\n\
         \n\
         # Example groups (uncomment to use):\n\
         # proxy_groups:\n\
         #   - name: Proxy\n\
         #     type: select\n\
         #     members:\n\
         #       - kind: node\n\
         #         tag: hong-kong-1\n\
         #       - kind: node\n\
         #         tag: singapore-1\n\
         #       - kind: direct\n\
         #   - name: Auto\n\
         #     type: url-test\n\
         #     members:\n\
         #       - kind: group\n\
         #         name: Proxy\n\
         #     url_test:\n\
         #       url: http://www.gstatic.com/generate_204\n\
         #       interval_seconds: 300\n\
         #       tolerance_ms: 50\n\
         #   - name: Fallback\n\
         #     type: fallback\n\
         #     members:\n\
         #       - kind: group\n\
         #         name: Auto\n\
         #       - kind: direct\n\
         #     url_test:\n\
         #       url: http://www.gstatic.com/generate_204\n\
         #       interval_seconds: 300\n\
         #   - name: Balanced\n\
         #     type: load-balance\n\
         #     members:\n\
         #       - kind: group\n\
         #         name: Auto\n\
         #     url_test:\n\
         #       url: http://www.gstatic.com/generate_204\n\
         #       interval_seconds: 300\n\
         #   - name: Chain\n\
         #     type: relay\n\
         #     members:\n\
         #       - kind: group\n\
         #         name: Auto\n\
         #       - kind: group\n\
         #         name: Fallback\n",
    );
    (PathBuf::from("config.d/85-proxy-groups.yaml"), body)
}

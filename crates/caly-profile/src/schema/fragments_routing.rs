//! Generated `config.d/90-routing.yaml` and `config.d/95-rule-providers.yaml`
//! fragment text.
//!
//! The routing fragment seeds the `rules:` list with an example that uses
//! each new matcher (`RULE-SET`, `GEOSITE`, `PROCESS-NAME`,
//! `SRC-IP-CIDR`) and the legacy ones (`DOMAIN-SUFFIX`, `GEOIP`, `IP-CIDR`)
//! so an operator can copy-paste the snippets into a real config. Every
//! line is commented by default — the loader does not auto-create rules.
//!
//! The rule-providers fragment ships an empty `rule_providers: []` plus
//! commented Loyalsoldier `v2ray-rules-dat` examples covering all three
//! source kinds (`http` / `file` / `inline`) and the three
//! `behavior:` values the renderer understands. Uncommenting the
//! examples produces a self-validating config (validated at load time).

use std::path::PathBuf;

/// The `config.d/90-routing.yaml` fragment. Empty by default so a fresh
/// install has zero user rules; the renderer always appends the built-in
/// `MATCH,<group>` catch-all so the kernel still routes every packet.
pub(super) fn routing_fragment() -> (PathBuf, String) {
    (
        PathBuf::from("config.d/90-routing.yaml"),
        "# Routing rules. First match wins. The `MATCH,<group>` catch-all is\n\
         # appended by the renderer when this list does not already end one.\n\
         # Available matchers:\n\
         #   - DOMAIN,<host>           exact host match\n\
         #   - DOMAIN-SUFFIX,<suffix>  host ends with the suffix\n\
         #   - DOMAIN-KEYWORD,<key>    host contains the keyword\n\
         #   - GEOIP,<cc>              GeoIP database lookup (Mihomo / sing-box)\n\
         #   - IP-CIDR,<cidr>          source/destination IP CIDR\n\
         #   - SRC-IP-CIDR,<cidr>      source-only IP CIDR (sing-box 1.12+)\n\
         #   - PROCESS-NAME,<glob>     process executable name (mihomo only)\n\
         #   - RULE-SET,<name>,<out>   user-declared provider from\n\
         #                             `rule_providers:` (Mihomo / sing-box)\n\
         #   - GEOSITE,<category>,<out>  SagerNet geosite category; caly\n\
         #                                auto-emits the rule set on the\n\
         #                                sing-box side, mihomo uses the\n\
         #                                built-in `geosite`.\n\
         #   - MATCH,<out>             catch-all\n\
         rules: []\n\
         \n\
         # Example routing (uncomment the lines you want, then list the\n\
         # referenced providers in 95-rule-providers.yaml below):\n\
         # rules:\n\
         #   - DOMAIN-SUFFIX,example.com,DIRECT\n\
         #   - GEOIP,CN,DIRECT\n\
         #   - GEOSITE,private,DIRECT\n\
         #   - SRC-IP-CIDR,192.168.0.0/16,DIRECT\n\
         #   - RULE-SET,my-google,DIRECT\n\
         #   - PROCESS-NAME,docker,DIRECT\n\
         #   - MATCH,PROXY\n"
            .to_owned(),
    )
}

/// The `config.d/95-rule-providers.yaml` fragment. Empty by default.
pub(super) fn rule_providers_fragment() -> (PathBuf, String) {
    (
        PathBuf::from("config.d/95-rule-providers.yaml"),
        "# User-declared rule providers. Each entry must have a unique\n\
         # `name`; a `RULE-SET,<name>,<out>` rule in 90-routing.yaml\n\
         # refers to that name. Available source kinds (`type:`):\n\
         #\n\
         #   type: http    url + interval_ms (>= 60000) ; the core\n\
         #                 fetches the body on its own polling cadence.\n\
         #   type: file    path on disk; the core reads the body once\n\
         #                 at start.\n\
         #   type: inline  payload: the rule body inline; caly-specific\n\
         #                 extension to the upstream spec, see CHANGELOG\n\
         #                 `Round 6`.\n\
         #\n\
         # Available behaviors (`behavior:`):\n\
         #   domain         exact host matchers (DOMAIN,...)\n\
         #   domain_suffix  suffix matchers (DOMAIN-SUFFIX,...)\n\
         #   ip_cidr        CIDR matchers (IP-CIDR,...)\n\
         #   classical      full Mihomo rule lines (TYPE,arg,out)\n\
         #\n\
         # Available formats (`format:`):\n\
         #   source         text body (default)\n\
         #   binary         compiled `.srs` for sing-box, only valid\n\
         #                  with `type: http` or `type: file`.\n\
         rule_providers: []\n\
         \n\
         # Example providers (Loyalsoldier v2ray-rules-dat; uncomment to use):\n\
         # rule_providers:\n\
         #   - name: private\n\
         #     type: http\n\
         #     behavior: domain_suffix\n\
         #     format: source\n\
         #     url: https://raw.githubusercontent.com/Loyalsoldier/v2ray-rules-dat/release/private-domain-list.txt\n\
         #     interval_ms: 86400000\n\
         #   - name: gfw\n\
         #     type: http\n\
         #     behavior: domain_suffix\n\
         #     format: source\n\
         #     url: https://raw.githubusercontent.com/Loyalsoldier/v2ray-rules-dat/release/gfw.txt\n\
         #     interval_ms: 86400000\n\
         #   - name: cncidr\n\
         #     type: http\n\
         #     behavior: ip_cidr\n\
         #     format: source\n\
         #     url: https://raw.githubusercontent.com/Loyalsoldier/v2ray-rules-dat/release/cn-cidr.txt\n\
         #     interval_ms: 86400000\n\
         #   - name: local-extra\n\
         #     type: file\n\
         #     behavior: domain_suffix\n\
         #     format: source\n\
         #     path: /etc/caly/extra-rules.yaml\n\
         #   - name: inline-blocklist\n\
         #     type: inline\n\
         #     behavior: domain\n\
         #     format: source\n\
         #     payload: |\n\
         #       ads.example.com\n\
         #       track.example.org\n\
         #       metrics.example.io\n"
            .to_owned(),
    )
}

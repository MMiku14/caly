//! Generated `config.d/40-dns.yaml` fragment text.

use std::path::PathBuf;

/// The `config.d/40-dns.yaml` fragment (kept out of the file list builder for
/// the function-size budget).
pub(super) fn dns_fragment() -> (PathBuf, String) {
    (
        PathBuf::from("config.d/40-dns.yaml"),
        "# DNS block rendered into the kernel config. Environment `CALY_DNS_*`\n\
             # still overrides this section when `CALY_DNS_ENABLE` is set.\n\
             #\n\
             # fake_ip_filter lists domains that must receive REAL IPs even in\n\
             # fake-ip mode (STUN/WebRTC, mDNS/LAN discovery, NTP, NCSI network\n\
             # checks, game platforms); the defaults below cover the common cases.\n\
             # fallback_filter fights DNS poisoning: answers inside `ipcidr` or a\n\
             # GeoIP mismatch against `geoip_code` switch to the fallback servers.\n\
             # `listen` below 1024 needs CAP_NET_BIND_SERVICE; a per-user daemon\n\
             # should prefer 127.0.0.1:1053-style addresses (Mihomo only; sing-box\n\
             # ignores fake_ip_filter/fallback_filter/listen by design).\n\
             dns:\n\
             \x20 enabled: false\n\
             \x20 mode: fake-ip          # standard | fake-ip | redir-host\n\
             \x20 ipv6: false\n\
             \x20 # listen: 127.0.0.1:1053\n\
             \x20 nameservers: []\n\
             \x20 fallback: []\n\
             \x20 fake_ip_filter:\n\
             \x20\x20 - '+.local'\n\
             \x20\x20 - '+.lan'\n\
             \x20\x20 - '+.home.arpa'\n\
             \x20\x20 - '+.stun.*'\n\
             \x20\x20 - 'stun.l.google.com'\n\
             \x20\x20 - 'msftconnecttest.com'\n\
             \x20\x20 - 'dns.msftncsi.com'\n\
             \x20\x20 - 'time.apple.com'\n\
             \x20\x20 - '+.pool.ntp.org'\n\
             \x20\x20 - '+.srv.nintendo.net'\n\
             \x20\x20 - '+.stun.playstation.net'\n\
             \x20\x20 - 'xbox.*.microsoft.com'\n\
             \x20 # fallback_filter:\n\
             \x20 #   geoip: true\n\
             \x20 #   geoip_code: CN\n\
             \x20 #   ipcidr: [240.0.0.0/4, 0.0.0.0/32]\n\
             \x20 #   domain: ['+.google.com', '+.github.com']\n\
             \n\
             # To enable, replace the empty lists above with real resolvers, e.g.:\n\
             # dns:\n\
             #   enabled: true\n\
             #   mode: fake-ip\n\
             #   nameservers:\n\
             #     - 8.8.8.8\n\
             #     - 1.1.1.1\n\
             #   fallback:\n\
             #     - 223.5.5.5\n\
             #   fake_ip_range: 198.18.0.1/16\n"
            .to_owned(),
    )
}

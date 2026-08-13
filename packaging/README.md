# Packaging

## systemd (per-user service)

```bash
bash packaging/install-systemd.sh
```

Installs the release binary to `/usr/local/bin/caly`, a per-user unit
(`~/.config/systemd/user/caly.service`), and enables/starts it.

Notes:

- The unit runs the daemon only; cores are started on demand by
  `caly core start` (or `auto_start_core: true`).
- TUN needs CAP_NET_ADMIN on the core binaries. Run `caly doctor --fix`
  once (one sudo prompt) to grant it; without it, `caly tun on` still
  works through the configured `escalation` (pkexec/sudo) but the
  sing-box self-managed TUN inbound requires the binary capability.
- The daemon is SIGTERM-friendly (systemd's default stop signal): it
  exits cleanly and removes its socket. `caly restart` from the CLI
  spawns a replacement process in the same way `systemctl restart`
  would.

## Manual install (no systemd)

```bash
cargo build --release --bin caly
install -Dm755 target/release/caly ~/.local/bin/caly
caly config generate && caly config edit
```

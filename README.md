# caly

Daemon-first proxy core manager (Mihomo + sing-box).

`caly` manages Mihomo and sing-box through one daemon: a shared config
tree, a profile system, a subscription pipeline, TUN / system-proxy
effects, and a typed control plane. The CLI follows a v3 resource-domain
design (`docs/cli-v3-design.md`): entries (protocol nodes AND proxy
groups) are one domain, subscriptions a second, configuration a third.

## Features

- **Dual core**: Mihomo and sing-box rendered from one config tree;
  switch at runtime (`caly core switch sing-box`).
- **Subscriptions**: Clash YAML / URI-lines / base64 intake, offline
  cache, per-source refresh cadence (`--every`), `sub parse` entry-tree
  view with a frozen JSON contract.
- **TUN**: one command (`caly tun on` / `caly tun off`), automatic
  policy routing (sing-box auto_route), pkexec/sudo escalation,
  fake-ip DNS fallback injected when the config has no DNS section.
- **Profiles**: layered config with `profile use` context switching.
- **v3 CLI**: entry-tree listings (`--format=tree`), terraform-plan
  `config diff --format=diff`, user aliases (`~/.config/caly/aliases.yaml`),
  adaptive table/TSV output, exit codes pinned to 0/1/2/3/130.

## Quick start

```bash
# Build (toolchain pinned in rust-toolchain.toml)
cargo build --release

# First run: generate a documented default config
caly config generate
caly config edit            # or edit ~/.config/caly/config.yaml

# Declare a subscription and pull it
caly sub add https://example.com/sub.yaml --name my-sub
caly sub refresh

# Start the daemon (a bare `caly` also runs it), then the core
caly daemon &              # or just: caly
caly core start

# Drive the proxy from the CLI
caly node list             # online snapshot
caly node list --format=tree   # offline declared entry tree
caly node select <id>
caly t                     # shortcut: node list --format=tree
caly sub list              # offline-derived node/group counts
caly config diff --format=diff

# TUN mode (needs CAP_NET_ADMIN; `caly doctor --fix` grants it once)
caly tun on
caly tun status
```

## Configuration

`~/.config/caly/config.yaml` (plus `config.d/*.yaml` fragments) is the
single source of truth; the daemon renders the active core's config
from it. Key sections:

- `core:` — `mihomo` or `sing-box`.
- `subscriptions:` — `url` (legacy scalar) and/or `sources:` entries
  with per-source `enabled`, `name`, `refresh_every_minutes`.
- `providers:` — explicit provider list (`kind: subscription-sources`
  or `kind: !inline-nodes` YAML-tag form).
- `proxy_groups:` — declared selectors / url-test groups (ASCII
  path-safe names).
- `tun:` — `enabled`, `mtu`, `stack`, `auto_route`, `strict_route`,
  `escalation` (`auto|pkexec|sudo|none`).
- `dns:` — optional; when a TUN inbound renders without one, a built-in
  fake-ip DNS block is injected automatically.
- `rules:` — Clash-format rule lines.

`caly config validate` / `caly config diff --format=diff` preview
changes before `caly config apply`.

## Documentation

- `docs/cli-v3-design.md` — the authoritative CLI v3 design (command
  tree, output contracts, JSON contracts, implementation rulings).
- `docs/cli-proxy-group-design.md` — the G-series design that fed v3.
- `docs/crate-replan.md` — crate topology and budgets.
- `FIX_PROGRESS.md` (workspace root) — the fix ledger: 80 audit items,
  W1–W4 implementation periods, gate results.
- `proof/` (workspace root) — test scripts and review reports.

## Development

```bash
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets   # must stay 0 warnings
CARGO_BUILD_JOBS=1 CARGO_PROFILE_TEST_DEBUG=0 cargo test --workspace
cargo fmt --check
```

The workspace pins Rust 1.88 (rust-toolchain.toml). On machines with a
tight linker budget use `CARGO_BUILD_JOBS=1 CARGO_PROFILE_TEST_DEBUG=0`.

### Template worker

`caly-template-worker` is an isolated one-request template rendering
worker: it reads a framed request (`template source + context`) from
stdin and writes the rendered output to stdout. The daemon spawns it
per render so template evaluation runs outside the privileged process.
Exit codes: 0 = rendered, 2 = request/render failure.

## License

MIT OR Apache-2.0 (see LICENSE / LICENSE-APACHE / LICENSE-MIT).

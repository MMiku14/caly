#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
lock="$root/vendor/kernels.lock.toml"
destination="$root/vendor/bin"

if [[ "$(uname -s)" != "Linux" || "$(uname -m)" != "x86_64" ]]; then
  printf 'unsupported test-kernel platform: %s/%s\n' "$(uname -s)" "$(uname -m)" >&2
  exit 1
fi

read_lock() {
  local section="$1" key="$2"
  python3 - "$lock" "$section" "$key" <<'PY'
import sys, tomllib
with open(sys.argv[1], "rb") as source:
    value = tomllib.load(source)[sys.argv[2]][sys.argv[3]]
print(value)
PY
}

mihomo_url="$(read_lock mihomo url)"
mihomo_archive_sha="$(read_lock mihomo archive_sha256)"
mihomo_binary_sha="$(read_lock mihomo binary_sha256)"
sing_url="$(read_lock sing_box url)"
sing_archive_sha="$(read_lock sing_box archive_sha256)"
sing_binary_sha="$(read_lock sing_box binary_sha256)"

work="$(mktemp -d "$root/.kernel-download.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir -p "$destination" "$work/sing-box"

curl -L --fail --retry 3 --retry-delay 2 --silent --show-error \
  "$mihomo_url" -o "$work/mihomo.gz"
curl -L --fail --retry 3 --retry-delay 2 --silent --show-error \
  "$sing_url" -o "$work/sing-box.tar.gz"

printf '%s  %s\n' "$mihomo_archive_sha" "$work/mihomo.gz" | sha256sum -c -
printf '%s  %s\n' "$sing_archive_sha" "$work/sing-box.tar.gz" | sha256sum -c -

gzip -dc "$work/mihomo.gz" > "$work/mihomo"
tar -xzf "$work/sing-box.tar.gz" -C "$work/sing-box"
sing_binary="$(find "$work/sing-box" -type f -name sing-box -print -quit)"
if [[ -z "$sing_binary" ]]; then
  printf 'sing-box archive did not contain the expected executable\n' >&2
  exit 1
fi

printf '%s  %s\n' "$mihomo_binary_sha" "$work/mihomo" | sha256sum -c -
printf '%s  %s\n' "$sing_binary_sha" "$sing_binary" | sha256sum -c -

install -m 0755 "$work/mihomo" "$destination/mihomo"
install -m 0755 "$sing_binary" "$destination/sing-box"

"$destination/mihomo" -v | head -n 1
"$destination/sing-box" version | head -n 1
printf 'verified test kernels installed in %s\n' "$destination"

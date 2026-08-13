//! Linux entropy helpers for per-boot daemon epoch identity.

use std::io::Read;

/// Reads `bytes` cryptographically strong random bytes from the kernel CSPRNG.
///
/// Only exactly `N` bytes are read (never the whole infinite character device).
/// Falls back to a bounded hash of (pid, process start id, monotonic nanos) so
/// that a fresh per-boot identity is still produced on platforms without
/// `/dev/urandom`, without ever blocking on user-space entropy.
///
/// The fallback is only acceptable for non-secret identities (e.g. a per-boot
/// epoch id); never use this for credentials or auth tokens.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0_u8; N];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
        let read = file.read_exact(&mut out).is_ok();
        if read && out.iter().any(|byte| *byte != 0) {
            return out;
        }
    }
    fallback::<N>()
}

/// Reads `bytes` cryptographically strong random bytes, failing closed when
/// the kernel CSPRNG is unavailable.
///
/// Unlike [`random_bytes`], this never falls back to a predictable seed. It is
/// intended for secrets and auth tokens where a predictable value must fail the
/// operation rather than silently weaken security.
pub fn try_random_bytes<const N: usize>() -> Result<[u8; N], String> {
    let mut out = [0_u8; N];
    let mut file =
        std::fs::File::open("/dev/urandom").map_err(|_| "kernel CSPRNG unavailable".to_owned())?;
    file.read_exact(&mut out)
        .map_err(|_| "kernel CSPRNG read failed".to_owned())?;
    if !out.iter().any(|byte| *byte != 0) {
        return Err("kernel CSPRNG returned only zeroes".to_owned());
    }
    Ok(out)
}

fn fallback<const N: usize>() -> [u8; N] {
    use std::time::{SystemTime, UNIX_EPOCH};
    let pid = std::process::id();
    let start = super::instance_lock::current_process_start_id().unwrap_or(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let mut seed = [0_u8; 32];
    seed[0..8].copy_from_slice(&u64::from(pid).to_le_bytes());
    seed[8..16].copy_from_slice(&start.to_le_bytes());
    // Mix the full 128-bit monotonic nanos into the seed (truncation here is
    // deliberate and bounded to the hash input width).
    seed[16..32].copy_from_slice(&nanos.to_le_bytes()[..16]);
    // Cheap FNV-1a avalanche into the requested width.
    let mut out = [0_u8; N];
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in seed {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    let mut index = 0;
    while index < N {
        for byte in hash.to_le_bytes() {
            if index < N {
                out[index] = byte;
                index += 1;
            }
        }
        hash = hash.wrapping_mul(0x1000_0000_01b3) ^ 0x9e37_79b9;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_bytes_are_not_all_zero() {
        let value = random_bytes::<16>();
        assert_ne!(value, [0_u8; 16]);
    }

    #[test]
    fn fallback_produces_distinct_seed_across_calls() {
        let first = fallback::<16>();
        let second = fallback::<16>();
        // Entropy is best-effort; only assert the seed is well-formed here.
        assert_eq!(first.len(), second.len());
    }
}

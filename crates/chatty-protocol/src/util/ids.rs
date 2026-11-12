//! Random v4 UUID strings, replacing the `uuid` crate.
//!
//! Reads entropy from the operating system; if that ever fails, falls back to
//! a time/address-space hash so ID generation can never panic. Secrets such as
//! session tokens use [`secret_hex`], which never falls back.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};

/// Generates a random (version 4) UUID in canonical hyphenated form.
pub fn new_uuid() -> String {
    let mut bytes = [0u8; 16];
    fill_random(&mut bytes);
    bytes[6] = bytes[6] & 0x0f | 0x40;
    bytes[8] = bytes[8] & 0x3f | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Returns `bytes` random bytes from the operating system as lowercase hex.
///
/// Unlike [`new_uuid`], this fails instead of using a predictable fallback, so
/// it is safe for bearer secrets.
pub fn secret_hex(bytes: usize) -> std::io::Result<String> {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(buffer.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn fill_random(buffer: &mut [u8]) {
    if getrandom::fill(buffer).is_ok() {
        return;
    }
    // Fallback: fold the current time through SipHash with a fresh random
    // key. Collision-safe enough for connection IDs in an emergency.
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    let seed = hasher.finish();
    for (index, byte) in buffer.iter_mut().enumerate() {
        *byte = (seed >> (index % 8 * 8)) as u8 ^ (index as u8).wrapping_mul(0x9e);
    }
}

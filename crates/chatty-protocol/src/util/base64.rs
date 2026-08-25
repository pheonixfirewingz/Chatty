//! Base64 (RFC 4648 standard alphabet) encode/decode, replacing the
//! `base64` crate. Padding is emitted/required as usual.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn value_of(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some((byte - b'A') as u32),
        b'a'..=b'z' => Some((byte - b'a') as u32 + 26),
        b'0'..=b'9' => Some((byte - b'0') as u32 + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let bytes = [
            chunk[0],
            chunk.get(1).copied().unwrap_or_default(),
            chunk.get(2).copied().unwrap_or_default(),
        ];
        let word = u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]);
        out.push(ALPHABET[(word >> 18 & 63) as usize] as char);
        out.push(ALPHABET[(word >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(word >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(word & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Decodes standard-alphabet base64, ignoring embedded whitespace.
/// Returns `None` on invalid characters or impossible padding.
pub fn decode(input: &str) -> Option<Vec<u8>> {
    let cleaned: Vec<u8> = input
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let padding = cleaned.iter().rev().take_while(|&&b| b == b'=').count();
    if padding > 2 || cleaned.len() % 4 != 0 {
        return None;
    }
    let body = &cleaned[..cleaned.len() - padding];
    let mut out = Vec::with_capacity(body.len() * 3 / 4);
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    for &byte in body {
        accumulator = accumulator << 6 | value_of(byte)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    Some(out)
}

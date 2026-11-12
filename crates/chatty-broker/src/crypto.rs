//! Authenticated encryption for stored character and message images.
use super::*;
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, AeadInPlace, KeyInit, Payload},
};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

const IMAGE_CIPHERTEXT_MAGIC: &[u8] = b"CHIMG1";

pub(crate) fn load_or_create_image_key(tls_key_path: &str) -> Result<[u8; 32]> {
    let path = PathBuf::from(tls_key_path).with_extension("image-key");
    if path.exists() {
        let mut key = [0u8; 32];
        File::open(&path)
            .with_context(|| format!("open image encryption key {}", path.display()))?
            .read_exact(&mut key)
            .with_context(|| format!("read image encryption key {}", path.display()))?;
        return Ok(key);
    }
    let mut key = [0u8; 32];
    getrandom::fill(&mut key)
        .map_err(|error| format_err!("generate image encryption key: {error}"))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    match options.open(&path) {
        Ok(mut file) => file
            .write_all(&key)
            .with_context(|| format!("write image encryption key {}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            File::open(&path)
                .with_context(|| format!("open image encryption key {}", path.display()))?
                .read_exact(&mut key)
                .with_context(|| format!("read image encryption key {}", path.display()))?;
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("create image encryption key {}", path.display()));
        }
    }
    Ok(key)
}

fn encrypt_image_bytes(key: &[u8; 32], aad: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
    let mut nonce = [0u8; 24];
    getrandom::fill(&mut nonce)
        .map_err(|error| format_err!("generate image encryption nonce: {error}"))?;
    let header_len = IMAGE_CIPHERTEXT_MAGIC.len() + nonce.len();
    let mut stored = Vec::with_capacity(header_len + plaintext.len() + 16);
    stored.extend_from_slice(IMAGE_CIPHERTEXT_MAGIC);
    stored.extend_from_slice(&nonce);
    stored.extend_from_slice(plaintext);
    let tag = XChaCha20Poly1305::new(key.into())
        .encrypt_in_place_detached(
            XNonce::from_slice(&nonce),
            aad.as_bytes(),
            &mut stored[header_len..],
        )
        .map_err(|_| format_err!("encrypt image"))?;
    stored.extend_from_slice(&tag);
    Ok(stored)
}

fn decrypt_image_bytes(key: &[u8; 32], aad: &str, stored: &[u8]) -> Result<Vec<u8>> {
    let encrypted = stored
        .strip_prefix(IMAGE_CIPHERTEXT_MAGIC)
        .context("image is not encrypted")?;
    if encrypted.len() < 24 {
        bail!("image ciphertext is truncated")
    }
    let (nonce, ciphertext) = encrypted.split_at(24);
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| format_err!("decrypt image"))
}

pub(crate) fn encrypt_character_images(
    key: &[u8; 32],
    owner_id: &str,
    character_id: &str,
    images: &[CharacterImage],
) -> Result<Vec<u8>> {
    if images.is_empty() {
        return Ok(Vec::new());
    }
    encrypt_image_bytes(
        key,
        &format!("{owner_id}:{character_id}"),
        &encode(&images)?,
    )
}

pub(crate) fn decrypt_character_images(
    key: &[u8; 32],
    owner_id: &str,
    character_id: &str,
    stored: &[u8],
) -> Result<Vec<CharacterImage>> {
    if stored.is_empty() {
        return Ok(Vec::new());
    }
    let plaintext = decrypt_image_bytes(key, &format!("{owner_id}:{character_id}"), stored)
        .context("decrypt character images")?;
    Ok(decode(&plaintext)?)
}

/// Message images use a separate associated-data namespace from character images.
pub(crate) fn encrypt_message_image(
    key: &[u8; 32],
    owner_id: &str,
    message_id: &str,
    png: &[u8],
) -> Result<Vec<u8>> {
    encrypt_image_bytes(key, &format!("message:{owner_id}:{message_id}"), png)
}

pub(crate) fn decrypt_message_image(
    key: &[u8; 32],
    owner_id: &str,
    message_id: &str,
    stored: &[u8],
) -> Result<Vec<u8>> {
    decrypt_image_bytes(key, &format!("message:{owner_id}:{message_id}"), stored)
        .context("decrypt message image")
}

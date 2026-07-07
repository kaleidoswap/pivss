//! Optional client-side encryption helper. PIVSS assumes backups are already
//! encrypted (bring your own ciphertext — e.g. the wallet's VSS envelope) and
//! treats every payload as opaque, so the provider is zero-knowledge by
//! construction. This module is a convenience for clients that don't already
//! encrypt: providers still only ever store opaque ciphertext, and the key
//! never leaves the client.
//!
//! Envelope layout (the bytes uploaded to the server), mirroring the spirit of
//! VSS's `Storable`/`EncryptionMetadata`:
//!
//! ```text
//! magic "PIVSS1" (6) | salt (16) | nonce (12) | AES-256-GCM ciphertext+tag
//! ```
//!
//! Key = Argon2id(passphrase, salt). A fresh random salt+nonce is generated
//! per encryption, so re-encrypting the same plaintext under the same
//! passphrase still produces a distinct envelope — except when the salt+nonce
//! are supplied explicitly (see [`encrypt_with`]), which the client uses to
//! *reproduce* the exact uploaded bytes when answering a proof-of-storage
//! challenge, without keeping a second copy on disk.

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use argon2::Argon2;
use rand::RngCore;

pub const MAGIC: &[u8; 6] = b"PIVSS1";
pub const SALT_LEN: usize = 16;
pub const NONCE_LEN: usize = 12;
pub const CIPHER_FORMAT: &str = "AES-256-GCM/Argon2id";

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("key derivation failed: {0}")]
    Kdf(String),
    #[error("encryption failed")]
    Encrypt,
    #[error("decryption failed (wrong passphrase or corrupt data)")]
    Decrypt,
    #[error("not a PIVSS envelope")]
    BadMagic,
    #[error("truncated envelope")]
    Truncated,
}

/// Derived key + the salt used, so callers can persist the salt for reproduction.
pub struct DerivedKey {
    pub key: [u8; 32],
    pub salt: [u8; SALT_LEN],
}

pub fn derive_key(passphrase: &str, salt: &[u8; SALT_LEN]) -> Result<[u8; 32], CryptoError> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| CryptoError::Kdf(e.to_string()))?;
    Ok(key)
}

fn seal(
    key: &[u8; 32],
    nonce: &[u8; NONCE_LEN],
    salt: &[u8; SALT_LEN],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::Encrypt)?;
    let ct = cipher
        .encrypt(Nonce::from_slice(nonce), plaintext)
        .map_err(|_| CryptoError::Encrypt)?;
    let mut out = Vec::with_capacity(MAGIC.len() + SALT_LEN + NONCE_LEN + ct.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(salt);
    out.extend_from_slice(nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Encrypt with a fresh random salt+nonce. Returns the envelope plus the
/// salt/nonce (hex) so the client can reproduce it later for proofs.
pub fn encrypt(
    passphrase: &str,
    plaintext: &[u8],
) -> Result<(Vec<u8>, String, String), CryptoError> {
    let mut rng = rand::thread_rng();
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    rng.fill_bytes(&mut salt);
    rng.fill_bytes(&mut nonce);
    let key = derive_key(passphrase, &salt)?;
    let envelope = seal(&key, &nonce, &salt, plaintext)?;
    Ok((envelope, hex::encode(salt), hex::encode(nonce)))
}

/// Deterministically reproduce a previously uploaded envelope from local
/// plaintext plus the stored salt+nonce — used at proof/verify time.
pub fn encrypt_with(
    passphrase: &str,
    salt_hex: &str,
    nonce_hex: &str,
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let salt: [u8; SALT_LEN] = hex::decode(salt_hex)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or(CryptoError::Truncated)?;
    let nonce: [u8; NONCE_LEN] = hex::decode(nonce_hex)
        .ok()
        .and_then(|v| v.try_into().ok())
        .ok_or(CryptoError::Truncated)?;
    let key = derive_key(passphrase, &salt)?;
    seal(&key, &nonce, &salt, plaintext)
}

/// Decrypt an envelope produced by [`encrypt`]/[`encrypt_with`].
pub fn decrypt(passphrase: &str, envelope: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let header = MAGIC.len() + SALT_LEN + NONCE_LEN;
    if envelope.len() < header {
        return Err(CryptoError::Truncated);
    }
    if &envelope[..MAGIC.len()] != MAGIC {
        return Err(CryptoError::BadMagic);
    }
    let salt: [u8; SALT_LEN] = envelope[MAGIC.len()..MAGIC.len() + SALT_LEN]
        .try_into()
        .unwrap();
    let nonce = &envelope[MAGIC.len() + SALT_LEN..header];
    let ciphertext = &envelope[header..];
    let key = derive_key(passphrase, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| CryptoError::Decrypt)?;
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| CryptoError::Decrypt)
}

/// True if `data` looks like a PIVSS envelope (used to auto-detect on restore).
pub fn is_envelope(data: &[u8]) -> bool {
    data.len() >= MAGIC.len() && &data[..MAGIC.len()] == MAGIC
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let (env, _, _) = encrypt("correct horse", b"lightning channel state").unwrap();
        assert!(is_envelope(&env));
        assert_ne!(&env[..], b"lightning channel state");
        let out = decrypt("correct horse", &env).unwrap();
        assert_eq!(out, b"lightning channel state");
    }

    #[test]
    fn wrong_passphrase_fails() {
        let (env, _, _) = encrypt("right", b"secret rgb stash").unwrap();
        assert!(matches!(decrypt("wrong", &env), Err(CryptoError::Decrypt)));
    }

    #[test]
    fn reproduction_is_deterministic() {
        let (env, salt, nonce) = encrypt("pw", b"backup bytes").unwrap();
        let reproduced = encrypt_with("pw", &salt, &nonce, b"backup bytes").unwrap();
        assert_eq!(
            env, reproduced,
            "same pw+salt+nonce+plaintext must reproduce the exact envelope"
        );
        // ...but different plaintext under the same salt+nonce diverges.
        let other = encrypt_with("pw", &salt, &nonce, b"tampered bytes").unwrap();
        assert_ne!(env, other);
    }

    #[test]
    fn fresh_salt_nonce_each_time() {
        let (a, _, _) = encrypt("pw", b"x").unwrap();
        let (b, _, _) = encrypt("pw", b"x").unwrap();
        assert_ne!(a, b, "random salt+nonce should make envelopes differ");
    }

    #[test]
    fn rejects_non_envelope() {
        // Long enough to pass the length check, so we hit the magic check.
        let junk = vec![0xabu8; 64];
        assert!(matches!(decrypt("pw", &junk), Err(CryptoError::BadMagic)));
        // Too short: caught by the length check first.
        assert!(matches!(
            decrypt("pw", b"short"),
            Err(CryptoError::Truncated)
        ));
        assert!(!is_envelope(b"nope"));
    }
}

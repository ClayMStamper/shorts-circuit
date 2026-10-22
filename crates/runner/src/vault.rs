//! Envelope encryption for credentials at rest.
//!
//! Every write gets a fresh random 256-bit data key. The plaintext is sealed with AES-256-GCM
//! under that key, bound to a context string (the account it belongs to), and the data key is
//! itself sealed with AES-256-GCM under the master key. Only the wrapped data key and the
//! ciphertext are stored. The master key comes from the environment and is never stored or
//! logged; retired master keys stay usable for reading until [`Vault::rewrap`] has moved
//! every data key under the current one.

use std::fmt;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rand::RngCore;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// The current master key: base64 of 32 random bytes (`shorts-runner generate-key`).
pub const MASTER_KEY_ENV: &str = "SHORTS_RUNNER_MASTER_KEY";
/// Retired master keys, comma-separated, kept while `shorts-runner rotate-keys` runs.
pub const OLD_MASTER_KEYS_ENV: &str = "SHORTS_RUNNER_OLD_MASTER_KEYS";

const NONCE_BYTES: usize = 12;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VaultError {
    #[error("{MASTER_KEY_ENV} is not set; generate one with `shorts-runner generate-key`")]
    MissingKey,
    #[error("a master key must be base64 of exactly 32 bytes")]
    BadKey,
    /// Wrong key, wrong context, or tampered data. Deliberately says nothing more.
    #[error("could not decrypt a stored credential (wrong master key or damaged data)")]
    Decrypt,
    #[error("the credential was sealed with master key {0}, which is not configured")]
    UnknownKey(String),
}

struct MasterKey {
    id: String,
    cipher: Aes256Gcm,
}

impl MasterKey {
    fn parse(encoded: &str) -> Result<Self, VaultError> {
        let bytes = STANDARD
            .decode(encoded.trim())
            .map_err(|_| VaultError::BadKey)?;
        if bytes.len() != 32 {
            return Err(VaultError::BadKey);
        }
        let mut hasher = Sha256::new();
        hasher.update(b"shorts-runner master key id\0");
        hasher.update(&bytes);
        let id = hex::encode(&hasher.finalize()[..8]);
        Ok(Self {
            id,
            cipher: Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&bytes)),
        })
    }
}

/// A sealed value as stored.
#[derive(Clone, PartialEq, Eq)]
pub struct Sealed {
    /// Which master key wrapped the data key.
    pub key_id: String,
    /// Nonce followed by the data key sealed under the master key.
    pub wrapped_key: Vec<u8>,
    /// Nonce followed by the plaintext sealed under the data key.
    pub ciphertext: Vec<u8>,
}

impl fmt::Debug for Sealed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sealed")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

pub struct Vault {
    active: MasterKey,
    retired: Vec<MasterKey>,
}

impl fmt::Debug for Vault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vault")
            .field("active_key", &self.active.id)
            .field("retired_keys", &self.retired.len())
            .finish()
    }
}

fn seal_with(cipher: &Aes256Gcm, plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
    let mut nonce = [0u8; NONCE_BYTES];
    rand::rng().fill_bytes(&mut nonce);
    let sealed = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("AES-GCM sealing does not fail for in-memory buffers");
    let mut out = nonce.to_vec();
    out.extend_from_slice(&sealed);
    out
}

fn open_with(cipher: &Aes256Gcm, sealed: &[u8], aad: &[u8]) -> Result<Vec<u8>, VaultError> {
    if sealed.len() < NONCE_BYTES {
        return Err(VaultError::Decrypt);
    }
    let (nonce, body) = sealed.split_at(NONCE_BYTES);
    cipher
        .decrypt(Nonce::from_slice(nonce), Payload { msg: body, aad })
        .map_err(|_| VaultError::Decrypt)
}

fn wrap_aad(key_id: &str, context: &str) -> Vec<u8> {
    format!("shorts-runner:data-key:{key_id}:{context}").into_bytes()
}

fn data_aad(context: &str) -> Vec<u8> {
    format!("shorts-runner:data:{context}").into_bytes()
}

impl Vault {
    /// `active` and each of `retired` are base64 of 32 bytes.
    pub fn new(active: &str, retired: &[&str]) -> Result<Self, VaultError> {
        Ok(Self {
            active: MasterKey::parse(active)?,
            retired: retired
                .iter()
                .filter(|k| !k.trim().is_empty())
                .map(|k| MasterKey::parse(k))
                .collect::<Result<_, _>>()?,
        })
    }

    /// Reads [`MASTER_KEY_ENV`] and [`OLD_MASTER_KEYS_ENV`].
    pub fn from_env() -> Result<Self, VaultError> {
        let active = std::env::var(MASTER_KEY_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or(VaultError::MissingKey)?;
        let old = std::env::var(OLD_MASTER_KEYS_ENV).unwrap_or_default();
        let old: Vec<&str> = old.split(',').collect();
        Self::new(&active, &old)
    }

    /// A new random master key, base64-encoded.
    pub fn generate_key() -> String {
        let mut key = [0u8; 32];
        rand::rng().fill_bytes(&mut key);
        STANDARD.encode(key)
    }

    pub fn active_key_id(&self) -> &str {
        &self.active.id
    }

    fn key(&self, id: &str) -> Result<&MasterKey, VaultError> {
        std::iter::once(&self.active)
            .chain(&self.retired)
            .find(|k| k.id == id)
            .ok_or_else(|| VaultError::UnknownKey(id.to_owned()))
    }

    /// Seals `plaintext` for `context` (e.g. `credential:<account id>`).
    pub fn seal(&self, plaintext: &[u8], context: &str) -> Sealed {
        let mut data_key = [0u8; 32];
        rand::rng().fill_bytes(&mut data_key);
        let data_cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&data_key));
        let ciphertext = seal_with(&data_cipher, plaintext, &data_aad(context));
        let wrapped_key = seal_with(
            &self.active.cipher,
            &data_key,
            &wrap_aad(&self.active.id, context),
        );
        Sealed {
            key_id: self.active.id.clone(),
            wrapped_key,
            ciphertext,
        }
    }

    fn data_key(&self, sealed: &Sealed, context: &str) -> Result<Vec<u8>, VaultError> {
        let master = self.key(&sealed.key_id)?;
        let key = open_with(
            &master.cipher,
            &sealed.wrapped_key,
            &wrap_aad(&master.id, context),
        )?;
        if key.len() != 32 {
            return Err(VaultError::Decrypt);
        }
        Ok(key)
    }

    pub fn open(&self, sealed: &Sealed, context: &str) -> Result<Vec<u8>, VaultError> {
        let key = self.data_key(sealed, context)?;
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
        open_with(&cipher, &sealed.ciphertext, &data_aad(context))
    }

    /// Re-wraps the data key under the current master key, leaving the ciphertext alone.
    /// `None` when it already uses the current key.
    pub fn rewrap(&self, sealed: &Sealed, context: &str) -> Result<Option<Sealed>, VaultError> {
        if sealed.key_id == self.active.id {
            return Ok(None);
        }
        let key = self.data_key(sealed, context)?;
        Ok(Some(Sealed {
            key_id: self.active.id.clone(),
            wrapped_key: seal_with(
                &self.active.cipher,
                &key,
                &wrap_aad(&self.active.id, context),
            ),
            ciphertext: sealed.ciphertext.clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_values_open_only_with_the_right_key_and_context() {
        let vault = Vault::new(&Vault::generate_key(), &[]).unwrap();
        let sealed = vault.seal(b"fake-refresh-token", "credential:a");
        assert!(
            !sealed
                .ciphertext
                .windows(18)
                .any(|w| w == b"fake-refresh-token"),
            "the plaintext must not appear in the ciphertext"
        );
        assert_eq!(
            vault.open(&sealed, "credential:a").unwrap(),
            b"fake-refresh-token"
        );
        // Moved to another account's row: refused.
        assert_eq!(
            vault.open(&sealed, "credential:b"),
            Err(VaultError::Decrypt)
        );
        // Tampered: refused.
        let mut tampered = sealed.clone();
        *tampered.ciphertext.last_mut().unwrap() ^= 1;
        assert_eq!(
            vault.open(&tampered, "credential:a"),
            Err(VaultError::Decrypt)
        );
        // Another master key: refused.
        let other = Vault::new(&Vault::generate_key(), &[]).unwrap();
        assert!(matches!(
            other.open(&sealed, "credential:a"),
            Err(VaultError::UnknownKey(_))
        ));
        // Each seal uses a fresh data key and nonces.
        let again = vault.seal(b"fake-refresh-token", "credential:a");
        assert_ne!(again.ciphertext, sealed.ciphertext);
        assert_ne!(again.wrapped_key, sealed.wrapped_key);
    }

    #[test]
    fn rotating_the_master_key_rewraps_without_reencrypting() {
        let old_key = Vault::generate_key();
        let old = Vault::new(&old_key, &[]).unwrap();
        let sealed = old.seal(b"secret", "credential:a");

        let new_key = Vault::generate_key();
        let rotating = Vault::new(&new_key, &[old_key.as_str()]).unwrap();
        assert_eq!(rotating.open(&sealed, "credential:a").unwrap(), b"secret");
        let rewrapped = rotating.rewrap(&sealed, "credential:a").unwrap().unwrap();
        assert_eq!(rewrapped.key_id, rotating.active_key_id());
        assert_eq!(rewrapped.ciphertext, sealed.ciphertext);
        assert!(
            rotating
                .rewrap(&rewrapped, "credential:a")
                .unwrap()
                .is_none()
        );

        // Once the old key is dropped, only rewrapped values open.
        let rotated = Vault::new(&new_key, &[]).unwrap();
        assert_eq!(rotated.open(&rewrapped, "credential:a").unwrap(), b"secret");
        assert!(rotated.open(&sealed, "credential:a").is_err());
    }

    #[test]
    fn keys_must_be_32_bytes_and_never_show() {
        assert_eq!(
            Vault::new("dG9vIHNob3J0", &[]).unwrap_err(),
            VaultError::BadKey
        );
        assert_eq!(
            Vault::new("not base64!", &[]).unwrap_err(),
            VaultError::BadKey
        );
        let key = Vault::generate_key();
        let vault = Vault::new(&key, &[]).unwrap();
        let shown = format!("{vault:?}");
        assert!(!shown.contains(&key));
        assert!(shown.contains(vault.active_key_id()));
    }
}

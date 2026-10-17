//! Connector tokens in the OS credential store (Windows Credential Manager, macOS Keychain,
//! Linux kernel keyring).

use shorts_desktop::credentials::{CredentialError, CredentialStore};

const SERVICE: &str = "io.github.claymstamper.shorts-circuit";

pub struct KeyringCredentials;

fn entry(key: &str) -> Result<keyring::Entry, CredentialError> {
    keyring::Entry::new(SERVICE, key).map_err(|e| CredentialError(e.to_string()))
}

impl CredentialStore for KeyringCredentials {
    fn get(&self, key: &str) -> Result<Option<String>, CredentialError> {
        match entry(key)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CredentialError(e.to_string())),
        }
    }

    fn set(&self, key: &str, secret: &str) -> Result<(), CredentialError> {
        entry(key)?
            .set_password(secret)
            .map_err(|e| CredentialError(e.to_string()))
    }

    fn delete(&self, key: &str) -> Result<(), CredentialError> {
        match entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CredentialError(e.to_string())),
        }
    }
}

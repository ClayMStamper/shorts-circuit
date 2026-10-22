//! Where connector tokens live. The desktop shell provides the OS credential store; tests use
//! [`MemoryCredentials`]. Secrets never go into SQLite, logs or views.

use std::collections::HashMap;
use std::sync::Mutex;

use thiserror::Error;

#[derive(Debug, Error)]
#[error("credential store: {0}")]
pub struct CredentialError(pub String);

/// A small secret store keyed by name.
pub trait CredentialStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, CredentialError>;
    fn set(&self, key: &str, secret: &str) -> Result<(), CredentialError>;
    fn delete(&self, key: &str) -> Result<(), CredentialError>;
}

/// In-memory store for tests and for running without an OS keyring.
#[derive(Debug, Default)]
pub struct MemoryCredentials {
    entries: Mutex<HashMap<String, String>>,
}

impl CredentialStore for MemoryCredentials {
    fn get(&self, key: &str) -> Result<Option<String>, CredentialError> {
        Ok(self
            .entries
            .lock()
            .expect("credentials lock")
            .get(key)
            .cloned())
    }

    fn set(&self, key: &str, secret: &str) -> Result<(), CredentialError> {
        self.entries
            .lock()
            .expect("credentials lock")
            .insert(key.to_owned(), secret.to_owned());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), CredentialError> {
        self.entries.lock().expect("credentials lock").remove(key);
        Ok(())
    }
}

use std::collections::HashMap;
use std::sync::Mutex;

use crate::error::Error;
use crate::Result;

pub const SERVICE: &str = "ForgeDownloadManager";

/// Password storage. The database keeps only the `keychain_id`, never the secret.
pub trait SecretStore: Send + Sync {
    fn set_secret(&self, keychain_id: &str, secret: &str) -> Result<()>;
    fn get_secret(&self, keychain_id: &str) -> Result<Option<String>>;
    fn delete_secret(&self, keychain_id: &str) -> Result<()>;
}

/// Test double. Production uses the OS keychain on Windows and macOS.
#[derive(Default)]
pub struct MemorySecrets {
    inner: Mutex<HashMap<String, String>>,
}

impl MemorySecrets {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemorySecrets {
    fn set_secret(&self, keychain_id: &str, secret: &str) -> Result<()> {
        lock(&self.inner).insert(keychain_id.to_string(), secret.to_string());
        Ok(())
    }

    fn get_secret(&self, keychain_id: &str) -> Result<Option<String>> {
        Ok(lock(&self.inner).get(keychain_id).cloned())
    }

    fn delete_secret(&self, keychain_id: &str) -> Result<()> {
        lock(&self.inner).remove(keychain_id);
        Ok(())
    }
}

/// OS keychain. Linux builds return an error: secret-service depends on dbus, which is AFL/GPL.
pub struct KeyringSecrets;

impl SecretStore for KeyringSecrets {
    fn set_secret(&self, keychain_id: &str, secret: &str) -> Result<()> {
        native_set(keychain_id, secret)
    }

    fn get_secret(&self, keychain_id: &str) -> Result<Option<String>> {
        native_get(keychain_id)
    }

    fn delete_secret(&self, keychain_id: &str) -> Result<()> {
        native_delete(keychain_id)
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn native_set(keychain_id: &str, secret: &str) -> Result<()> {
    let entry = keyring::Entry::new(SERVICE, keychain_id).map_err(keyring_err)?;
    entry.set_password(secret).map_err(keyring_err)?;
    Ok(())
}

#[cfg(any(windows, target_os = "macos"))]
fn native_get(keychain_id: &str) -> Result<Option<String>> {
    let entry = keyring::Entry::new(SERVICE, keychain_id).map_err(keyring_err)?;
    match entry.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(keyring_err(err)),
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn native_delete(keychain_id: &str) -> Result<()> {
    let entry = keyring::Entry::new(SERVICE, keychain_id).map_err(keyring_err)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(keyring_err(err)),
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn keyring_err(err: keyring::Error) -> Error {
    Error::Failed(format!("keychain: {err}"))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn native_set(_keychain_id: &str, _secret: &str) -> Result<()> {
    Err(linux_gap())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn native_get(_keychain_id: &str) -> Result<Option<String>> {
    Err(linux_gap())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn native_delete(_keychain_id: &str) -> Result<()> {
    Err(linux_gap())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn linux_gap() -> Error {
    Error::Failed(
        "this build has no OS keychain; the Linux secret-service crate depends on dbus (AFL/GPL), which this MIT app does not link"
            .into(),
    )
}

fn lock(map: &Mutex<HashMap<String, String>>) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
    map.lock().unwrap_or_else(|err| err.into_inner())
}

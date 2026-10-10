//! Secret storage.
//!
//! Two backends, chosen by the `secretStore` setting:
//!
//! * **file** (default) — a `0600` JSON file in the app's data directory. No
//!   prompts, works identically in dev and release. It is *not* encrypted: any
//!   process running as this user can read it, exactly like `~/.aws/credentials`
//!   or a `.npmrc`. The file mode keeps it out of other accounts and out of
//!   world-readable backups; that is the whole of its protection, and the
//!   settings screen says so.
//!
//! * **keychain** — the OS credential store, which does protect the value at
//!   rest. macOS binds its ACLs to a binary's code signature, so unsigned dev
//!   builds re-prompt on every rebuild; a signed release build asks once.
//!
//! Switching backends migrates the existing value across, so the key is never
//! entered twice.

use crate::error::{AppError, Result};
use keyring::Entry;
use parking_lot::RwLock;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

const SERVICE: &str = "com.strawberry.agent";
pub const OPENROUTER_KEY: &str = "openrouter-api-key";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    File,
    Keychain,
}

impl Backend {
    pub fn parse(s: &str) -> Self {
        if s == "keychain" {
            Self::Keychain
        } else {
            Self::File
        }
    }
}

static BACKEND: RwLock<Backend> = RwLock::new(Backend::File);

pub fn backend() -> Backend {
    *BACKEND.read()
}

/// Switches backend, moving any stored secrets across so nothing is lost.
pub fn set_backend(next: Backend) {
    let current = backend();
    if current == next {
        return;
    }
    let carried: BTreeMap<String, String> = known_accounts()
        .into_iter()
        .filter_map(|a| read_from(current, &a).ok().flatten().map(|v| (a, v)))
        .collect();

    *BACKEND.write() = next;

    for (account, value) in carried {
        if write_to(next, &account, &value).is_ok() {
            let _ = clear_from(current, &account);
        }
    }
}

/// One-time adoption for installs that predate the file store.
///
/// If the active backend has no API key but the keychain does, bring it across
/// so the user is not asked to paste it again. Best effort: a locked or
/// unavailable keychain simply means there is nothing to adopt.
pub fn adopt_legacy_keychain_key() {
    if backend() == Backend::Keychain {
        return;
    }
    if matches!(read_from(Backend::File, OPENROUTER_KEY), Ok(Some(_))) {
        return;
    }
    if let Ok(Some(existing)) = read_from(Backend::Keychain, OPENROUTER_KEY) {
        if write_to(Backend::File, OPENROUTER_KEY, &existing).is_ok() {
            let _ = clear_from(Backend::Keychain, OPENROUTER_KEY);
        }
    }
}

/// Accounts worth migrating: the API key plus every saved SSH credential.
fn known_accounts() -> Vec<String> {
    let mut out = vec![OPENROUTER_KEY.to_string()];
    if let Ok(map) = read_file_map() {
        out.extend(map.keys().filter(|k| *k != OPENROUTER_KEY).cloned());
    }
    out
}

pub fn set(account: &str, secret: &str) -> Result<()> {
    write_to(backend(), account, secret)
}

pub fn get(account: &str) -> Result<Option<String>> {
    read_from(backend(), account)
}

pub fn delete(account: &str) -> Result<()> {
    clear_from(backend(), account)
}

// ------------------------------------------------------------------ backends

fn write_to(backend: Backend, account: &str, secret: &str) -> Result<()> {
    match backend {
        Backend::File => {
            let mut map = read_file_map()?;
            map.insert(account.to_string(), Value::String(secret.to_string()));
            write_file_map(&map)
        }
        Backend::Keychain => entry(account)?
            .set_password(secret)
            .map_err(|e| AppError::msg(format!("could not save to the keychain: {e}"))),
    }
}

fn read_from(backend: Backend, account: &str) -> Result<Option<String>> {
    match backend {
        Backend::File => Ok(read_file_map()?.get(account).and_then(Value::as_str).map(str::to_owned)),
        Backend::Keychain => match entry(account)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(AppError::msg(format!("could not read the keychain: {e}"))),
        },
    }
}

fn clear_from(backend: Backend, account: &str) -> Result<()> {
    match backend {
        Backend::File => {
            let mut map = read_file_map()?;
            map.remove(account);
            write_file_map(&map)
        }
        Backend::Keychain => match entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(AppError::msg(format!("could not clear the keychain: {e}"))),
        },
    }
}

fn entry(account: &str) -> Result<Entry> {
    Entry::new(SERVICE, account).map_err(|e| AppError::msg(format!("keychain unavailable: {e}")))
}

// --------------------------------------------------------------- file store

pub fn credentials_path() -> PathBuf {
    crate::db::data_dir().join("credentials.json")
}

fn read_file_map() -> Result<Map<String, Value>> {
    let path = credentials_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(serde_json::from_str(&text).unwrap_or_default()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(AppError::msg(format!("could not read {}: {e}", path.display()))),
    }
}

fn write_file_map(map: &Map<String, Value>) -> Result<()> {
    let path = credentials_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Write to a temporary file and rename, so a crash mid-write cannot leave a
    // truncated credentials file behind.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(map)?)?;
    restrict(&tmp)?;
    std::fs::rename(&tmp, &path)?;
    restrict(&path)
}

/// Owner read/write only.
fn restrict(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

// ------------------------------------------------------------------ helpers

pub fn ssh_account(host_id: &str) -> String {
    format!("ssh:{host_id}")
}

/// `sk-or-v1-…abcd` — enough to recognise which key is installed, not enough to
/// use it.
pub fn mask(key: &str) -> String {
    let n = key.chars().count();
    if n <= 12 {
        return "•".repeat(n.max(4));
    }
    let head: String = key.chars().take(8).collect();
    let tail: String = key.chars().skip(n - 4).collect();
    format!("{head}…{tail}")
}

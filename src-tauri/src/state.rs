//! Shared application state, owned by Tauri and handed to every command.

use crate::agent::RunRegistry;
use crate::db::Db;
use crate::error::{AppError, Result};
use crate::openrouter;
use crate::secrets;
use crate::settings::Settings;
use crate::terminal::TerminalManager;
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub struct AppState {
    pub db: Arc<Db>,
    pub settings: RwLock<Settings>,
    pub client: RwLock<openrouter::Client>,
    pub runs: RunRegistry,
    pub terminals: TerminalManager,
    pub shells: crate::shells::BackgroundShells,
    /// In-flight architecture scans, keyed by chat id, so a second click
    /// cancels-and-restarts instead of stacking work. The generation number
    /// lets a finished scan remove only its own entry.
    pub analysis_runs: Mutex<HashMap<String, (u64, CancellationToken)>>,
    /// Live SSH sessions for remote workspaces, one per host, shared by tools,
    /// the file browser and commands. See `remote::pooled`.
    pub remotes: tokio::sync::Mutex<HashMap<String, std::sync::Arc<crate::remote::Remote>>>,
    /// The keychain read can show a system prompt and take tens of
    /// milliseconds, so the key is cached for the process lifetime and
    /// invalidated explicitly when the user changes it.
    api_key: RwLock<Option<Option<String>>>,
}

impl AppState {
    pub fn new(db: Db) -> Result<Self> {
        let db = Arc::new(db);
        let settings = crate::settings::load(&db);
        crate::settings::remap_chat_models(&db, &settings);
        // Adopt the configured backend before anything reads a secret. A first
        // run with no file store still finds a key left in the keychain by an
        // earlier version, and moves it across.
        secrets::set_backend(secrets::Backend::parse(&settings.secret_store));
        secrets::adopt_legacy_keychain_key();
        let client = openrouter::Client::new(settings.connect_timeout_ms, settings.request_timeout_ms)?
            .with_attribution(settings.send_attribution);
        Ok(Self {
            db,
            settings: RwLock::new(settings),
            client: RwLock::new(client),
            runs: RunRegistry::default(),
            terminals: TerminalManager::default(),
            shells: crate::shells::BackgroundShells::default(),
            analysis_runs: Mutex::new(HashMap::new()),
            remotes: tokio::sync::Mutex::new(HashMap::new()),
            api_key: RwLock::new(None),
        })
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().clone()
    }

    pub fn client(&self) -> openrouter::Client {
        self.client.read().clone()
    }

    /// Rebuilds the HTTP client when a setting that affects it changes.
    pub fn apply_settings(&self, next: Settings) -> Result<()> {
        let rebuild = {
            let cur = self.settings.read();
            cur.connect_timeout_ms != next.connect_timeout_ms
                || cur.request_timeout_ms != next.request_timeout_ms
                || cur.send_attribution != next.send_attribution
        };
        if next.secret_store != self.settings.read().secret_store {
            secrets::set_backend(secrets::Backend::parse(&next.secret_store));
            self.invalidate_api_key();
        }
        if rebuild {
            let c = openrouter::Client::new(next.connect_timeout_ms, next.request_timeout_ms)?
                .with_attribution(next.send_attribution);
            *self.client.write() = c;
        }
        crate::settings::save(&self.db, &next)?;
        *self.settings.write() = next;
        Ok(())
    }

    pub fn api_key(&self) -> Result<String> {
        if let Some(cached) = self.api_key.read().clone() {
            return cached.ok_or(AppError::MissingApiKey);
        }
        let fetched = secrets::get(secrets::OPENROUTER_KEY)?;
        *self.api_key.write() = Some(fetched.clone());
        fetched.ok_or(AppError::MissingApiKey)
    }

    pub fn invalidate_api_key(&self) {
        *self.api_key.write() = None;
    }

    pub fn has_api_key(&self) -> bool {
        self.api_key().is_ok()
    }
}

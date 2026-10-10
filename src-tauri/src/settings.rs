//! User settings.
//!
//! Persisted as one JSON blob in the `meta` table. Every field has a default so
//! upgrading the app never leaves a user with a half-populated settings object.

use crate::db::Db;
use crate::error::Result;
use serde::{Deserialize, Serialize};

fn t_true() -> bool {
    true
}
fn coalesce_ms() -> u64 {
    16
}
fn max_steps() -> u32 {
    40
}
fn request_timeout() -> u64 {
    300_000
}
fn connect_timeout() -> u64 {
    15_000
}
fn tool_timeout() -> u64 {
    120_000
}
fn history_limit() -> u32 {
    200
}
fn default_model() -> String {
    "z-ai/glm-5.3-flash".into()
}
/// Tried in order if the primary model is unavailable, so a fresh install
/// always has somewhere to land.
fn default_fallbacks() -> Vec<String> {
    vec!["anthropic/claude-sonnet-5".into(), "openai/gpt-5".into()]
}
fn theme() -> String {
    "system".into()
}
fn density() -> String {
    "comfortable".into()
}
fn accent() -> String {
    "strawberry".into()
}
fn zoom() -> f32 {
    1.0
}
fn ui_font_size() -> u32 {
    14
}
fn content_width() -> u32 {
    820
}
fn auto_compact_pct() -> u32 {
    80
}
fn term_font_size() -> u32 {
    12
}
fn term_font() -> String {
    "ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, 'Cascadia Mono', monospace".into()
}
fn max_upload_mb() -> u64 {
    32
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    // ---- appearance
    pub theme: String,
    pub accent: String,
    pub density: String,
    pub ui_font_size: u32,
    /// Whole-interface scale, driven by the zoom shortcuts.
    pub zoom: f32,
    pub reduce_motion: bool,
    /// Max reading width of the chat column, px. Notion-style margins grow
    /// around it on wide windows.
    pub content_width: u32,

    // ---- model
    pub default_model: String,
    pub fallback_models: Vec<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub top_p: Option<f32>,
    pub reasoning_effort: Option<String>,
    pub show_reasoning: bool,
    pub system_prompt: Option<String>,

    // ---- agent loop
    pub tools_enabled: bool,
    pub enabled_tools: Vec<String>,
    pub max_steps: u32,
    /// `readOnly` | `ask` | `acceptEdits` | `auto`. See [`PermissionMode`].
    pub permission_mode: String,
    /// Confine file tools to the workspace roots. On by default: an agent that
    /// can write anywhere on the disk is not a tool anyone should ship.
    pub sandbox_files: bool,
    /// Best-effort OS-level confinement for `run_command`.
    pub sandbox_commands: bool,
    pub allowed_roots: Vec<String>,
    /// Always refused, sandbox or not.
    pub denied_paths: Vec<String>,
    pub tool_timeout_ms: u64,
    /// Fold older history into a rolling summary automatically once a run's
    /// prompt crosses this share of the model's context window. 0 disables.
    pub auto_compact_pct: u32,

    // ---- network
    pub request_timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub max_retries: u32,
    /// Trims context aggressively and skips background refreshes. Meant for
    /// tethered / high-latency connections.
    pub low_bandwidth: bool,
    pub send_attribution: bool,
    pub history_limit: u32,

    // ---- streaming / rendering
    pub stream_coalesce_ms: u64,

    // ---- terminal
    pub shell: Option<String>,
    pub terminal_font_family: String,
    pub terminal_font_size: u32,
    pub terminal_scrollback: u32,

    // ---- files
    pub max_upload_mb: u64,

    /// `file` (default) or `keychain`. See [`crate::secrets`] for the trade-off.
    pub secret_store: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: theme(),
            accent: accent(),
            density: density(),
            ui_font_size: ui_font_size(),
            zoom: zoom(),
            reduce_motion: false,
            content_width: content_width(),

            default_model: default_model(),
            fallback_models: default_fallbacks(),
            temperature: None,
            max_tokens: None,
            top_p: None,
            reasoning_effort: None,
            show_reasoning: true,
            system_prompt: None,

            tools_enabled: t_true(),
            enabled_tools: Vec::new(), // empty = all registered tools
            max_steps: max_steps(),
            permission_mode: "acceptEdits".into(),
            sandbox_files: t_true(),
            sandbox_commands: false,
            allowed_roots: Vec::new(),
            denied_paths: default_denied(),
            tool_timeout_ms: tool_timeout(),
            auto_compact_pct: auto_compact_pct(),

            request_timeout_ms: request_timeout(),
            connect_timeout_ms: connect_timeout(),
            max_retries: 3,
            low_bandwidth: false,
            send_attribution: true,
            history_limit: history_limit(),

            stream_coalesce_ms: coalesce_ms(),

            shell: None,
            terminal_font_family: term_font(),
            terminal_font_size: term_font_size(),
            terminal_scrollback: 5000,

            max_upload_mb: max_upload_mb(),
            secret_store: "file".into(),
        }
    }
}

/// Directories that hold credentials and are never a legitimate target for an
/// agent edit. Refused regardless of sandbox settings or permission mode.
fn default_denied() -> Vec<String> {
    [
        "~/.ssh",
        "~/.aws",
        "~/.gnupg",
        "~/.kube",
        "~/.docker/config.json",
        "~/.config/gh",
        "~/.netrc",
        "~/.npmrc",
        "~/.pypirc",
        "~/Library/Keychains",
        "~/.local/share/keyrings",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

/// How much the agent may do without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    /// Inspect only. Writes, commands and network calls are refused outright.
    ReadOnly,
    /// Everything that changes something needs a click.
    Ask,
    /// File edits go through; commands and network calls still ask.
    AcceptEdits,
    /// Nothing asks. Sandbox limits still apply.
    Auto,
}

impl Settings {
    pub fn mode(&self) -> PermissionMode {
        match self.permission_mode.as_str() {
            "readOnly" | "plan" => PermissionMode::ReadOnly,
            "ask" => PermissionMode::Ask,
            "auto" => PermissionMode::Auto,
            _ => PermissionMode::AcceptEdits,
        }
    }
}

const KEY: &str = "settings";
const MIGRATION_KEY: &str = "settings_migration";
const MIGRATION_LEVEL: i64 = 3;

/// Defaults that have shipped before. A stored value matching one of these was
/// never chosen by the user, so it is safe to move forward; anything else is
/// left alone.
const SUPERSEDED_DEFAULT_MODELS: [&str; 2] = ["anthropic/claude-sonnet-5", "stealth/ox-alpha"];

pub fn load(db: &Db) -> Settings {
    // A corrupt or partial blob must never stop the app from starting.
    let mut settings: Settings =
        db.get_meta(KEY).ok().flatten().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();

    if migrate(db, &mut settings) {
        let _ = save(db, &settings);
    }
    settings
}

/// Brings a stored settings blob forward. Returns true if anything changed.
///
/// Only values that still match a previously shipped default are touched — a
/// migration must never overwrite a choice the user actually made.
fn migrate(db: &Db, settings: &mut Settings) -> bool {
    let level: i64 = db.get_meta(MIGRATION_KEY).ok().flatten().and_then(|v| v.parse().ok()).unwrap_or(0);
    if level >= MIGRATION_LEVEL {
        return false;
    }

    // Level 2: catalogues cached before the wire-format fix stored empty
    // capability lists, which made every model look tool-incapable.
    if level < 2 {
        let _ = db.clear_model_cache();
    }

    let mut changed = false;
    if SUPERSEDED_DEFAULT_MODELS.contains(&settings.default_model.as_str()) {
        settings.default_model = default_model();
        changed = true;
    }
    if settings.fallback_models.is_empty() {
        settings.fallback_models = default_fallbacks();
        changed = true;
    }

    // Level 3: the terminal font shipped at 13px, oversized next to the 11.5px
    // UI. Only the untouched default moves; a chosen size stays.
    if level < 3 && settings.terminal_font_size == 13 {
        settings.terminal_font_size = 12;
        changed = true;
    }

    let _ = db.set_meta(MIGRATION_KEY, &MIGRATION_LEVEL.to_string());
    changed
}

/// Applies the model migration to existing chats. Best effort: a failure here
/// only means a chat keeps an older model, which is harmless.
pub fn remap_chat_models(db: &Db, settings: &Settings) {
    if settings.default_model != default_model() {
        return;
    }
    let _ = db.remap_default_model(&SUPERSEDED_DEFAULT_MODELS, &settings.default_model);
}

pub fn save(db: &Db, s: &Settings) -> Result<()> {
    db.set_meta(KEY, &serde_json::to_string(s)?)
}

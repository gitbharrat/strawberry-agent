//! Tool registry.
//!
//! A harness is only as good as its tools. Each one is a plain async function
//! plus a JSON-Schema spec that goes straight into the OpenRouter `tools` array,
//! and every one of them is cancellable and deadline-bounded — a tool can fail,
//! but it can never wedge a run.

pub mod capture;
pub mod context;
pub mod docker;
pub mod fs_tools;
pub mod git;
pub mod interact;
pub mod memory;
pub mod net;
pub mod remote_fs;
pub mod research;
pub mod search;
pub mod session_tools;
pub mod shell;
pub mod subagent;
pub mod tmux_tools;
pub mod web;

use crate::db::Db;
use crate::error::{AppError, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Drives the approval prompt. Read-only tools are safe to run unattended;
/// anything that mutates the disk, spawns a process or leaves the machine gets
/// gated by policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Read,
    Write,
    Exec,
    Network,
}

pub struct ToolCtx {
    pub db: Arc<Db>,
    pub cwd: PathBuf,
    /// When set, this chat's workspace lives on another machine: file tools go
    /// through SFTP and commands execute over SSH, both on this connection.
    pub remote: Option<Arc<crate::remote::Remote>>,
    /// Empty means unconfined. Non-empty confines every path argument.
    pub allowed_roots: Vec<PathBuf>,
    /// Always refused, even when `allowed_roots` is empty.
    pub denied_paths: Vec<PathBuf>,
    pub cancel: CancellationToken,
    pub timeout: Duration,
    pub sandbox_commands: bool,
    pub handle: std::sync::Arc<crate::agent::RunHandle>,
    pub state: std::sync::Arc<crate::state::AppState>,
    pub settings: crate::settings::Settings,
    // Provenance, so a write can be attributed and later reverted.
    pub chat_id: String,
    pub run_id: String,
    pub call_id: String,
    /// The user explicitly approved this specific call touching a protected
    /// path, so the deny list steps aside for it — and only for it.
    pub allow_denied: bool,
}

/// The parts of a `ToolCtx` that are shared by every call in a run. A fresh
/// `ToolCtx` is built per call from this plus the call id.
#[derive(Clone)]
pub struct ToolEnv {
    pub db: Arc<Db>,
    pub cwd: PathBuf,
    pub remote: Option<Arc<crate::remote::Remote>>,
    pub allowed_roots: Vec<PathBuf>,
    pub denied_paths: Vec<PathBuf>,
    pub cancel: CancellationToken,
    pub timeout: Duration,
    pub sandbox_commands: bool,
    pub handle: std::sync::Arc<crate::agent::RunHandle>,
    pub state: std::sync::Arc<crate::state::AppState>,
    pub settings: crate::settings::Settings,
    pub chat_id: String,
    pub run_id: String,
}

impl ToolEnv {
    /// `allow_denied` is set only after the user approved this exact call in
    /// the protected-path prompt.
    pub fn ctx(&self, call_id: &str, allow_denied: bool) -> Arc<ToolCtx> {
        Arc::new(ToolCtx {
            db: self.db.clone(),
            cwd: self.cwd.clone(),
            remote: self.remote.clone(),
            allowed_roots: self.allowed_roots.clone(),
            denied_paths: self.denied_paths.clone(),
            cancel: self.cancel.clone(),
            timeout: self.timeout,
            sandbox_commands: self.sandbox_commands,
            handle: self.handle.clone(),
            state: self.state.clone(),
            settings: self.settings.clone(),
            chat_id: self.chat_id.clone(),
            run_id: self.run_id.clone(),
            call_id: call_id.to_string(),
            allow_denied,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutput {
    /// Text handed back to the model.
    pub content: String,
    /// Optional structured payload for rich rendering in the UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<Value>,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn text(s: impl Into<String>) -> Self {
        Self { content: s.into(), display: None, is_error: false }
    }
    pub fn with_display(mut self, d: Value) -> Self {
        self.display = Some(d);
        self
    }
    pub fn error(s: impl Into<String>) -> Self {
        Self { content: s.into(), display: None, is_error: true }
    }
}

type Fut = Pin<Box<dyn Future<Output = Result<ToolOutput>> + Send>>;
type Handler = fn(Arc<ToolCtx>, Value) -> Fut;

pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub risk: Risk,
    /// Exempt from the tool deadline. Only for tools whose whole job is to
    /// wait — on the user, or on a long-running session.
    pub unbounded: bool,
    pub parameters: fn() -> Value,
    pub handler: Handler,
}

impl ToolDef {
    /// OpenAI/OpenRouter `tools[]` entry.
    pub fn to_spec(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": (self.parameters)(),
            }
        })
    }
}

/// Wraps a plain async fn so it can be stored as a function pointer.
macro_rules! tool {
    // Waiting tools opt out of the deadline by passing `true` before the desc.
    ($name:literal, $risk:expr, $unbounded:literal, $desc:literal, $params:expr, $f:path) => {
        ToolDef {
            name: $name,
            description: $desc,
            risk: $risk,
            unbounded: $unbounded,
            parameters: || $params,
            handler: |ctx, args| Box::pin($f(ctx, args)),
        }
    };
    ($name:literal, $risk:expr, $desc:literal, $params:expr, $f:path) => {
        tool!($name, $risk, false, $desc, $params, $f)
    };
}

pub fn registry() -> &'static [ToolDef] {
    use context as ctxt;
    use docker as dk;
    use fs_tools as f;
    use git as g;
    use interact as ia;
    use memory as mem;
    use net as n;
    use search as s;
    use session_tools as st;
    use shell as sh;
    use subagent as sa;
    use tmux_tools as tm;

    static TOOLS: std::sync::OnceLock<Vec<ToolDef>> = std::sync::OnceLock::new();
    TOOLS.get_or_init(|| {
        vec![
            tool!(
                "read_file",
                Risk::Read,
                "Read a file from disk. Returns the contents with 1-based line numbers. Use `offset` and `limit` to page through large files instead of reading them whole.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Absolute path, or relative to the working directory." },
                        "offset": { "type": "integer", "description": "1-based line to start from." },
                        "limit": { "type": "integer", "description": "Maximum number of lines to return. Defaults to 2000." }
                    },
                    "required": ["path"]
                }),
                f::read_file
            ),
            tool!(
                "write_file",
                Risk::Write,
                "Create a file or replace its entire contents. Parent directories are created as needed. Prefer `edit_file` for changes to an existing file.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }),
                f::write_file
            ),
            tool!(
                "edit_file",
                Risk::Write,
                "Replace an exact string in a file. `old_string` must match the file byte-for-byte and must be unique unless `replace_all` is true. Returns a unified diff of what changed.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "old_string": { "type": "string" },
                        "new_string": { "type": "string" },
                        "replace_all": { "type": "boolean", "description": "Replace every occurrence instead of requiring a unique match." }
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
                f::edit_file
            ),
            tool!(
                "list_dir",
                Risk::Read,
                "List the entries of a directory. Respects .gitignore. Use this to orient yourself before reading files.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "depth": { "type": "integer", "description": "Recursion depth, 1-5. Defaults to 1." }
                    }
                }),
                f::list_dir
            ),
            tool!(
                "glob",
                Risk::Read,
                "Find files by glob pattern (for example `src/**/*.rs`), newest first. Respects .gitignore.",
                json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string" },
                        "path": { "type": "string", "description": "Directory to search in. Defaults to the working directory." }
                    },
                    "required": ["pattern"]
                }),
                s::glob_files
            ),
            tool!(
                "grep",
                Risk::Read,
                "Search file contents with a regular expression, using the ripgrep engine. Respects .gitignore. Much faster than reading files to look for something.",
                json!({
                    "type": "object",
                    "properties": {
                        "pattern": { "type": "string", "description": "Rust regex syntax." },
                        "path": { "type": "string" },
                        "glob": { "type": "string", "description": "Only search files matching this glob, e.g. `*.ts`." },
                        "case_insensitive": { "type": "boolean" },
                        "max_results": { "type": "integer", "description": "Defaults to 200." }
                    },
                    "required": ["pattern"]
                }),
                s::grep
            ),
            tool!(
                "run_command",
                Risk::Exec,
                "Run a shell command in the working directory and return its combined output and exit code. Long-running or interactive commands are not appropriate here — use the terminal for those.",
                json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "cwd": { "type": "string" },
                        "timeout_ms": { "type": "integer", "description": "Defaults to the configured tool timeout." }
                    },
                    "required": ["command"]
                }),
                sh::run_command
            ),
            tool!(
                "run_python",
                Risk::Exec,
                "Execute a Python snippet with the workspace's python3 (or python) and return its output — for calculations, data wrangling, and quick scripts. The code runs as-is from stdin; print what you need back.",
                json!({
                    "type": "object",
                    "properties": {
                        "code": { "type": "string", "description": "Python source to execute." },
                        "cwd": { "type": "string" },
                        "timeout_ms": { "type": "integer", "description": "Defaults to the configured tool timeout." }
                    },
                    "required": ["code"]
                }),
                sh::run_python
            ),
            tool!(
                "ssh_exec",
                Risk::Exec,
                "Run a command on a configured remote SSH host and return its output. Use `list_ssh_hosts` first to discover host ids.",
                json!({
                    "type": "object",
                    "properties": {
                        "host_id": { "type": "string", "description": "Id of a saved SSH host." },
                        "command": { "type": "string" }
                    },
                    "required": ["host_id", "command"]
                }),
                sh::ssh_exec
            ),
            tool!(
                "list_ssh_hosts",
                Risk::Read,
                "List the SSH hosts the user has configured, with their ids.",
                json!({ "type": "object", "properties": {} }),
                sh::list_ssh_hosts
            ),
            tool!(
                "ssh_download",
                Risk::Write,
                "Download a file from a configured SSH host to the local workspace over SFTP (pipelined, fast). Use `list_ssh_hosts` first to discover host ids.",
                json!({
                    "type": "object",
                    "properties": {
                        "host_id": { "type": "string", "description": "Id of a saved SSH host." },
                        "remote_path": { "type": "string", "description": "Absolute path on the remote machine." },
                        "local_path": { "type": "string", "description": "Where to save it locally. Parent directories are created." }
                    },
                    "required": ["host_id", "remote_path", "local_path"]
                }),
                sh::ssh_download
            ),
            tool!(
                "ssh_upload",
                Risk::Network,
                "Upload a local file to a configured SSH host over SFTP (pipelined, fast). The remote file is created or replaced.",
                json!({
                    "type": "object",
                    "properties": {
                        "host_id": { "type": "string", "description": "Id of a saved SSH host." },
                        "local_path": { "type": "string", "description": "File to send, absolute or relative to the working directory." },
                        "remote_path": { "type": "string", "description": "Destination path on the remote machine." }
                    },
                    "required": ["host_id", "local_path", "remote_path"]
                }),
                sh::ssh_upload
            ),
            tool!(
                "docker_ps",
                Risk::Read,
                "List Docker containers (running and stopped) with names, images, status and ports.",
                json!({
                    "type": "object",
                    "properties": {
                        "all": { "type": "boolean", "description": "Include stopped containers. Defaults to true." }
                    }
                }),
                dk::docker_ps
            ),
            tool!(
                "docker_logs",
                Risk::Read,
                "Read recent logs from a Docker container, with timestamps.",
                json!({
                    "type": "object",
                    "properties": {
                        "container": { "type": "string", "description": "Container name or id." },
                        "tail": { "type": "integer", "description": "How many trailing lines, 1-5000. Defaults to 120." },
                        "since": { "type": "string", "description": "Only logs after this, e.g. `10m` or an RFC3339 timestamp." }
                    },
                    "required": ["container"]
                }),
                dk::docker_logs
            ),
            tool!(
                "docker_exec",
                Risk::Exec,
                "Run a command inside a running Docker container (via /bin/sh -c) and return its output.",
                json!({
                    "type": "object",
                    "properties": {
                        "container": { "type": "string", "description": "Container name or id." },
                        "command": { "type": "string" }
                    },
                    "required": ["container", "command"]
                }),
                dk::docker_exec
            ),
            tool!(
                "docker_inspect",
                Risk::Read,
                "Inspect a Docker container: image, state, command, env, ports, mounts and networks as a compact summary. Set `full` for the raw JSON.",
                json!({
                    "type": "object",
                    "properties": {
                        "target": { "type": "string", "description": "Container (or image) name or id." },
                        "full": { "type": "boolean", "description": "Return the complete `docker inspect` JSON instead of the summary." }
                    },
                    "required": ["target"]
                }),
                dk::docker_inspect
            ),
            tool!(
                "shell_start",
                Risk::Exec,
                "Start a long-running command in a background session — a dev server, watcher, REPL or slow build. Returns a session_id. Pair with shell_read (which can wait for a pattern) and shell_send.",
                json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Shell command to run. Keeps running after this call returns." }
                    },
                    "required": ["command"]
                }),
                st::shell_start
            ),
            tool!(
                "shell_read",
                Risk::Read,
                true,
                "Read new output from a background session, starting at `cursor`. With `wait_for` (a regex) and `wait_ms`, blocks until the pattern appears, the process exits, or the wait elapses — use this to wait for a server to be ready or a build to finish.",
                json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "cursor": { "type": "integer", "description": "Byte offset from a previous read. Start at 0." },
                        "wait_for": { "type": "string", "description": "Regex to wait for in fresh output." },
                        "wait_ms": { "type": "integer", "description": "How long to wait, in milliseconds. Defaults to 0 (return immediately)." }
                    },
                    "required": ["session_id"]
                }),
                st::shell_read
            ),
            tool!(
                "shell_send",
                Risk::Exec,
                "Send input to a background session — a command at a prompt, an answer to an interactive question, or a control sequence such as Ctrl-C.",
                json!({
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "input": { "type": "string" },
                        "press_enter": { "type": "boolean", "description": "Append a newline. Defaults to true." }
                    },
                    "required": ["session_id", "input"]
                }),
                st::shell_send
            ),
            tool!(
                "shell_kill",
                Risk::Exec,
                "Terminate a background session.",
                json!({
                    "type": "object",
                    "properties": { "session_id": { "type": "string" } },
                    "required": ["session_id"]
                }),
                st::shell_kill
            ),
            tool!(
                "shell_list",
                Risk::Read,
                "List background sessions with their ids and state.",
                json!({ "type": "object", "properties": {} }),
                st::shell_list
            ),
            tool!(
                "tmux_list",
                Risk::Read,
                "List tmux sessions, windows and panes with their targets. Works on sessions the user already has open, including ones they are attached to.",
                json!({ "type": "object", "properties": {} }),
                tm::tmux_list
            ),
            tool!(
                "tmux_run",
                Risk::Exec,
                "Type a command into a tmux pane and press Enter. `target` is `session` or `session:window.pane`; a bare session name that does not exist yet is created (detached, in the working directory). tmux sessions survive this app restarting — prefer them over shell_start for anything that must outlive the harness.",
                json!({
                    "type": "object",
                    "properties": {
                        "target": { "type": "string", "description": "Session name, or session:window.pane." },
                        "command": { "type": "string" }
                    },
                    "required": ["target", "command"]
                }),
                tm::tmux_run
            ),
            tool!(
                "tmux_capture",
                Risk::Read,
                true,
                "Read the last N lines of a tmux pane. With `wait_for` (regex) and `wait_ms`, polls until the pattern appears in the pane or the wait elapses — use it to wait for a prompt, a server banner, or a test summary.",
                json!({
                    "type": "object",
                    "properties": {
                        "target": { "type": "string" },
                        "lines": { "type": "integer", "description": "History lines to capture, 10-2000. Defaults to 200." },
                        "wait_for": { "type": "string", "description": "Regex to wait for in the pane." },
                        "wait_ms": { "type": "integer", "description": "How long to keep polling, in milliseconds. Defaults to 0." }
                    },
                    "required": ["target"]
                }),
                tm::tmux_capture
            ),
            tool!(
                "tmux_kill",
                Risk::Exec,
                "Kill a tmux session. Never kill a session you did not create unless the user asked.",
                json!({
                    "type": "object",
                    "properties": { "target": { "type": "string" } },
                    "required": ["target"]
                }),
                tm::tmux_kill
            ),
            tool!(
                "subagent",
                Risk::Read,
                true,
                "Delegate a self-contained task to a sub-agent that has its own context window and the same file tools. It returns only its final answer, keeping the detail out of your context. Several subagent calls in one turn run in parallel. Good for: broad searches, summarising large files, writing an isolated module. Not for: tasks needing the conversation history.",
                json!({
                    "type": "object",
                    "properties": {
                        "task": { "type": "string", "description": "Complete, self-contained instructions. The sub-agent cannot see this conversation." },
                        "model": { "type": "string", "description": "Model id to use. Defaults to the chat's model. Pick a cheaper one for mechanical work." },
                        "max_steps": { "type": "integer", "description": "Tool rounds allowed, 1-12. Defaults to 8." }
                    },
                    "required": ["task"]
                }),
                sa::subagent
            ),
            tool!(
                "plan",
                Risk::Read,
                "Write or update the plan for this task. It renders beside the conversation as markdown — checkboxes (`- [ ]` / `- [x]`), ```mermaid flowcharts and $LaTeX$ all work. Call it before starting multi-step work, and call it again with the FULL updated document as steps complete or the approach changes.",
                json!({
                    "type": "object",
                    "properties": {
                        "markdown": { "type": "string", "description": "The complete plan document. Replaces the previous version." },
                        "title": { "type": "string", "description": "Short name for the plan. Defaults to the first heading." }
                    },
                    "required": ["markdown"]
                }),
                ia::plan
            ),
            tool!(
                "ask_user",
                Risk::Read,
                true,
                "Ask the user a question and wait for their answer. Use when a decision genuinely changes the work — not for confirmations you can infer. Provide options and mark one `recommended` so the user can answer with one click.",
                json!({
                    "type": "object",
                    "properties": {
                        "question": { "type": "string" },
                        "options": { "type": "array", "items": { "type": "string" }, "description": "Choices rendered as buttons." },
                        "recommended": { "type": "string", "description": "The option you would pick, shown as the default." },
                        "allow_free_text": { "type": "boolean", "description": "Let the user type their own answer. Defaults to true." }
                    },
                    "required": ["question"]
                }),
                ia::ask_user
            ),
            tool!(
                "share_image",
                Risk::Read,
                "Show an image file inline in the conversation — a plot you generated, a screenshot, a diagram. This shows it to the USER only; the bytes never enter your context. To look at an image yourself, use view_image.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to an image file (png, jpg, gif, webp, svg…)." },
                        "caption": { "type": "string", "description": "One line shown under the image." }
                    },
                    "required": ["path"]
                }),
                ia::share_image
            ),
            tool!(
                "view_image",
                Risk::Read,
                "LOOK at an image file yourself: it is attached to your next step as a real image (when the current model reads images), so you can see the pixels — a downloaded reference, a photo to edit, a generated plot, a UI mock. The user is shown which image you are viewing. Screenshots from web_screenshot attach automatically; use this for every other image file.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Path to a raster image (png, jpg, gif, webp), absolute or relative to the working directory." }
                    },
                    "required": ["path"]
                }),
                ia::view_image
            ),
            tool!(
                "compact_context",
                Risk::Write,
                true,
                "Fold this conversation's older history into a structured rolling summary and prune the originals. Use when the context is getting heavy with stale tool output. The recent tail is kept verbatim; the summary covers goal, decisions, code state, open threads and constraints.",
                json!({ "type": "object", "properties": {} }),
                ctxt::compact_context
            ),
            tool!(
                "memory_search",
                Risk::Read,
                "Search the user's past chats in this app — their long-term memory. Use it when they refer to earlier conversations, prior decisions, or work discussed before. Returns matching snippets with chat ids; follow up with memory_read_chat.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Keyword or phrase to look for. Plain substring match." },
                        "limit": { "type": "integer", "description": "Max results, 1-20. Defaults to 8." }
                    },
                    "required": ["query"]
                }),
                mem::memory_search
            ),
            tool!(
                "memory_read_chat",
                Risk::Read,
                "Read the transcript of a past chat (found via memory_search), newest messages last. Tool output is summarised to one line each; user and assistant text is clipped, not omitted.",
                json!({
                    "type": "object",
                    "properties": {
                        "chat_id": { "type": "string", "description": "Id of the chat to read, from memory_search." },
                        "limit": { "type": "integer", "description": "How many trailing messages to include, 1-60. Defaults to 30." }
                    },
                    "required": ["chat_id"]
                }),
                mem::memory_read_chat
            ),
            tool!(
                "web_search",
                Risk::Network,
                "Search the web (no API key needed). Returns titles, URLs and snippets. Snippets are pointers, not evidence — follow up with fetch_url on the promising results before relying on them.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search terms. Specific beats generic." },
                        "max_results": { "type": "integer", "description": "1-10. Defaults to 8." }
                    },
                    "required": ["query"]
                }),
                web::web_search
            ),
            tool!(
                "deep_research",
                Risk::Network,
                true,
                "Research a question on the live web: an inner agent plans sub-queries, searches, reads the actual pages, cross-checks, and returns a cited report — the intermediate reading never enters this context. Use for anything needing current or multi-source information; use plain web_search for a single quick lookup.",
                json!({
                    "type": "object",
                    "properties": {
                        "question": { "type": "string", "description": "The research question, with any constraints that matter (timeframe, ecosystem, versions)." },
                        "depth": { "type": "string", "enum": ["quick", "standard", "thorough"], "description": "How many search-and-read rounds to budget. Defaults to standard." },
                        "model": { "type": "string", "description": "Model id for the researcher. Defaults to the chat's default model." }
                    },
                    "required": ["question"]
                }),
                research::deep_research
            ),
            tool!(
                "download_file",
                Risk::Network,
                "Download a URL — a page, image, archive or any file — to a path in the workspace. Streams to disk with a size cap; on a remote workspace the file lands on that machine.",
                json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" },
                        "path": { "type": "string", "description": "Where to save it, absolute or relative to the working directory." }
                    },
                    "required": ["url", "path"]
                }),
                n::download_file
            ),
            tool!(
                "web_screenshot",
                Risk::Exec,
                "Render a URL or local HTML file in a headless browser, save a PNG screenshot (shown to the user inline) and return the page's rendered text. This is how you SEE what you built: screenshot the dev server or the file, judge the pixels and the rendered text, and fix what looks wrong. Capture desktop and mobile widths for UI work. It cannot click or type — it only looks.",
                json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "http(s) URL to capture — e.g. a dev server you started." },
                        "path": { "type": "string", "description": "A local HTML file to capture instead of a URL." },
                        "width": { "type": "integer", "description": "Viewport width in px. Defaults to 1280; use 390 for mobile." },
                        "height": { "type": "integer", "description": "Viewport height in px. Defaults to 800." },
                        "wait_ms": { "type": "integer", "description": "How much virtual time the page gets to settle (JS, fonts). Defaults to 2500." },
                        "caption": { "type": "string", "description": "One line shown under the screenshot." }
                    }
                }),
                capture::web_screenshot
            ),
            tool!(
                "git_status",
                Risk::Read,
                "Git status of the workspace (porcelain format, with branch line). Works on remote workspaces too.",
                json!({ "type": "object", "properties": {} }),
                g::git_status
            ),
            tool!(
                "git_diff",
                Risk::Read,
                "Show git changes: working tree by default, `staged` for the index, `base` to diff against a ref, `path` to narrow.",
                json!({
                    "type": "object",
                    "properties": {
                        "staged": { "type": "boolean", "description": "Diff the index instead of the working tree." },
                        "base": { "type": "string", "description": "Diff against this ref, e.g. `main` or `HEAD~3`." },
                        "path": { "type": "string", "description": "Restrict to one file or directory." }
                    }
                }),
                g::git_diff
            ),
            tool!(
                "git_log",
                Risk::Read,
                "Recent commits, one line each with decorations.",
                json!({
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer", "description": "How many commits, 1-200. Defaults to 20." },
                        "path": { "type": "string", "description": "Only commits touching this path." }
                    }
                }),
                g::git_log
            ),
            tool!(
                "git_commit",
                Risk::Write,
                "Create a git commit. Stages everything first unless `add_all` is false. Never pushes.",
                json!({
                    "type": "object",
                    "properties": {
                        "message": { "type": "string" },
                        "add_all": { "type": "boolean", "description": "Run `git add -A` first. Defaults to true." }
                    },
                    "required": ["message"]
                }),
                g::git_commit
            ),
            tool!(
                "git_branch",
                Risk::Write,
                "List branches (no arguments), create and switch to one (`create`), or switch to an existing one (`switch`).",
                json!({
                    "type": "object",
                    "properties": {
                        "create": { "type": "string", "description": "Name for a new branch to create and switch to." },
                        "switch": { "type": "string", "description": "Existing branch to switch to." }
                    }
                }),
                g::git_branch
            ),
            tool!(
                "fetch_url",
                Risk::Network,
                "Fetch a URL over HTTPS and return its content as text, with HTML reduced to readable text.",
                json!({
                    "type": "object",
                    "properties": {
                        "url": { "type": "string" },
                        "max_bytes": { "type": "integer", "description": "Truncate after this many bytes. Defaults to 200000." }
                    },
                    "required": ["url"]
                }),
                n::fetch_url
            ),
        ]
    })
    .as_slice()
}

pub fn find(name: &str) -> Option<&'static ToolDef> {
    registry().iter().find(|t| t.name == name)
}

/// Specs for the enabled subset, ready to drop into a chat request.
pub fn specs(enabled: &[String]) -> Vec<Value> {
    registry()
        .iter()
        .filter(|t| enabled.is_empty() || enabled.iter().any(|e| e == t.name))
        .map(ToolDef::to_spec)
        .collect()
}

/// Runs a tool under a hard deadline and the run's cancellation token.
///
/// A tool that ignores cancellation still cannot outlive the deadline, which is
/// what keeps a stuck subprocess from stalling an agent forever.
pub async fn invoke(def: &'static ToolDef, ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let timeout = ctx.timeout;
    let cancel = ctx.cancel.clone();
    let fut = (def.handler)(ctx, args);
    if def.unbounded {
        // Waiting tools answer to cancellation only; a deadline would turn
        // "ask the user" into "ask the user quickly".
        return tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(AppError::Cancelled),
            r = fut => r,
        };
    }
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(AppError::Cancelled),
        r = tokio::time::timeout(timeout, fut) => match r {
            Ok(v) => v,
            Err(_) => Err(AppError::Timeout(timeout.as_millis() as u64)),
        },
    }
}

// ------------------------------------------------------------ vision markers

/// The image path a tool result asks to have attached to the model's context:
/// `web_screenshot` and `view_image` both lead with a marker line ending in
/// `<path> (…`. Returns None for every other tool result.
pub(crate) fn inline_image(content: &str) -> Option<String> {
    let rest = content
        .strip_prefix(capture::SCREENSHOT_MARKER)
        .or_else(|| content.strip_prefix(interact::VIEWED_MARKER))?;
    let line = rest.lines().next()?;
    let path = line.split(" (").next()?.trim();
    (!path.is_empty()).then(|| path.to_string())
}

/// Mime for attaching a raster image by extension; PNG when unsure.
pub(crate) fn image_mime(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" | "jfif" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => "image/png",
    }
}

// --------------------------------------------------------------- path safety

/// Normalises `.`/`..` without touching the filesystem, so a path that does not
/// exist yet (a file about to be written) can still be checked.
pub(crate) fn lexical_absolute(base: &Path, p: &Path) -> PathBuf {
    let joined = if p.is_absolute() { p.to_path_buf() } else { base.join(p) };
    let mut out = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Resolves a tool-supplied path against the working directory and enforces the
/// sandbox.
///
/// Two checks, in order: the deny list always wins, then the allow list if one
/// is configured. Both compare canonical forms so a symlink cannot be used to
/// step outside, and fall back to the lexical form for paths that do not exist
/// yet (a file about to be created).
pub fn resolve_path(ctx: &ToolCtx, raw: &str) -> Result<PathBuf> {
    let expanded = expand_home(raw);
    let abs = lexical_absolute(&ctx.cwd, &expanded);
    let target = abs.canonicalize().unwrap_or_else(|_| abs.clone());

    if !ctx.allow_denied {
        for denied in &ctx.denied_paths {
            let d = denied.canonicalize().unwrap_or_else(|_| denied.clone());
            if target.starts_with(&d) || abs.starts_with(denied) {
                return Err(AppError::Denied(format!(
                    "{} is a protected credential path. It is refused unless the user explicitly                      approves the call in the prompt that appears for it.",
                    abs.display()
                )));
            }
        }
    }

    if ctx.allowed_roots.is_empty() {
        return Ok(abs);
    }

    let ok = ctx.allowed_roots.iter().any(|r| {
        let root = r.canonicalize().unwrap_or_else(|_| r.clone());
        target.starts_with(&root) || abs.starts_with(r)
    });
    if ok {
        Ok(abs)
    } else {
        let roots = ctx.allowed_roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", ");
        Err(AppError::Denied(format!("{} is outside this chat's workspace. Allowed: {roots}", abs.display())))
    }
}

pub fn expand_home(raw: &str) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(raw),
    }
}

/// Human-friendly path for tool output: relative to cwd when it is inside it.
pub fn display_path(ctx: &ToolCtx, p: &Path) -> String {
    p.strip_prefix(&ctx.cwd).unwrap_or(p).display().to_string()
}

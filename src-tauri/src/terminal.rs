//! Terminal sessions — local PTY and remote SSH behind one interface.
//!
//! Output is coalesced in Rust before it crosses the IPC boundary: a PTY can
//! emit thousands of tiny writes a second, and forwarding each one individually
//! is what makes web-based terminals feel sluggish. Batching on an ~8 ms budget
//! keeps latency imperceptible while cutting IPC traffic by orders of magnitude.

use crate::db::Db;
use crate::error::{AppError, Result};
use crate::ssh::ShellCmd;
use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::ipc::{Channel, InvokeResponseBody};
use tokio::sync::mpsc;

/// Upper bound on how long a byte may sit in the coalescing buffer.
const FLUSH_EVERY: Duration = Duration::from_millis(8);
/// Flush early once the buffer reaches this size, so bulk output stays smooth.
const FLUSH_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub cwd: Option<String>,
    pub host_id: Option<String>,
    pub alive: bool,
}

struct Session {
    info: TerminalInfo,
    tx: mpsc::UnboundedSender<ShellCmd>,
    alive: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct TerminalManager {
    sessions: Mutex<HashMap<String, Session>>,
}

impl TerminalManager {
    pub fn list(&self) -> Vec<TerminalInfo> {
        self.sessions
            .lock()
            .values()
            .map(|s| TerminalInfo { alive: s.alive.load(Ordering::Relaxed), ..s.info.clone() })
            .collect()
    }

    pub fn write(&self, id: &str, data: &[u8]) -> Result<()> {
        self.send(id, ShellCmd::Write(data.to_vec()))
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<()> {
        self.send(id, ShellCmd::Resize { cols, rows })
    }

    pub fn close(&self, id: &str) -> Result<()> {
        let removed = self.sessions.lock().remove(id);
        match removed {
            Some(s) => {
                let _ = s.tx.send(ShellCmd::Close);
                Ok(())
            }
            None => Ok(()),
        }
    }

    pub fn close_all(&self) {
        for (_, s) in self.sessions.lock().drain() {
            let _ = s.tx.send(ShellCmd::Close);
        }
    }

    fn send(&self, id: &str, cmd: ShellCmd) -> Result<()> {
        let map = self.sessions.lock();
        let s = map.get(id).ok_or_else(|| AppError::NotFound(format!("terminal {id}")))?;
        s.tx.send(cmd).map_err(|_| AppError::msg("terminal has exited"))
    }

    /// Spawns a local shell attached to a real PTY.
    pub fn open_local(
        &self,
        id: String,
        shell: Option<String>,
        cwd: Option<String>,
        cols: u16,
        rows: u16,
        out: Channel<InvokeResponseBody>,
    ) -> Result<TerminalInfo> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| AppError::msg(format!("could not allocate a pty: {e}")))?;

        let program = shell.unwrap_or_else(default_shell);
        let mut cmd = CommandBuilder::new(&program);
        if !cfg!(windows) {
            // A login shell so the user's profile, PATH and prompt apply.
            cmd.arg("-l");
        }
        if let Some(dir) = cwd.as_deref() {
            cmd.cwd(dir);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| AppError::msg(format!("could not start {program}: {e}")))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().map_err(|e| AppError::msg(e.to_string()))?;
        let writer = pair.master.take_writer().map_err(|e| AppError::msg(e.to_string()))?;
        let master = Arc::new(Mutex::new(pair.master));
        let alive = Arc::new(AtomicBool::new(true));

        // Reader thread plus a flusher thread.
        //
        // The reader cannot flush on a timer by itself: `read` blocks, so a
        // small write like the shell's first prompt would sit in the buffer
        // until the next byte arrived — which is only when the user typed. The
        // flusher owns the time budget instead, and the reader only fills.
        let pending: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::with_capacity(FLUSH_BYTES)));

        {
            let pending = pending.clone();
            let out = out.clone();
            let alive = alive.clone();
            std::thread::Builder::new()
                .name(format!("pty-read-{id}"))
                .spawn(move || {
                    let mut buf = [0u8; 8192];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let mut p = pending.lock();
                                p.extend_from_slice(&buf[..n]);
                                // Bulk output should not wait for the tick.
                                if p.len() >= FLUSH_BYTES {
                                    let batch = std::mem::take(&mut *p);
                                    drop(p);
                                    if out.send(InvokeResponseBody::Raw(batch)).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    alive.store(false, Ordering::Relaxed);
                })
                .map_err(|e| AppError::msg(e.to_string()))?;
        }

        {
            let pending = pending.clone();
            let out = out.clone();
            let alive = alive.clone();
            std::thread::Builder::new()
                .name(format!("pty-flush-{id}"))
                .spawn(move || {
                    loop {
                        std::thread::sleep(FLUSH_EVERY);
                        let batch = {
                            let mut p = pending.lock();
                            if p.is_empty() {
                                if !alive.load(Ordering::Relaxed) {
                                    break;
                                }
                                continue;
                            }
                            std::mem::take(&mut *p)
                        };
                        if out.send(InvokeResponseBody::Raw(batch)).is_err() {
                            break;
                        }
                    }
                    // Zero-length frame is the agreed end-of-stream marker.
                    let _ = out.send(InvokeResponseBody::Raw(Vec::new()));
                })
                .map_err(|e| AppError::msg(e.to_string()))?;
        }

        let (tx, mut rx) = mpsc::unbounded_channel::<ShellCmd>();
        {
            let alive = alive.clone();
            let master = master.clone();
            let mut writer = writer;
            std::thread::Builder::new()
                .name(format!("pty-write-{id}"))
                .spawn(move || {
                    while let Some(cmd) = rx.blocking_recv() {
                        match cmd {
                            ShellCmd::Write(b) => {
                                if writer.write_all(&b).is_err() || writer.flush().is_err() {
                                    break;
                                }
                            }
                            ShellCmd::Resize { cols, rows } => {
                                let _ = master.lock().resize(PtySize {
                                    rows,
                                    cols,
                                    pixel_width: 0,
                                    pixel_height: 0,
                                });
                            }
                            ShellCmd::Close => break,
                        }
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    alive.store(false, Ordering::Relaxed);
                })
                .map_err(|e| AppError::msg(e.to_string()))?;
        }

        let info = TerminalInfo {
            id: id.clone(),
            kind: "local".into(),
            title: cwd
                .as_deref()
                .and_then(|c| std::path::Path::new(c).file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "shell".into()),
            cwd,
            host_id: None,
            alive: true,
        };
        self.sessions.lock().insert(id, Session { info: info.clone(), tx, alive });
        Ok(info)
    }

    /// Attaches an interactive shell on a saved SSH host.
    pub async fn open_ssh(
        &self,
        db: Arc<Db>,
        id: String,
        host_id: String,
        cols: u16,
        rows: u16,
        out: Channel<InvokeResponseBody>,
    ) -> Result<TerminalInfo> {
        let host = db.get_ssh_host(&host_id)?;
        let alive = Arc::new(AtomicBool::new(true));

        // Same coalescing budget as the local PTY, driven by a timer task
        // because SSH data arrives as discrete callbacks rather than reads.
        let pending = Arc::new(Mutex::new(Vec::<u8>::with_capacity(FLUSH_BYTES)));
        {
            let pending = pending.clone();
            let out = out.clone();
            let alive = alive.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(FLUSH_EVERY);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                while alive.load(Ordering::Relaxed) {
                    tick.tick().await;
                    let batch = {
                        let mut p = pending.lock();
                        if p.is_empty() {
                            continue;
                        }
                        std::mem::take(&mut *p)
                    };
                    if out.send(InvokeResponseBody::Raw(batch)).is_err() {
                        break;
                    }
                }
                let rest = std::mem::take(&mut *pending.lock());
                if !rest.is_empty() {
                    let _ = out.send(InvokeResponseBody::Raw(rest));
                }
                let _ = out.send(InvokeResponseBody::Raw(Vec::new()));
            });
        }

        let sink = pending.clone();
        let out_for_flush = out.clone();
        let alive_for_exit = alive.clone();
        let tx = crate::ssh::open_shell(
            db.clone(),
            &host,
            cols,
            rows,
            move |bytes| {
                let mut p = sink.lock();
                p.extend_from_slice(&bytes);
                if p.len() >= FLUSH_BYTES {
                    let _ = out_for_flush.send(InvokeResponseBody::Raw(std::mem::take(&mut *p)));
                }
            },
            move |_code| alive_for_exit.store(false, Ordering::Relaxed),
        )
        .await?;

        db.touch_ssh_host(&host_id)?;

        let info = TerminalInfo {
            id: id.clone(),
            kind: "ssh".into(),
            title: host.label.clone(),
            cwd: None,
            host_id: Some(host_id),
            alive: true,
        };
        self.sessions.lock().insert(id, Session { info: info.clone(), tx, alive });
        Ok(info)
    }
}

fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".into())
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into())
    }
}

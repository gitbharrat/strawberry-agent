//! Background shell sessions for the agent.
//!
//! `run_command` is for quick, synchronous things. These sessions are for
//! everything else: dev servers, watchers, REPLs, long builds — anything that
//! outlives one tool call. Each session is a real PTY with a rolling output
//! buffer; the agent starts one, sends input, and reads output incrementally
//! with an optional "wait until this pattern appears" — which is how it waits
//! for a server to be ready or a test run to finish without polling blindly.
//!
//! The buffer is capped and byte-offset addressed: readers keep a cursor, so
//! two reads never return the same bytes twice and a chatty process cannot
//! grow memory without bound.

use crate::error::{AppError, Result};
use parking_lot::Mutex;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

/// Rolling buffer cap per session. Old output is trimmed from the front; the
/// cursor arithmetic stays valid because offsets are absolute.
const BUFFER_CAP: usize = 512 * 1024;

struct Buffer {
    data: Vec<u8>,
    /// Absolute offset of `data[0]` since the session started.
    start: u64,
}

pub struct ShellSession {
    pub id: String,
    pub command: String,
    buffer: Mutex<Buffer>,
    notify: Notify,
    alive: AtomicBool,
    writer: Mutex<Box<dyn Write + Send>>,
    killer: Mutex<Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
}

impl ShellSession {
    /// Bytes from `cursor` onward, plus the new cursor.
    fn read_from(&self, cursor: u64) -> (String, u64, bool) {
        let buf = self.buffer.lock();
        let end = buf.start + buf.data.len() as u64;
        let from = cursor.clamp(buf.start, end);
        let slice = &buf.data[(from - buf.start) as usize..];
        let trimmed = cursor < buf.start;
        (String::from_utf8_lossy(slice).into_owned(), end, trimmed)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellInfo {
    pub id: String,
    pub command: String,
    pub alive: bool,
    pub buffered_bytes: usize,
}

#[derive(Default)]
pub struct BackgroundShells {
    sessions: Mutex<HashMap<String, Arc<ShellSession>>>,
}

impl BackgroundShells {
    pub fn start(&self, command: &str, cwd: &std::path::Path) -> Result<Arc<ShellSession>> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows: 40, cols: 160, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| AppError::msg(format!("could not allocate a pty: {e}")))?;

        let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut cmd = CommandBuilder::new(&sh);
        cmd.arg("-c");
        cmd.arg(command);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| AppError::msg(format!("could not start `{command}`: {e}")))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().map_err(|e| AppError::msg(e.to_string()))?;
        let writer = pair.master.take_writer().map_err(|e| AppError::msg(e.to_string()))?;

        let session = Arc::new(ShellSession {
            id: crate::db::new_id(),
            command: command.to_string(),
            buffer: Mutex::new(Buffer { data: Vec::new(), start: 0 }),
            notify: Notify::new(),
            alive: AtomicBool::new(true),
            writer: Mutex::new(writer),
            killer: Mutex::new(Some(child.clone_killer())),
        });

        {
            let s = session.clone();
            // The master must stay alive as long as the reader; move it in.
            let master = pair.master;
            std::thread::Builder::new()
                .name(format!("bg-shell-{}", s.id))
                .spawn(move || {
                    let _master = master;
                    let mut chunk = [0u8; 8192];
                    loop {
                        match reader.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let mut buf = s.buffer.lock();
                                buf.data.extend_from_slice(&chunk[..n]);
                                if buf.data.len() > BUFFER_CAP {
                                    let cut = buf.data.len() - BUFFER_CAP;
                                    buf.data.drain(..cut);
                                    buf.start += cut as u64;
                                }
                                drop(buf);
                                s.notify.notify_waiters();
                            }
                        }
                    }
                    s.alive.store(false, Ordering::Relaxed);
                    s.notify.notify_waiters();
                })
                .map_err(|e| AppError::msg(e.to_string()))?;
        }

        // The child handle only exists to be killed later; waiting happens
        // implicitly through the reader seeing EOF.
        let mut child = child;
        std::thread::spawn(move || {
            let _ = child.wait();
        });

        self.sessions.lock().insert(session.id.clone(), session.clone());
        Ok(session)
    }

    pub fn get(&self, id: &str) -> Result<Arc<ShellSession>> {
        self.sessions.lock().get(id).cloned().ok_or_else(|| AppError::NotFound(format!("shell session {id}")))
    }

    pub fn write(&self, id: &str, input: &[u8]) -> Result<()> {
        let s = self.get(id)?;
        if !s.alive.load(Ordering::Relaxed) {
            return Err(AppError::msg("that session has exited"));
        }
        let mut w = s.writer.lock();
        w.write_all(input).and_then(|()| w.flush()).map_err(|e| AppError::msg(e.to_string()))
    }

    /// Reads new output. With `wait_for`, blocks until the pattern shows up in
    /// fresh output, the process exits, or the deadline passes — whichever is
    /// first. Never hangs: the deadline is capped by the caller's tool timeout.
    pub async fn read(
        &self,
        id: &str,
        cursor: u64,
        wait_for: Option<&regex_lite::Regex>,
        deadline: std::time::Duration,
    ) -> Result<ShellRead> {
        let s = self.get(id)?;
        let until = tokio::time::Instant::now() + deadline;

        loop {
            let (text, next, trimmed) = s.read_from(cursor);
            let alive = s.alive.load(Ordering::Relaxed);
            let matched = wait_for.map(|re| re.is_match(&text));

            let done = match (wait_for, matched) {
                (None, _) => !text.is_empty() || !alive,
                (Some(_), Some(true)) => true,
                _ => !alive,
            };
            if done {
                return Ok(ShellRead {
                    text,
                    cursor: next,
                    alive,
                    trimmed,
                    matched: matched.unwrap_or(false),
                });
            }

            // Nothing yet: sleep until new output or the deadline.
            let notified = s.notify.notified();
            if tokio::time::timeout_at(until, notified).await.is_err() {
                let (text, next, trimmed) = s.read_from(cursor);
                return Ok(ShellRead {
                    text,
                    cursor: next,
                    alive: s.alive.load(Ordering::Relaxed),
                    trimmed,
                    matched: false,
                });
            }
        }
    }

    pub fn kill(&self, id: &str) -> Result<()> {
        let s = self.get(id)?;
        if let Some(mut killer) = s.killer.lock().take() {
            let _ = killer.kill();
        }
        s.alive.store(false, Ordering::Relaxed);
        s.notify.notify_waiters();
        self.sessions.lock().remove(id);
        Ok(())
    }

    pub fn list(&self) -> Vec<ShellInfo> {
        self.sessions
            .lock()
            .values()
            .map(|s| ShellInfo {
                id: s.id.clone(),
                command: s.command.clone(),
                alive: s.alive.load(Ordering::Relaxed),
                buffered_bytes: s.buffer.lock().data.len(),
            })
            .collect()
    }

    pub fn kill_all(&self) {
        let ids: Vec<String> = self.sessions.lock().keys().cloned().collect();
        for id in ids {
            let _ = self.kill(&id);
        }
    }
}

pub struct ShellRead {
    pub text: String,
    pub cursor: u64,
    pub alive: bool,
    pub trimmed: bool,
    pub matched: bool,
}

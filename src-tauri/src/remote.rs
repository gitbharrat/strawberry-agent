//! Remote workspaces over SSH.
//!
//! A chat whose working directory is `ssh://<host_id>/<path>` runs against
//! that machine instead of this one: file tools read and write through SFTP,
//! commands execute over the same connection, and the file browser lists the
//! remote tree. One authenticated session per host is kept in a pool and
//! shared by every caller — channels are cheap, connections are not.

use crate::db::SshHost;
use crate::error::{AppError, Result};
use russh::client;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Files above this are refused whole; the tools say to use offset/grep.
pub const MAX_REMOTE_READ: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct RemoteTarget {
    pub host_id: String,
    /// Absolute path on the remote machine.
    pub root: String,
}

/// Parses `ssh://<host_id>/<abs path>`. Anything else is a local path.
pub fn parse(cwd: &str) -> Option<RemoteTarget> {
    let rest = cwd.strip_prefix("ssh://")?;
    let slash = rest.find('/')?;
    let (host_id, path) = rest.split_at(slash);
    if host_id.is_empty() || path.is_empty() {
        return None;
    }
    Some(RemoteTarget { host_id: host_id.to_string(), root: path.to_string() })
}

pub fn format(host_id: &str, path: &str) -> String {
    format!("ssh://{host_id}{path}")
}

/// Lexically joins and normalises a tool-supplied path against the remote
/// root. `..` cannot escape the root while the file sandbox is on — the same
/// contract local tools get, minus symlink games we cannot see from here.
pub fn resolve(root: &str, raw: &str, confined: bool) -> Result<String> {
    let joined = if raw.starts_with('/') {
        raw.to_string()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        // The remote home is unknowable lexically; tools address the workspace.
        format!("{}/{rest}", root.trim_end_matches('/'))
    } else {
        format!("{}/{raw}", root.trim_end_matches('/'))
    };
    let mut parts: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    let abs = format!("/{}", parts.join("/"));
    if confined {
        let root_norm = root.trim_end_matches('/');
        if !(abs == root_norm || abs.starts_with(&format!("{root_norm}/"))) {
            return Err(AppError::Denied(format!(
                "{abs} is outside this chat's remote workspace ({root_norm})"
            )));
        }
    }
    Ok(abs)
}

/// One live connection: the SSH session plus an SFTP subsystem on it.
pub struct Remote {
    pub host: SshHost,
    session: client::Handle<crate::ssh::Handler>,
    sftp: SftpSession,
}

pub struct RemoteEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: i64,
}

impl Remote {
    /// Cheap liveness probe used by the pool before handing the session out.
    async fn alive(&self) -> bool {
        self.sftp.metadata("/").await.is_ok()
    }

    /// Runs a command through the remote login shell, `cd`'d into `cwd`.
    pub async fn exec(&self, cwd: &str, command: &str) -> Result<(String, u32)> {
        let full = format!("export NO_COLOR=1 CI=1 TERM=dumb; cd -- {} && {{ {command}\n}}", sh_quote(cwd));
        let mut channel =
            self.session.channel_open_session().await.map_err(|e| AppError::msg(e.to_string()))?;
        channel.exec(true, full.as_str()).await.map_err(|e| AppError::msg(e.to_string()))?;
        let mut out = String::new();
        let mut code = 0u32;
        while let Some(msg) = channel.wait().await {
            match msg {
                russh::ChannelMsg::Data { ref data } | russh::ChannelMsg::ExtendedData { ref data, .. } => {
                    out.push_str(&String::from_utf8_lossy(data));
                }
                russh::ChannelMsg::ExitStatus { exit_status } => code = exit_status,
                russh::ChannelMsg::Eof | russh::ChannelMsg::Close => break,
                _ => {}
            }
        }
        Ok((out, code))
    }

    pub async fn metadata(&self, path: &str) -> Result<russh_sftp::client::fs::Metadata> {
        self.sftp.metadata(path).await.map_err(|e| AppError::msg(format!("{path}: {e}")))
    }

    pub async fn read(&self, path: &str, cap: u64) -> Result<Vec<u8>> {
        let meta = self.metadata(path).await?;
        let size = meta.size.unwrap_or(0);
        if size > cap {
            return Err(AppError::msg(format!(
                "{path} is {:.1} MB — too large to read whole remotely",
                size as f64 / 1_048_576.0
            )));
        }
        let mut f = self
            .sftp
            .open_with_flags(path, OpenFlags::READ)
            .await
            .map_err(|e| AppError::msg(format!("{path}: {e}")))?;
        let mut buf = Vec::with_capacity(size as usize);
        f.read_to_end(&mut buf).await.map_err(|e| AppError::msg(format!("{path}: {e}")))?;
        Ok(buf)
    }

    pub async fn write(&self, path: &str, bytes: &[u8]) -> Result<()> {
        if let Some(parent) = path.rsplit_once('/').map(|(d, _)| d).filter(|d| !d.is_empty()) {
            let _ = self.exec("/", &format!("mkdir -p {}", sh_quote(parent))).await;
        }
        let mut f = self
            .sftp
            .open_with_flags(path, OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE)
            .await
            .map_err(|e| AppError::msg(format!("{path}: {e}")))?;
        f.write_all(bytes).await.map_err(|e| AppError::msg(format!("{path}: {e}")))?;
        f.shutdown().await.map_err(|e| AppError::msg(format!("{path}: {e}")))?;
        Ok(())
    }

    pub async fn list(&self, path: &str) -> Result<Vec<RemoteEntry>> {
        let rd = self.sftp.read_dir(path).await.map_err(|e| AppError::msg(format!("{path}: {e}")))?;
        let mut out = Vec::new();
        for entry in rd {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let meta = entry.metadata();
            out.push(RemoteEntry {
                is_dir: entry.file_type().is_dir(),
                size: meta.size.unwrap_or(0),
                modified: meta.mtime.map(|t| t as i64 * 1000).unwrap_or(0),
                name,
            });
        }
        Ok(out)
    }

    /// Resolves a user-supplied directory (may be `~`, relative, or absolute)
    /// to an absolute path, through the remote shell so tilde and $HOME work.
    pub async fn canonical_dir(&self, dir: &str) -> Result<String> {
        let d = dir.trim();
        let cd = if d.is_empty() || d == "~" {
            "cd".to_string()
        } else if let Some(rest) = d.strip_prefix("~/") {
            format!("cd -- ~/{}", sh_quote(rest).trim_matches('\'').replace('\'', "'\\''"))
        } else {
            format!("cd -- {}", sh_quote(d))
        };
        let (out, code) = self.exec("/", &format!("{cd} && pwd")).await?;
        let path = out.lines().last().unwrap_or("").trim().to_string();
        if code != 0 || !path.starts_with('/') {
            return Err(AppError::msg(format!(
                "no such directory on {}: {dir} ({})",
                self.host.label,
                out.trim()
            )));
        }
        Ok(path)
    }
}

impl Remote {
    /// Completes a directory prefix the way a shell's Tab would: `~/wo` gives
    /// every directory under home starting with `wo`, with trailing slashes.
    pub async fn complete_dir(&self, prefix: &str) -> Result<Vec<String>> {
        let (base, part) = match prefix.rsplit_once('/') {
            Some((b, p)) => (b.to_string(), p.to_string()),
            None => ("~".to_string(), prefix.to_string()),
        };
        let cd = if base.is_empty() {
            "cd /".to_string()
        } else if base == "~" {
            "cd".to_string()
        } else if let Some(rest) = base.strip_prefix("~/") {
            format!("cd -- ~/{}", sh_quote(rest))
        } else {
            format!("cd -- {}", sh_quote(&base))
        };
        // `ls -1d -- 'part'*/` keeps the glob outside the quotes and the
        // trailing slash marks directories; files never appear.
        let pat = if part.is_empty() { "*/".to_string() } else { format!("{}*/", sh_quote(&part)) };
        let (out, _code) =
            self.exec("/", &format!("{cd} 2>/dev/null && ls -1d -- {pat} 2>/dev/null | head -n 30")).await?;
        let shown_base = if base == "~" { "~".to_string() } else { base };
        Ok(out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| format!("{}/{}", shown_base.trim_end_matches('/'), l.trim_end_matches('/')))
            .collect())
    }
}

/// Single-quote a string for a POSIX shell.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Connects (or reuses) the pooled session for a host. A dead cached session
/// is replaced transparently, so a dropped Wi-Fi link costs one retry, not a
/// broken workspace.
pub async fn pooled(state: &crate::state::AppState, host_id: &str) -> Result<Arc<Remote>> {
    {
        let pool = state.remotes.lock().await;
        if let Some(r) = pool.get(host_id) {
            if r.alive().await {
                return Ok(r.clone());
            }
        }
    }
    let host = state.db.get_ssh_host(host_id)?;
    let session = crate::ssh::connect(&state.db, &host).await.inspect_err(|e| {
        tracing::error!(
            "remote workspace: connect to {} ({}:{}) failed: {e}",
            host.label,
            host.host,
            host.port
        );
    })?;
    let channel = session.channel_open_session().await.map_err(|e| AppError::msg(e.to_string()))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| AppError::msg(format!("{}: the server refused the sftp subsystem: {e}", host.label)))?;
    let sftp = SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| AppError::msg(format!("{}: sftp handshake failed: {e}", host.label)))?;
    let remote = Arc::new(Remote { host, session, sftp });
    state.remotes.lock().await.insert(host_id.to_string(), remote.clone());
    Ok(remote)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_remote_cwds() {
        let t = parse("ssh://abc123/workspace/app").unwrap();
        assert_eq!(t.host_id, "abc123");
        assert_eq!(t.root, "/workspace/app");
        assert!(parse("/local/path").is_none());
        assert!(parse("ssh://noslash").is_none());
    }

    #[test]
    fn resolve_confines_to_root() {
        assert_eq!(resolve("/w/app", "src/main.rs", true).unwrap(), "/w/app/src/main.rs");
        assert_eq!(resolve("/w/app", "/w/app/x", true).unwrap(), "/w/app/x");
        assert_eq!(resolve("/w/app", "a/../b", true).unwrap(), "/w/app/b");
        assert!(resolve("/w/app", "../escape", true).is_err());
        assert!(resolve("/w/app", "/etc/passwd", true).is_err());
        assert_eq!(resolve("/w/app", "/etc/passwd", false).unwrap(), "/etc/passwd");
    }
}

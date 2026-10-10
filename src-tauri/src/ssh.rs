//! SSH client built on russh.
//!
//! Two entry points: `exec_once` for the `ssh_exec` tool, and `open_shell` for
//! an interactive remote terminal. Host keys follow trust-on-first-use — the
//! fingerprint is recorded on first connect and a later mismatch is a hard
//! error, the same contract OpenSSH gives.

use crate::db::{Db, SshHost};
use crate::error::{AppError, Result};
use crate::secrets;
use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::ChannelMsg;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) struct Handler {
    expected: Option<String>,
    observed: Arc<parking_lot::Mutex<Option<String>>>,
}

impl client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let fp = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        *self.observed.lock() = Some(fp.clone());
        match &self.expected {
            // First contact: accept and let the caller persist the fingerprint.
            None => Ok(true),
            Some(known) => Ok(known == &fp),
        }
    }
}

fn fingerprint_key(host: &SshHost) -> String {
    format!("ssh-hostkey:{}:{}", host.host, host.port)
}

pub(crate) async fn connect(db: &Db, host: &SshHost) -> Result<client::Handle<Handler>> {
    let expected = db.get_meta(&fingerprint_key(host))?;
    let observed = Arc::new(parking_lot::Mutex::new(None));

    let config = Arc::new(client::Config {
        // Nothing to do with idle *sessions*: this is russh's own protocol
        // inactivity guard. Long agent runs keep terminals open for hours, so it
        // must be generous.
        inactivity_timeout: Some(Duration::from_secs(3600)),
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 6,
        ..Default::default()
    });

    let handler = Handler { expected: expected.clone(), observed: observed.clone() };
    let addr = (host.host.as_str(), host.port as u16);

    let mut session = tokio::time::timeout(CONNECT_TIMEOUT, client::connect(config, addr, handler))
        .await
        .map_err(|_| AppError::Timeout(CONNECT_TIMEOUT.as_millis() as u64))?
        .map_err(|e| {
            // russh reports a rejected host key as a generic failure; the
            // observed fingerprint tells us which case it really was.
            let seen = observed.lock().clone();
            match (&expected, seen) {
                (Some(known), Some(got)) if known != &got => AppError::Denied(format!(
                    "host key for {} changed!\n  expected {known}\n  got      {got}\nRemove the saved key in Settings → SSH if this is expected.",
                    host.host
                )),
                _ => AppError::msg(format!("could not connect to {}:{}: {e}", host.host, host.port)),
            }
        })?;

    if expected.is_none() {
        if let Some(fp) = observed.lock().clone() {
            db.set_meta(&fingerprint_key(host), &fp)?;
        }
    }

    authenticate(&mut session, host).await?;
    Ok(session)
}

async fn authenticate(session: &mut client::Handle<Handler>, host: &SshHost) -> Result<()> {
    let user = host.username.clone();

    let result = match host.auth.as_str() {
        "password" => {
            let pw = secrets::get(&secrets::ssh_account(&host.id))?
                .ok_or_else(|| AppError::msg("no saved password for this host"))?;
            session.authenticate_password(user, pw).await.map_err(|e| AppError::msg(e.to_string()))?
        }
        _ => {
            // The ssh-agent first, exactly like OpenSSH: it holds already
            // decrypted identities, so passphrase-protected keys work without
            // this app ever seeing a passphrase. This is why `ssh host` in a
            // terminal succeeds on setups where reading the key file fails.
            if agent_auth(session, &user).await {
                tracing::info!("ssh: authenticated {}@{} via ssh-agent", host.username, host.host);
                return Ok(());
            }

            // Key files next: an explicit path if configured, otherwise the
            // usual suspects in ~/.ssh, tried in modern-first order.
            let candidates: Vec<std::path::PathBuf> = match &host.key_path {
                Some(p) => vec![shellexpand_home(p)],
                None => {
                    let ssh = dirs::home_dir().unwrap_or_default().join(".ssh");
                    ["id_ed25519", "id_ecdsa", "id_rsa"].iter().map(|n| ssh.join(n)).collect()
                }
            };
            let passphrase = secrets::get(&secrets::ssh_account(&host.id))?;

            let mut last_err = None;
            let mut authed = None;
            for path in candidates {
                if !path.exists() {
                    continue;
                }
                let key = match russh::keys::load_secret_key(&path, passphrase.as_deref()) {
                    Ok(k) => k,
                    Err(e) => {
                        last_err = Some(format!("{}: {e}", path.display()));
                        continue;
                    }
                };
                let hash = session.best_supported_rsa_hash().await.ok().flatten().flatten();
                match session
                    .authenticate_publickey(user.clone(), PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                    .await
                {
                    Ok(r) if r.success() => {
                        authed = Some(r);
                        break;
                    }
                    Ok(r) => authed = Some(r),
                    Err(e) => last_err = Some(e.to_string()),
                }
            }
            authed.ok_or_else(|| {
                AppError::msg(format!(
                    "no usable SSH key: the ssh-agent offered nothing this host accepts{}",
                    last_err.map(|e| format!(", and key files failed ({e})")).unwrap_or_default()
                ))
            })?
        }
    };

    if !result.success() {
        return Err(AppError::Denied(format!("authentication failed for {}@{}", host.username, host.host)));
    }
    Ok(())
}

/// Tries every identity the local ssh-agent holds. `true` means the session
/// is authenticated.
#[cfg(unix)]
async fn agent_auth(session: &mut client::Handle<Handler>, user: &str) -> bool {
    let Ok(mut agent) = russh::keys::agent::client::AgentClient::connect_env().await else {
        return false;
    };
    let Ok(identities) = agent.request_identities().await else {
        return false;
    };
    for id in identities {
        let key = id.public_key().into_owned();
        let hash = session.best_supported_rsa_hash().await.ok().flatten().flatten();
        match session.authenticate_publickey_with(user.to_string(), key, hash, &mut agent).await {
            Ok(r) if r.success() => return true,
            Ok(_) => {}
            Err(e) => tracing::debug!("ssh-agent identity refused: {e}"),
        }
    }
    false
}

#[cfg(not(unix))]
async fn agent_auth(_session: &mut client::Handle<Handler>, _user: &str) -> bool {
    false
}

fn shellexpand_home(p: &str) -> std::path::PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => std::path::PathBuf::from(p),
    }
}

/// Runs one command and collects its output. Used by the `ssh_exec` tool.
pub async fn exec_once(
    db: &Db,
    host: &SshHost,
    command: &str,
    cancel: &CancellationToken,
) -> Result<(String, u32)> {
    let session = connect(db, host).await?;
    let mut channel = session.channel_open_session().await.map_err(|e| AppError::msg(e.to_string()))?;
    channel.exec(true, command).await.map_err(|e| AppError::msg(e.to_string()))?;

    let mut out = String::new();
    let mut code = 0u32;
    loop {
        let msg = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(AppError::Cancelled),
            m = channel.wait() => m,
        };
        let Some(msg) = msg else { break };
        match msg {
            ChannelMsg::Data { ref data } | ChannelMsg::ExtendedData { ref data, .. } => {
                out.push_str(&String::from_utf8_lossy(data));
            }
            ChannelMsg::ExitStatus { exit_status } => code = exit_status,
            ChannelMsg::Eof | ChannelMsg::Close => break,
            _ => {}
        }
    }
    let _ = session.disconnect(russh::Disconnect::ByApplication, "", "en").await;
    Ok((out, code))
}

/// A transfer bigger than this is refused outright. File transfer here is a
/// coding-agent convenience — configs, logs, builds — not a backup pipe, and a
/// hard cap is what keeps a mistyped path from downloading a disk image.
const MAX_TRANSFER: u64 = 512 * 1024 * 1024;

/// Per-request chunk size. 64 KiB is at or under every real server's read/write
/// limit while being big enough that the window, not the chunk, sets throughput.
const CHUNK: u64 = 64 * 1024;
/// In-flight requests per transfer: 32 × 64 KiB = 2 MiB on the wire, which is
/// what turns a 100-round-trip file into a ~4-round-trip one. This is the whole
/// reason these functions use the raw SFTP session — the high-level `File`
/// reads strictly one request at a time.
const WINDOW: usize = 32;

async fn sftp_open(
    db: &Db,
    host: &SshHost,
) -> Result<(client::Handle<Handler>, russh_sftp::client::rawsession::RawSftpSession)> {
    let session = connect(db, host).await?;
    let channel = session.channel_open_session().await.map_err(|e| AppError::msg(e.to_string()))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| AppError::msg(format!("the server refused the sftp subsystem: {e}")))?;
    let raw = russh_sftp::client::rawsession::RawSftpSession::new(channel.into_stream());
    // Generous per-request timeout: on a saturated link a queued chunk can sit
    // a while, and the caller has its own cancellation.
    raw.set_timeout(120);
    raw.init().await.map_err(|e| AppError::msg(format!("sftp handshake failed: {e}")))?;
    Ok((session, raw))
}

/// Positional local write, so out-of-order chunk arrivals land in place.
#[cfg(unix)]
fn write_all_at(f: &std::fs::File, buf: &[u8], off: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(f, buf, off)
}
#[cfg(windows)]
fn write_all_at(f: &std::fs::File, mut buf: &[u8], mut off: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_write(buf, off)?;
        buf = &buf[n..];
        off += n as u64;
    }
    Ok(())
}

#[cfg(unix)]
fn read_exact_at(f: &std::fs::File, buf: &mut [u8], off: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, off)
}
#[cfg(windows)]
fn read_exact_at(f: &std::fs::File, mut buf: &mut [u8], mut off: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_read(buf, off)?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "file shrank while reading"));
        }
        buf = &mut buf[n..];
        off += n as u64;
    }
    Ok(())
}

/// Copies a remote file to a local path over SFTP, with `WINDOW` chunk reads
/// pipelined so latency is paid once, not per chunk. Returns bytes written.
pub async fn sftp_download(
    db: &Db,
    host: &SshHost,
    remote: &str,
    local: &std::path::Path,
    cancel: &CancellationToken,
) -> Result<u64> {
    use futures_util::stream::{FuturesUnordered, StreamExt};

    let (session, sftp) = sftp_open(db, host).await?;
    let work = async {
        let handle = sftp
            .open(
                remote,
                russh_sftp::protocol::OpenFlags::READ,
                russh_sftp::protocol::FileAttributes::default(),
            )
            .await
            .map_err(|e| AppError::msg(format!("could not open remote {remote}: {e}")))?
            .handle;
        let size = sftp
            .fstat(handle.clone())
            .await
            .map_err(|e| AppError::msg(format!("remote {remote}: {e}")))?
            .attrs
            .size
            .unwrap_or(0);
        if size > MAX_TRANSFER {
            return Err(AppError::msg(format!(
                "remote file is {size} bytes; transfers are capped at {MAX_TRANSFER}"
            )));
        }

        if let Some(parent) = local.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| AppError::msg(e.to_string()))?;
        }
        let file = std::fs::File::create(local).map_err(|e| AppError::msg(e.to_string()))?;
        file.set_len(size).map_err(|e| AppError::msg(e.to_string()))?;

        let read_chunk = |off: u64, len: u64| {
            let h = handle.clone();
            let sftp = &sftp;
            async move { (off, len, sftp.read(h, off, len as u32).await) }
        };

        let mut pending = FuturesUnordered::new();
        let mut next = 0u64;
        while next < size && pending.len() < WINDOW {
            let len = CHUNK.min(size - next);
            pending.push(read_chunk(next, len));
            next += len;
        }
        let mut received = 0u64;
        while let Some((off, len, res)) = pending.next().await {
            let data = res.map_err(|e| AppError::msg(format!("read at {off} failed: {e}")))?.data;
            if data.is_empty() || data.len() as u64 > len {
                return Err(AppError::msg("remote file changed while downloading"));
            }
            write_all_at(&file, &data, off).map_err(|e| AppError::msg(e.to_string()))?;
            received += data.len() as u64;
            // Servers may legally return fewer bytes than asked; re-request the gap.
            if (data.len() as u64) < len {
                pending.push(read_chunk(off + data.len() as u64, len - data.len() as u64));
            }
            if next < size {
                let l = CHUNK.min(size - next);
                pending.push(read_chunk(next, l));
                next += l;
            }
        }
        let _ = sftp.close(handle).await;
        if received != size {
            return Err(AppError::msg(format!("transfer incomplete: {received} of {size} bytes")));
        }
        Ok(size)
    };
    let out = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(AppError::Cancelled),
        r = work => r,
    };
    let _ = session.disconnect(russh::Disconnect::ByApplication, "", "en").await;
    out
}

/// Copies a local file to a remote path over SFTP, with `WINDOW` chunk writes
/// pipelined. Returns bytes sent.
pub async fn sftp_upload(
    db: &Db,
    host: &SshHost,
    local: &std::path::Path,
    remote: &str,
    cancel: &CancellationToken,
) -> Result<u64> {
    use futures_util::stream::{FuturesUnordered, StreamExt};

    let size = tokio::fs::metadata(local)
        .await
        .map_err(|e| AppError::msg(format!("{}: {e}", local.display())))?
        .len();
    if size > MAX_TRANSFER {
        return Err(AppError::msg(format!(
            "{} is {size} bytes; transfers are capped at {MAX_TRANSFER}",
            local.display()
        )));
    }

    let (session, sftp) = sftp_open(db, host).await?;
    let work = async {
        let file = std::fs::File::open(local).map_err(|e| AppError::msg(e.to_string()))?;
        use russh_sftp::protocol::OpenFlags;
        let handle = sftp
            .open(
                remote,
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
                russh_sftp::protocol::FileAttributes::default(),
            )
            .await
            .map_err(|e| AppError::msg(format!("could not create remote {remote}: {e}")))?
            .handle;

        let write_chunk = |off: u64, data: Vec<u8>| {
            let h = handle.clone();
            let sftp = &sftp;
            async move { (off, data.len() as u64, sftp.write(h, off, data).await) }
        };

        let mut pending = FuturesUnordered::new();
        let mut next = 0u64;
        let read_next = |next: &mut u64| -> Result<Option<(u64, Vec<u8>)>> {
            if *next >= size {
                return Ok(None);
            }
            let len = CHUNK.min(size - *next) as usize;
            let mut buf = vec![0u8; len];
            read_exact_at(&file, &mut buf, *next).map_err(|e| AppError::msg(e.to_string()))?;
            let off = *next;
            *next += len as u64;
            Ok(Some((off, buf)))
        };
        while pending.len() < WINDOW {
            match read_next(&mut next)? {
                Some((off, buf)) => pending.push(write_chunk(off, buf)),
                None => break,
            }
        }
        let mut sent = 0u64;
        while let Some((off, len, res)) = pending.next().await {
            res.map_err(|e| AppError::msg(format!("write at {off} failed: {e}")))?;
            sent += len;
            if let Some((o, buf)) = read_next(&mut next)? {
                pending.push(write_chunk(o, buf));
            }
        }
        let _ = sftp.close(handle).await;
        if sent != size {
            return Err(AppError::msg(format!("transfer incomplete: {sent} of {size} bytes")));
        }
        Ok(size)
    };
    let out = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(AppError::Cancelled),
        r = work => r,
    };
    let _ = session.disconnect(russh::Disconnect::ByApplication, "", "en").await;
    out
}

/// Control messages for a live remote shell.
pub enum ShellCmd {
    Write(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Close,
}

/// Opens an interactive remote shell.
///
/// The channel is owned by one task that multiplexes reads and control
/// messages, which sidesteps the need to split the channel and makes shutdown a
/// single well-defined path.
pub async fn open_shell(
    db: Arc<Db>,
    host: &SshHost,
    cols: u16,
    rows: u16,
    mut on_data: impl FnMut(Vec<u8>) + Send + 'static,
    on_exit: impl FnOnce(Option<u32>) + Send + 'static,
) -> Result<mpsc::UnboundedSender<ShellCmd>> {
    let session = connect(&db, host).await?;
    let mut channel = session.channel_open_session().await.map_err(|e| AppError::msg(e.to_string()))?;
    channel
        .request_pty(true, "xterm-256color", cols as u32, rows as u32, 0, 0, &[])
        .await
        .map_err(|e| AppError::msg(e.to_string()))?;
    channel.request_shell(true).await.map_err(|e| AppError::msg(e.to_string()))?;

    let (tx, mut rx) = mpsc::unbounded_channel::<ShellCmd>();
    tokio::spawn(async move {
        let mut exit = None;
        loop {
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(ShellCmd::Write(b)) => {
                        if channel.data(&b[..]).await.is_err() { break }
                    }
                    Some(ShellCmd::Resize { cols, rows }) => {
                        let _ = channel.window_change(cols as u32, rows as u32, 0, 0).await;
                    }
                    Some(ShellCmd::Close) | None => break,
                },
                msg = channel.wait() => match msg {
                    Some(ChannelMsg::Data { ref data }) | Some(ChannelMsg::ExtendedData { ref data, .. }) => {
                        on_data(data.to_vec());
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) => exit = Some(exit_status),
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                    _ => {}
                },
            }
        }
        let _ = channel.close().await;
        let _ = session.disconnect(russh::Disconnect::ByApplication, "", "en").await;
        on_exit(exit);
    });

    Ok(tx)
}

/// Connects, authenticates and immediately disconnects — the "Test connection"
/// button in settings.
pub async fn test_connection(db: Arc<Db>, host: &SshHost) -> Result<String> {
    let session = connect(&db, host).await?;
    let banner = db.get_meta(&fingerprint_key(host))?.unwrap_or_default();
    let _ = session.disconnect(russh::Disconnect::ByApplication, "", "en").await;
    Ok(banner)
}

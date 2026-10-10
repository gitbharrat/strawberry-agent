//! Headless rendering: screenshot a URL or local HTML file and read its
//! rendered text.
//!
//! This is deliberately not computer use. The tool loads a page in a headless
//! Chromium, waits for it to settle, saves one PNG and dumps the rendered DOM
//! as text — evidence a verifier can judge and the user can see. It cannot
//! click, type or navigate; anything interactive stays the human's job.

use super::{ToolCtx, ToolOutput};
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// The marker line the graph executor greps for to hand the PNG to a
/// vision-capable model. Keep the format stable: `Screenshot saved: <path> …`.
pub const SCREENSHOT_MARKER: &str = "Screenshot saved: ";

const MAX_PAGE_TEXT: usize = 12_000;

/// Chromium-family binaries, most common first. Headless flags are identical
/// across all of them.
fn find_browser() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let candidates: &[&str] = &[
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
        "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        "/Applications/Arc.app/Contents/MacOS/Arc",
    ];
    #[cfg(target_os = "windows")]
    let candidates: &[&str] = &[
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ];
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates: &[&str] = &[];

    for c in candidates {
        let p = PathBuf::from(c);
        if p.is_file() {
            return Some(p);
        }
    }
    // PATH lookups cover Linux and hand-installed browsers everywhere.
    for name in ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "msedge", "brave"]
    {
        if let Ok(out) = std::process::Command::new(if cfg!(windows) { "where" } else { "which" })
            .arg(name)
            .output()
        {
            if out.status.success() {
                let line = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or("").to_string();
                if !line.is_empty() {
                    return Some(PathBuf::from(line));
                }
            }
        }
    }
    None
}

/// Resolves the tool's `url`/`path` arguments into something a browser loads.
fn resolve_target(ctx: &ToolCtx, args: &Value) -> Result<String> {
    if let Some(url) = args.get("url").and_then(Value::as_str).filter(|u| !u.trim().is_empty()) {
        let url = url.trim();
        if url.starts_with("http://") || url.starts_with("https://") {
            return Ok(url.to_string());
        }
        if let Some(rest) = url.strip_prefix("file://") {
            return Ok(format!("file://{rest}"));
        }
        // A bare path handed in as `url` — treat it like `path`.
        let p = super::resolve_path(ctx, url)?;
        return Ok(format!("file://{}", p.display()));
    }
    if let Some(raw) = args.get("path").and_then(Value::as_str).filter(|p| !p.trim().is_empty()) {
        if ctx.remote.is_some() {
            return Err(AppError::msg(
                "this workspace is remote — screenshot a served http:// URL instead of a file path, or download the file locally first",
            ));
        }
        let p = super::resolve_path(ctx, raw)?;
        if !p.is_file() {
            return Err(AppError::msg(format!("{} does not exist", p.display())));
        }
        return Ok(format!("file://{}", p.display()));
    }
    Err(AppError::msg("pass `url` (http/https) or `path` (a local HTML file)"))
}

struct BrowserRun {
    stdout: Vec<u8>,
    stderr_tail: String,
}

/// Runs the browser with stdout/stderr redirected to FILES, never pipes.
///
/// Chrome on macOS spawns updater helpers that inherit the pipes and hold
/// them open long after the real work is done — `Command::output()` then
/// waits on pipe-EOF forever, which was exactly the "screenshot hangs" bug:
/// the PNG existed within a second while the tool sat until its deadline.
/// With files there is no EOF to wait for; the loop below watches the child
/// pid — and, when `watch` names the artifact, the artifact itself, declaring
/// success the moment it lands and stabilises even if the browser lingers.
async fn run_browser(
    ctx: &ToolCtx,
    browser: &PathBuf,
    args: Vec<String>,
    log_base: &std::path::Path,
    watch: Option<&std::path::Path>,
) -> Result<BrowserRun> {
    let out_path = log_base.with_extension("out");
    let err_path = log_base.with_extension("err");
    let out_file = std::fs::File::create(&out_path).map_err(|e| AppError::msg(e.to_string()))?;
    let err_file = std::fs::File::create(&err_path).map_err(|e| AppError::msg(e.to_string()))?;

    let mut cmd = tokio::process::Command::new(browser);
    cmd.args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(out_file))
        .stderr(std::process::Stdio::from(err_file))
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| AppError::msg(format!("could not launch the browser: {e}")))?;

    let deadline = ctx.timeout.clamp(Duration::from_secs(15), Duration::from_secs(90));
    let started = std::time::Instant::now();
    let mut last_len: u64 = 0;
    let outcome = loop {
        if ctx.cancel.is_cancelled() {
            let _ = child.start_kill();
            break Err(AppError::Cancelled);
        }
        if let Some(w) = watch {
            let len = std::fs::metadata(w).map(|m| m.len()).unwrap_or(0);
            // Landed and stopped growing across one tick: that is the result;
            // whatever the browser does next is its own business.
            if len > 0 && len == last_len {
                let _ = child.start_kill();
                break Ok(());
            }
            last_len = len;
        }
        match child.try_wait() {
            Ok(Some(_)) => break Ok(()),
            Ok(None) => {}
            Err(e) => break Err(AppError::msg(format!("browser process error: {e}"))),
        }
        if started.elapsed() > deadline {
            let _ = child.start_kill();
            let landed = watch.map(|w| std::fs::metadata(w).map(|m| m.len() > 0).unwrap_or(false));
            break if landed == Some(true) {
                Ok(())
            } else {
                Err(AppError::Timeout(deadline.as_millis() as u64))
            };
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    };

    let stdout = std::fs::read(&out_path).unwrap_or_default();
    let stderr = std::fs::read_to_string(&err_path).unwrap_or_default();
    let stderr_tail: String = stderr.chars().rev().take(500).collect::<String>().chars().rev().collect();
    let _ = std::fs::remove_file(&out_path);
    let _ = std::fs::remove_file(&err_path);
    outcome.map(|()| BrowserRun { stdout, stderr_tail })
}

/// Renders a page headlessly: one PNG for the humans, rendered text for the
/// model. Chrome's `--virtual-time-budget` fast-forwards timers so a page that
/// builds itself with JavaScript is captured settled, not blank.
pub async fn web_screenshot(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let Some(browser) = find_browser() else {
        return Ok(ToolOutput::error(
            "No Chromium-family browser found for headless capture (looked for Chrome, Chromium, Edge, Brave). \
             Install one, or verify without pixels: fetch the page text, check HTTP status, run the tests.",
        ));
    };
    let target = resolve_target(&ctx, &args)?;

    let width = args.get("width").and_then(Value::as_u64).unwrap_or(1280).clamp(320, 3840);
    let height = args.get("height").and_then(Value::as_u64).unwrap_or(800).clamp(320, 4000);
    let wait_ms = args.get("wait_ms").and_then(Value::as_u64).unwrap_or(2500).clamp(0, 20_000);

    let dir = crate::db::data_dir().join("screenshots");
    std::fs::create_dir_all(&dir).map_err(|e| AppError::msg(e.to_string()))?;
    let short_id: String = ctx.call_id.chars().take(8).collect();
    let shot = dir.join(format!("shot-{}-{}x{}.png", short_id, width, height));
    // A throwaway profile PER CALL: the user's browser is never touched, and
    // parallel graph nodes cannot trip over Chrome's profile lock.
    let profile = dir.join(format!("profile-{short_id}"));

    let common = |extra: &[String]| -> Vec<String> {
        let mut v = vec![
            "--headless=new".to_string(),
            "--disable-gpu".to_string(),
            "--no-first-run".to_string(),
            "--no-default-browser-check".to_string(),
            // Keeps Chrome from waking updaters and phoning home from the
            // throwaway profile — fewer stray child processes, faster runs.
            "--disable-background-networking".to_string(),
            "--disable-component-update".to_string(),
            "--disable-sync".to_string(),
            "--hide-scrollbars".to_string(),
            "--force-device-scale-factor=1".to_string(),
            format!("--window-size={width},{height}"),
            format!("--virtual-time-budget={wait_ms}"),
            format!("--user-data-dir={}", profile.display()),
        ];
        v.extend(extra.iter().cloned());
        v.push(target.clone());
        v
    };

    let result = run_browser(
        &ctx,
        &browser,
        common(&[format!("--screenshot={}", shot.display())]),
        &dir.join(format!("log-shot-{short_id}")),
        Some(&shot),
    )
    .await;
    let out = match result {
        Ok(out) => out,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&profile);
            return Err(e);
        }
    };
    if !shot.is_file() {
        let _ = std::fs::remove_dir_all(&profile);
        return Ok(ToolOutput::error(format!(
            "The browser produced no screenshot for {target}. Its output ended with:\n{}",
            out.stderr_tail
        )));
    }
    let bytes = std::fs::metadata(&shot).map(|m| m.len()).unwrap_or(0);

    // Second pass: the rendered DOM as text, so a non-vision model still gets
    // real signal (missing sections, error banners, empty roots).
    let page_text = match run_browser(
        &ctx,
        &browser,
        common(&["--dump-dom".to_string()]),
        &dir.join(format!("log-dom-{short_id}")),
        None,
    )
    .await
    {
        Ok(dom_out) => {
            let dom = String::from_utf8_lossy(&dom_out.stdout);
            let text = super::net::html_to_text(&dom);
            let clipped: String = text.chars().take(MAX_PAGE_TEXT).collect();
            if text.len() > clipped.len() {
                format!("{clipped}\n… rendered text truncated")
            } else {
                clipped
            }
        }
        Err(AppError::Cancelled) => {
            let _ = std::fs::remove_dir_all(&profile);
            return Err(AppError::Cancelled);
        }
        Err(_) => String::new(),
    };
    let _ = std::fs::remove_dir_all(&profile);

    let caption = args.get("caption").and_then(Value::as_str).unwrap_or(&target).to_string();
    let mut content = format!(
        "{SCREENSHOT_MARKER}{} ({width}x{height}, {} KB) — it is shown to the user in the conversation.",
        shot.display(),
        bytes / 1024
    );
    if page_text.trim().is_empty() {
        content.push_str("\n\nThe rendered page produced no readable text — for a UI this usually means a blank page, a load error, or a canvas-only render. Inspect the screenshot and the page source before passing judgement.");
    } else {
        content.push_str("\n\nRendered page text:\n");
        content.push_str(&page_text);
    }

    Ok(ToolOutput::text(content).with_display(json!({
        "kind": "image",
        "path": shot.display().to_string(),
        "name": shot.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        "caption": caption,
        "bytes": bytes,
    })))
}

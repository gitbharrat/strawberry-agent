//! Network tool: fetch a URL as readable text.

use super::{ToolCtx, ToolOutput};
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_MAX_BYTES: usize = 200_000;

pub async fn fetch_url(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let url = args
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::msg("missing required argument `url`"))?
        .to_string();
    let max_bytes =
        args.get("max_bytes").and_then(Value::as_u64).unwrap_or(DEFAULT_MAX_BYTES as u64) as usize;

    let parsed = reqwest::Url::parse(&url).map_err(|e| AppError::msg(format!("bad url: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Ok(ToolOutput::error("only http and https URLs can be fetched"));
    }

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .gzip(true)
        .brotli(true)
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(super::web::BROWSER_UA)
        .build()?;

    let resp = tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => return Err(AppError::Cancelled),
        r = client.get(parsed.clone()).send() => r?,
    };

    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = resp.text().await?;

    let text = if content_type.contains("html") { html_to_text(&body) } else { body };
    let truncated = text.len() > max_bytes;
    let text = if truncated { text.chars().take(max_bytes).collect() } else { text };

    Ok(ToolOutput::text(format!("{url} [{status}]\n\n{text}")).with_display(json!({
        "kind": "fetch",
        "url": url,
        "status": status.as_u16(),
        "contentType": content_type,
        "truncated": truncated,
    })))
}

const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;
/// Remote-workspace downloads buffer in memory before the SFTP write, so the
/// cap is far lower than the streamed local one.
const MAX_REMOTE_DOWNLOAD: u64 = 64 * 1024 * 1024;

/// Downloads a URL — page, image, archive, anything — to a file in the
/// workspace. Streams to disk locally; on a remote workspace the bytes are
/// pulled here first and pushed over SFTP.
pub async fn download_file(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let url = args
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::msg("missing required argument `url`"))?
        .to_string();
    let raw_path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::msg("missing required argument `path`"))?
        .to_string();

    let parsed = reqwest::Url::parse(&url).map_err(|e| AppError::msg(format!("bad url: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Ok(ToolOutput::error("only http and https URLs can be downloaded"));
    }

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(super::web::BROWSER_UA)
        .build()?;
    let mut resp = tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => return Err(AppError::Cancelled),
        r = client.get(parsed).send() => r?,
    };
    if !resp.status().is_success() {
        return Ok(ToolOutput::error(format!(
            "{url} answered {}. If you constructed this URL from memory, do not — copy the exact file URL from a search result or the hosting page (fetch_url the page first).",
            resp.status()
        )));
    }
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let cap = if ctx.remote.is_some() { MAX_REMOTE_DOWNLOAD } else { MAX_DOWNLOAD };
    if let Some(len) = resp.content_length() {
        if len > cap {
            return Ok(ToolOutput::error(format!("{url} is {len} bytes; the cap here is {cap}")));
        }
    }

    // Remote workspace: buffer, then one SFTP write.
    if let Some(r) = ctx.remote.clone() {
        let root = ctx.cwd.display().to_string();
        let rpath = crate::remote::resolve(&root, &raw_path, ctx.settings.sandbox_files)?;
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let chunk = tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => return Err(AppError::Cancelled),
                c = resp.chunk() => c?,
            };
            let Some(chunk) = chunk else { break };
            buf.extend_from_slice(&chunk);
            if buf.len() as u64 > cap {
                return Ok(ToolOutput::error(format!("{url} exceeded the {cap}-byte remote cap mid-stream")));
            }
        }
        let n = buf.len();
        r.write(&rpath, &buf).await?;
        return Ok(ToolOutput::text(format!(
            "Downloaded {url} to {rpath} on {} ({n} bytes, {content_type})",
            r.host.label
        ))
        .with_display(
            json!({ "kind": "download", "url": url, "path": rpath, "bytes": n, "remote": r.host.label }),
        ));
    }

    let path = super::resolve_path(&ctx, &raw_path)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::File::create(&path).await.map_err(|e| AppError::msg(e.to_string()))?;
    let mut n: u64 = 0;
    loop {
        let chunk = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => return Err(AppError::Cancelled),
            c = resp.chunk() => c?,
        };
        let Some(chunk) = chunk else { break };
        n += chunk.len() as u64;
        if n > cap {
            let _ = tokio::fs::remove_file(&path).await;
            return Ok(ToolOutput::error(format!("{url} exceeded the {cap}-byte cap mid-stream")));
        }
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|e| AppError::msg(e.to_string()))?;
    }
    tokio::io::AsyncWriteExt::flush(&mut file).await.map_err(|e| AppError::msg(e.to_string()))?;

    Ok(ToolOutput::text(format!(
        "Downloaded {url} to {} ({n} bytes, {content_type})",
        super::display_path(&ctx, &path)
    ))
    .with_display(json!({
        "kind": "download",
        "url": url,
        "path": super::display_path(&ctx, &path),
        "bytes": n,
        "contentType": content_type,
    })))
}

/// Strips tags, scripts and styles so the model reads prose instead of markup.
/// Deliberately crude: a full HTML parser is a large dependency for a tool whose
/// output is fed to a model that tolerates rough text.
///
/// Every index this walks is a char boundary: `i` only ever advances by a
/// char's UTF-8 width or by offsets found in `lower`, whose ASCII-only
/// lowercasing keeps the exact byte layout of `html`. The previous version
/// stepped byte-by-byte and sliced mid-character on any multibyte page —
/// a panic that took the whole graph run down with it.
pub(crate) fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    let mut in_tag = false;

    while i < html.len() {
        let Some(c) = html[i..].chars().next() else { break };
        if c == '<' {
            let rest = &lower[i..];
            // Script and style bodies are skipped wholesale; an unclosed one
            // means the remainder is code, not prose — stop there.
            let skip = if rest.starts_with("<script") {
                Some("</script>")
            } else if rest.starts_with("<style") {
                Some("</style>")
            } else {
                None
            };
            if let Some(close) = skip {
                match rest.find(close) {
                    Some(off) => {
                        i += off + close.len();
                        continue;
                    }
                    None => break,
                }
            }
            if rest.starts_with("<br")
                || rest.starts_with("</p")
                || rest.starts_with("</div")
                || rest.starts_with("</h")
                || rest.starts_with("</li")
            {
                out.push('\n');
            }
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
        i += c.len_utf8();
    }

    let decoded = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");

    // Collapse the sea of blank lines that tag stripping leaves behind.
    let mut result = String::with_capacity(decoded.len());
    let mut blanks = 0;
    for line in decoded.lines() {
        let t = line.trim();
        if t.is_empty() {
            blanks += 1;
            if blanks <= 1 {
                result.push('\n');
            }
        } else {
            blanks = 0;
            result.push_str(t);
            result.push('\n');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape that used to panic: multibyte characters inside a
    /// script body and in the prose, walked with byte offsets.
    #[test]
    fn html_to_text_survives_multibyte_pages() {
        let html = "<html><head><script>const s = \"日本語テキスト — ünïcode\";</script>\
                    <style>/* émojis 🎨 in comments */</style></head>\
                    <body><p>Voxel — octree lighting ✓</p><div>ambient occlusion</div></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Voxel — octree lighting ✓"), "prose lost: {text:?}");
        assert!(text.contains("ambient occlusion"));
        assert!(!text.contains("日本語"), "script body leaked");
        assert!(!text.contains("🎨"), "style body leaked");
    }

    #[test]
    fn html_to_text_handles_unclosed_script() {
        let text = html_to_text("<p>before</p><script>let x = 'né…");
        assert!(text.contains("before"));
        assert!(!text.contains("né"));
    }
}

//! Tools that talk to the user instead of the machine.

use super::{ToolCtx, ToolOutput};
use crate::agent::AgentEvent;
use crate::error::{AppError, Result};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::oneshot;

/// Asks the user a question and blocks the run until they answer.
///
/// This is the model's escape hatch from guessing: options plus a recommended
/// choice render as buttons, free text is allowed unless disabled, and the
/// chosen answer comes back as the tool result. Unbounded — a question does not
/// expire — but fully cancel-aware.
pub async fn ask_user(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let question = args
        .get("question")
        .and_then(Value::as_str)
        .filter(|q| !q.trim().is_empty())
        .ok_or_else(|| AppError::msg("missing required argument `question`"))?
        .to_string();
    let options: Vec<String> = args
        .get("options")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    let recommended = args.get("recommended").and_then(Value::as_str).map(str::to_owned);
    let allow_free_text = args.get("allow_free_text").and_then(Value::as_bool).unwrap_or(true);

    // A recommendation that is not one of the options is a model slip; surface
    // it as an option rather than dropping it.
    let mut options = options;
    if let Some(rec) = &recommended {
        if !options.iter().any(|o| o == rec) {
            options.insert(0, rec.clone());
        }
    }

    let id = crate::db::new_id();
    let (tx, rx) = oneshot::channel();
    ctx.handle.questions.lock().insert(id.clone(), tx);

    let _ = ctx.handle.tx.send(AgentEvent::UserQuestion {
        id: id.clone(),
        question: question.clone(),
        options,
        recommended,
        allow_free_text,
    });

    let answer = tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => {
            ctx.handle.questions.lock().remove(&id);
            return Err(AppError::Cancelled);
        }
        a = rx => a.map_err(|_| AppError::Cancelled)?,
    };

    Ok(ToolOutput::text(format!("The user answered: {answer}")))
}

/// Writes or replaces the plan for this chat.
///
/// The plan is a living markdown document rendered in its own pane beside the
/// conversation — the model is told to keep it current, ticking checkboxes as
/// steps land. Replacing the whole document each time keeps the tool trivial
/// for the model to use correctly; diffing a plan is not worth the failure
/// modes.
pub async fn plan(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let markdown = args
        .get("markdown")
        .and_then(Value::as_str)
        .filter(|m| !m.trim().is_empty())
        .ok_or_else(|| AppError::msg("missing required argument `markdown`"))?
        .to_string();
    let title = args
        .get("title")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| first_heading(&markdown))
        .unwrap_or_else(|| "Plan".to_string());

    let doc = serde_json::json!({
        "title": title,
        "markdown": markdown,
        "updatedAt": chrono::Utc::now().timestamp_millis(),
    });
    ctx.db.set_meta(&format!("plan:{}", ctx.chat_id), &doc.to_string())?;

    let _ = ctx.handle.tx.send(AgentEvent::PlanUpdated {
        chat_id: ctx.chat_id.clone(),
        title: title.clone(),
        markdown: markdown.clone(),
    });

    let steps = markdown.matches("- [").count();
    let done = markdown.matches("- [x]").count() + markdown.matches("- [X]").count();
    Ok(ToolOutput::text(format!(
        "Plan \"{title}\" is on screen{}. Keep it current: call this tool again with the \
         full updated document as steps complete or the approach changes.",
        if steps > 0 { format!(" ({done}/{steps} steps done)") } else { String::new() }
    ))
    .with_display(serde_json::json!({ "kind": "plan", "title": title, "steps": steps, "done": done })))
}

const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "jfif", "gif", "webp", "avif", "bmp", "ico", "svg", "tif", "tiff", "heic", "heif",
];
const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// Shows an image in the conversation.
///
/// The model produces plots, screenshots and diagrams as files; this is how
/// one gets in front of the user without them digging through the file
/// browser. The transcript renders it inline from the display payload — the
/// bytes never enter the context window, which is the point.
pub async fn share_image(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let raw = args
        .get("path")
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| AppError::msg("missing required argument `path`"))?;
    let caption = args.get("caption").and_then(Value::as_str).map(str::trim).filter(|c| !c.is_empty());

    // A remote image is pulled into a local cache first: the transcript's
    // asset protocol can only serve this machine's disk.
    let path = match &ctx.remote {
        Some(r) => {
            let root = ctx.cwd.display().to_string();
            let rp = crate::remote::resolve(&root, raw, ctx.settings.sandbox_files)?;
            let bytes = r.read(&rp, MAX_IMAGE_BYTES).await?;
            let name = rp.rsplit('/').next().unwrap_or("image").to_string();
            let dir = crate::db::data_dir().join("remote-shared");
            std::fs::create_dir_all(&dir).map_err(|e| AppError::msg(e.to_string()))?;
            let local = dir.join(format!("{}-{name}", &ctx.call_id[..8.min(ctx.call_id.len())]));
            std::fs::write(&local, &bytes).map_err(|e| AppError::msg(e.to_string()))?;
            local
        }
        None => super::resolve_path(&ctx, raw)?,
    };
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
    if !IMAGE_EXTS.contains(&ext.as_str()) {
        return Err(AppError::msg(format!(
            "{} does not look like an image (supported: {})",
            path.display(),
            IMAGE_EXTS.join(", ")
        )));
    }
    let meta = std::fs::metadata(&path).map_err(|e| AppError::msg(format!("{}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(AppError::msg(format!("{} is not a file", path.display())));
    }
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(AppError::msg(format!(
            "{} is {} bytes; images above {MAX_IMAGE_BYTES} are not shown inline",
            path.display(),
            meta.len()
        )));
    }

    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| raw.to_string());
    Ok(ToolOutput::text(format!(
        "Showing {name} ({} KB) to the user in the conversation{}",
        meta.len() / 1024,
        caption.map(|c| format!(" with caption: {c}")).unwrap_or_else(|| ".".into())
    ))
    .with_display(serde_json::json!({
        "kind": "image",
        "path": path.display().to_string(),
        "name": name,
        "caption": caption,
        "bytes": meta.len(),
    })))
}

/// The marker line the vision plumbing greps for to attach the file to the
/// model's next step. Keep the format stable: `Image attached for viewing: <path> …`.
pub const VIEWED_MARKER: &str = "Image attached for viewing: ";

/// Raster formats providers accept as image input. SVG is text — the model
/// reads it with read_file; HEIC/AVIF are widely rejected.
const VIEWABLE_EXTS: &[&str] = &["png", "jpg", "jpeg", "jfif", "gif", "webp"];
const MAX_VIEW_BYTES: u64 = 6 * 1024 * 1024;

/// Lets the model LOOK at an image file: the result carries a marker the
/// runner recognises, and the file is attached to the next request as a real
/// image part (when the model reads images). The user sees which image the
/// model is looking at via the display payload. This is the missing half of
/// share_image — that one shows the user, this one shows the model.
pub async fn view_image(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let raw = args
        .get("path")
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| AppError::msg("missing required argument `path`"))?;

    // Remote workspaces: pull the file into a local cache the runner can read.
    let path = match &ctx.remote {
        Some(r) => {
            let root = ctx.cwd.display().to_string();
            let rp = crate::remote::resolve(&root, raw, ctx.settings.sandbox_files)?;
            let bytes = r.read(&rp, MAX_VIEW_BYTES).await?;
            let name = rp.rsplit('/').next().unwrap_or("image").to_string();
            let dir = crate::db::data_dir().join("remote-shared");
            std::fs::create_dir_all(&dir).map_err(|e| AppError::msg(e.to_string()))?;
            let local = dir.join(format!("{}-{name}", &ctx.call_id[..8.min(ctx.call_id.len())]));
            std::fs::write(&local, &bytes).map_err(|e| AppError::msg(e.to_string()))?;
            local
        }
        None => super::resolve_path(&ctx, raw)?,
    };

    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_lowercase).unwrap_or_default();
    if ext == "svg" {
        return Ok(ToolOutput::error(format!(
            "{} is an SVG — that is text, not pixels. Read it with read_file; to see it rendered, use web_screenshot on the file.",
            path.display()
        )));
    }
    if !VIEWABLE_EXTS.contains(&ext.as_str()) {
        return Ok(ToolOutput::error(format!(
            "{} is not a viewable raster image (supported: {}). Convert it first (run_python with PIL, or `sips` on macOS).",
            path.display(),
            VIEWABLE_EXTS.join(", ")
        )));
    }
    let meta = std::fs::metadata(&path).map_err(|e| AppError::msg(format!("{}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(AppError::msg(format!("{} is not a file", path.display())));
    }
    if meta.len() > MAX_VIEW_BYTES {
        return Ok(ToolOutput::error(format!(
            "{} is {} bytes; the viewing cap is {MAX_VIEW_BYTES}. Downscale it first (run_python with PIL, or `sips -Z 1600` on macOS).",
            path.display(),
            meta.len()
        )));
    }

    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| raw.to_string());
    Ok(ToolOutput::text(format!(
        "{VIEWED_MARKER}{} ({} KB). If the current model reads images, it arrives as a real image on your next step — describe and judge what you actually see, not what the filename implies.",
        path.display(),
        meta.len() / 1024
    ))
    .with_display(serde_json::json!({
        "kind": "image",
        "path": path.display().to_string(),
        "name": name,
        "caption": "the agent is looking at this image",
        "bytes": meta.len(),
    })))
}

/// First `#`-style heading, as a title fallback.
fn first_heading(markdown: &str) -> Option<String> {
    markdown
        .lines()
        .find_map(|l| l.trim().strip_prefix('#'))
        .map(|h| h.trim_start_matches('#').trim().to_string())
        .filter(|h| !h.is_empty())
}

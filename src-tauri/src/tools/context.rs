//! Context-management tools.

use super::{ToolCtx, ToolOutput};
use crate::error::Result;
use serde_json::Value;
use std::sync::Arc;

/// Lets the model shed its own dead weight mid-run: the next step reloads
/// the pruned history from the database automatically.
pub async fn compact_context(ctx: Arc<ToolCtx>, _args: Value) -> Result<ToolOutput> {
    match crate::agent::compact::compact_chat(ctx.state.clone(), &ctx.chat_id, None).await {
        Ok(o) => Ok(ToolOutput::text(format!(
            "Compacted: {} older message(s) folded into a summary; the last {} kept verbatim. Continue from the summary — it is the first message now.",
            o.removed, o.kept
        ))),
        Err(e) => Ok(ToolOutput::error(format!("Could not compact: {e}"))),
    }
}

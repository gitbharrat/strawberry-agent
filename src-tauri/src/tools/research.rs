//! Deep research: an iterative search-and-read loop as a single tool.
//!
//! Single-pass retrieval scores dismally on multi-hop questions; what works —
//! per the published evaluations of research agents — is the loop: plan
//! sub-queries, search, read the actual pages, notice what is still unknown,
//! search again. This tool runs that loop in a sub-agent restricted to
//! `web_search` + `fetch_url`, so a hundred pages of intermediate reading
//! never enters the parent's context — only the final cited report does.
//!
//! Citation fidelity is the documented failure mode of research agents, so the
//! prompt is blunt about it: claims carry [n] markers, every marker resolves to
//! a fetched URL, and a source that was only seen as a snippet may not be cited.

use super::{subagent::run_inner_loop, ToolCtx, ToolOutput};
use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::sync::Arc;

/// Tool rounds per depth. Each round is a model turn that can batch several
/// searches or fetches, so even "quick" reads multiple sources.
fn steps_for(depth: &str) -> u64 {
    match depth {
        "quick" => 8,
        "thorough" => 22,
        _ => 14,
    }
}

const RESEARCH_TOOLS: &[&str] = &["web_search", "fetch_url"];

pub async fn deep_research(ctx: Arc<ToolCtx>, args: Value) -> Result<ToolOutput> {
    let question = args
        .get("question")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .ok_or_else(|| AppError::msg("missing required argument `question`"))?
        .to_string();
    let depth = args.get("depth").and_then(Value::as_str).unwrap_or("standard").to_string();
    let model = args
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| ctx.settings.default_model.clone());
    let steps = steps_for(&depth);

    let system = format!(
        "You are a research agent. Answer this question from the live web:\n\n{question}\n\n\
         Method — follow it, in order:\n\
         1. If the question is ambiguous, state the interpretation you are answering in one line, then proceed.\n\
         2. Break the question into 2-5 sub-questions and run a web_search for each. Vary phrasing; \
            prefer specific terms over generic ones.\n\
         3. Pick the most credible, most diverse results — official docs, primary sources, recognised \
            publications — and fetch_url them. Snippets are not evidence; never rely on one.\n\
         4. After reading, list for yourself what is still unknown or conflicting, and search again. \
            Repeat until new searches stop changing the answer or you approach the step budget.\n\
         5. Cross-check every load-bearing claim against at least two independent fetched sources when \
            possible; where sources disagree, say so explicitly rather than picking silently.\n\n\
         Report format (your final message, returned verbatim):\n\
         - Start with the direct answer in 2-4 sentences.\n\
         - Then organised findings; every factual claim carries a citation marker like [1].\n\
         - Note dates: prefer recent sources and write \"as of <date>\" where currency matters.\n\
         - End with a numbered Sources list mapping each [n] to the full URL you actually fetched. \
           Citing a page you did not fetch is fabrication — do not do it.\n\
         - If the web gave no reliable answer, say exactly that and show what you checked."
    );

    let (answer, tools_used) =
        run_inner_loop(&ctx, system, question.clone(), model.clone(), steps, RESEARCH_TOOLS).await?;

    match answer {
        Some(report) => Ok(ToolOutput::text(report).with_display(json!({
            "kind": "research",
            "question": question,
            "depth": depth,
            "model": model,
            "toolsUsed": tools_used,
        }))),
        None => Ok(ToolOutput::error(format!(
            "Research hit its step budget ({steps} rounds) before converging. Re-run with depth \
             \"thorough\", or narrow the question."
        ))),
    }
}

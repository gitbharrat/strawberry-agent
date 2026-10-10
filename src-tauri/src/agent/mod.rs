//! Agent runtime: the loop that turns a user message into model output, tool
//! calls, and more model output, for as long as it takes.

pub mod compact;
pub mod prompt;
pub mod runner;

use crate::error::{AppError, Result};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

/// Everything the UI learns about a run arrives as one of these.
///
/// The variants are deliberately fine-grained: the renderer appends deltas to a
/// single text node rather than re-deriving a message from a snapshot, which is
/// what keeps streaming free of layout thrash.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentEvent {
    #[serde(rename_all = "camelCase")]
    RunStarted { run_id: String, chat_id: String, model: String },
    #[serde(rename_all = "camelCase")]
    StepStarted { step: u32 },
    #[serde(rename_all = "camelCase")]
    MessageStarted { message_id: String, role: String },
    #[serde(rename_all = "camelCase")]
    Delta { message_id: String, text: String },
    #[serde(rename_all = "camelCase")]
    Reasoning { message_id: String, text: String },
    #[serde(rename_all = "camelCase")]
    ToolCallStarted { id: String, name: String },
    #[serde(rename_all = "camelCase")]
    ToolCallArgs { id: String, delta: String },
    #[serde(rename_all = "camelCase")]
    ToolApproval { id: String, name: String, args: Value, risk: String, warning: Option<String> },
    /// The plan document for this chat changed; the UI shows it in a pane.
    #[serde(rename_all = "camelCase")]
    PlanUpdated { chat_id: String, title: String, markdown: String },
    /// The model is asking the user something and the run is paused on it.
    #[serde(rename_all = "camelCase")]
    UserQuestion {
        id: String,
        question: String,
        options: Vec<String>,
        recommended: Option<String>,
        allow_free_text: bool,
    },
    #[serde(rename_all = "camelCase")]
    ToolStarted { id: String, name: String, args: Value },
    #[serde(rename_all = "camelCase")]
    ToolFinished {
        id: String,
        name: String,
        ok: bool,
        summary: String,
        display: Option<Value>,
        duration_ms: i64,
    },
    #[serde(rename_all = "camelCase")]
    MessageFinished {
        message_id: String,
        content: String,
        reasoning: Option<String>,
        finish_reason: Option<String>,
        usage: Option<Value>,
    },
    #[serde(rename_all = "camelCase")]
    Usage { prompt_tokens: i64, completion_tokens: i64, cost: f64 },
    #[serde(rename_all = "camelCase")]
    Retry { attempt: u32, delay_ms: u64, reason: String },
    /// Non-fatal information worth showing inline (context trimmed, tool
    /// skipped, model fell back).
    #[serde(rename_all = "camelCase")]
    Notice { level: String, text: String },
    #[serde(rename_all = "camelCase")]
    TitleChanged { chat_id: String, title: String },
    // ---- Model Execution Graph. Graph runs are ordinary runs, so their
    // events ride the same broadcast; the chat view ignores these and the
    // graph pane ignores most of the others.
    /// `nodes` is the run's roster — `[{id, label, kind}]` in flow order — so
    /// the transcript can narrate the pipeline without loading the graph.
    #[serde(rename_all = "camelCase")]
    GraphStarted { graph_id: String, name: String, nodes: Value },
    /// A sketch node reached the flow: the run is parked until the user draws
    /// in the sketch pad and sends it. Resolved via the question machinery.
    #[serde(rename_all = "camelCase")]
    SketchRequest { id: String, node_id: String, prompt: String },
    #[serde(rename_all = "camelCase")]
    GraphNode { node_id: String, status: String, detail: Option<String> },
    #[serde(rename_all = "camelCase")]
    GraphNodeDelta { node_id: String, text: String },
    #[serde(rename_all = "camelCase")]
    GraphNodeDone { node_id: String, status: String, artifact: Value, verdict: Option<Value> },
    #[serde(rename_all = "camelCase")]
    GraphLoop { edge_id: String, iteration: u32, max_iters: u32 },
    #[serde(rename_all = "camelCase")]
    RunFinished { run_id: String, status: String, error: Option<String> },
}

pub struct RunHandle {
    pub chat_id: String,
    pub cancel: CancellationToken,
    pub tx: broadcast::Sender<AgentEvent>,
    /// Pending approval prompts, keyed by tool-call id.
    pub approvals: Mutex<HashMap<String, oneshot::Sender<bool>>>,
    /// Pending questions to the user, keyed by question id.
    pub questions: Mutex<HashMap<String, oneshot::Sender<String>>>,
}

/// Live runs, keyed by run id.
///
/// A run outlives any particular UI subscription: closing and reopening a chat,
/// or reloading the webview, reattaches to the same broadcast rather than
/// interrupting the work.
#[derive(Default)]
pub struct RunRegistry {
    runs: Mutex<HashMap<String, Arc<RunHandle>>>,
}

impl RunRegistry {
    pub fn start(&self, run_id: &str, chat_id: &str) -> Arc<RunHandle> {
        // 512 slots is roughly eight seconds of coalesced events; a subscriber
        // that falls further behind than that gets a lag notice and resyncs
        // from the database rather than stalling the producer.
        let (tx, _) = broadcast::channel(512);
        let handle = Arc::new(RunHandle {
            chat_id: chat_id.to_string(),
            cancel: CancellationToken::new(),
            tx,
            approvals: Mutex::new(HashMap::new()),
            questions: Mutex::new(HashMap::new()),
        });
        self.runs.lock().insert(run_id.to_string(), handle.clone());
        handle
    }

    pub fn get(&self, run_id: &str) -> Option<Arc<RunHandle>> {
        self.runs.lock().get(run_id).cloned()
    }

    pub fn finish(&self, run_id: &str) {
        self.runs.lock().remove(run_id);
    }

    pub fn cancel(&self, run_id: &str) -> Result<()> {
        let h = self
            .runs
            .lock()
            .get(run_id)
            .cloned()
            .ok_or_else(|| AppError::NotFound(format!("run {run_id}")))?;
        h.cancel.cancel();
        // Unblock anything waiting on an approval or a question.
        for (_, tx) in h.approvals.lock().drain() {
            let _ = tx.send(false);
        }
        h.questions.lock().drain().for_each(drop);
        Ok(())
    }

    pub fn cancel_chat(&self, chat_id: &str) {
        let ids: Vec<String> =
            self.runs.lock().iter().filter(|(_, h)| h.chat_id == chat_id).map(|(id, _)| id.clone()).collect();
        for id in ids {
            let _ = self.cancel(&id);
        }
    }

    pub fn cancel_all(&self) {
        let ids: Vec<String> = self.runs.lock().keys().cloned().collect();
        for id in ids {
            let _ = self.cancel(&id);
        }
    }

    pub fn resolve_question(&self, run_id: &str, question_id: &str, answer: String) -> Result<()> {
        let h = self.get(run_id).ok_or_else(|| AppError::NotFound(format!("run {run_id}")))?;
        let tx = h
            .questions
            .lock()
            .remove(question_id)
            .ok_or_else(|| AppError::NotFound(format!("question {question_id}")))?;
        let _ = tx.send(answer);
        Ok(())
    }

    pub fn resolve_approval(&self, run_id: &str, call_id: &str, approved: bool) -> Result<()> {
        let h = self.get(run_id).ok_or_else(|| AppError::NotFound(format!("run {run_id}")))?;
        let tx = h
            .approvals
            .lock()
            .remove(call_id)
            .ok_or_else(|| AppError::NotFound(format!("approval {call_id}")))?;
        let _ = tx.send(approved);
        Ok(())
    }

    pub fn active_ids(&self) -> Vec<String> {
        self.runs.lock().keys().cloned().collect()
    }

    pub fn is_chat_running(&self, chat_id: &str) -> bool {
        self.runs.lock().values().any(|h| h.chat_id == chat_id)
    }
}

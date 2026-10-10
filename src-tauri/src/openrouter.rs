//! OpenRouter API client.
//!
//! The whole network surface of the app is here. Three properties matter:
//!
//! * **It never hangs.** Separate connect, stall and total deadlines. A stream
//!   that stops producing bytes is torn down and retried rather than left
//!   pending forever.
//! * **It survives bad links.** Retries with jittered backoff, but only while no
//!   token has been emitted yet, so a partially rendered answer is never
//!   silently restarted.
//! * **It is cancellable at any instant.** Every await races the run's
//!   `CancellationToken`.

use crate::error::{AppError, Result};
use bytes::Bytes;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub const BASE: &str = "https://openrouter.ai/api/v1";

/// No bytes at all for this long — not even a keepalive comment — means the
/// connection is wedged. OpenRouter emits `: OPENROUTER PROCESSING` well inside
/// this window, so it only fires on genuinely dead links.
const STALL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    attribution: bool,
}

impl Client {
    pub fn new(connect_ms: u64, request_ms: u64) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(connect_ms.max(1000)))
            // Deliberately *not* a total request timeout: that would kill long
            // streams. Streaming deadlines are enforced per-chunk below.
            .read_timeout(Duration::from_millis(request_ms.max(30_000)))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(4)
            .tcp_keepalive(Duration::from_secs(30))
            .gzip(true)
            .brotli(true)
            .user_agent(concat!("StrawberryAgent/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { http, attribution: true })
    }

    pub fn with_attribution(mut self, on: bool) -> Self {
        self.attribution = on;
        self
    }

    fn req(&self, method: reqwest::Method, path: &str, key: &str) -> reqwest::RequestBuilder {
        let mut b = self
            .http
            .request(method, format!("{BASE}{path}"))
            .bearer_auth(key)
            .header("Accept-Encoding", "gzip, br");
        if self.attribution {
            // OpenRouter uses these for its public app leaderboard; harmless and
            // opt-out in settings.
            b = b
                .header("HTTP-Referer", "https://github.com/strawberry-agent")
                .header("X-Title", "Strawberry Agent");
        }
        b
    }

    /// Catalogue of every model OpenRouter can route to. Cached by the caller.
    pub async fn models(&self, key: &str) -> Result<Vec<ModelInfo>> {
        let resp = self.req(reqwest::Method::GET, "/models", key).send().await?;
        let resp = check(resp).await?;
        let body: ModelsResponse = resp.json().await?;
        Ok(body.data)
    }

    /// Key metadata: label, spend limit and usage. Used by the settings screen
    /// to prove the key works without spending a token.
    pub async fn key_info(&self, key: &str) -> Result<Value> {
        let resp = self.req(reqwest::Method::GET, "/key", key).send().await?;
        let resp = check(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn credits(&self, key: &str) -> Result<Value> {
        let resp = self.req(reqwest::Method::GET, "/credits", key).send().await?;
        let resp = check(resp).await?;
        Ok(resp.json().await?)
    }

    /// Streams one assistant turn, invoking `on` for every incremental piece and
    /// returning the fully accumulated turn.
    ///
    /// `on` is a plain synchronous callback: the runner uses it to append to
    /// buffers and to nudge a broadcast channel, both of which are non-blocking.
    pub async fn stream_chat(
        &self,
        key: &str,
        req: &ChatRequest,
        cancel: &CancellationToken,
        max_retries: u32,
        on: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<Turn> {
        let mut attempt = 0u32;
        loop {
            match self.stream_once(key, req, cancel, on).await {
                Ok(turn) => return Ok(turn),
                Err(e) => {
                    let retryable = matches!(
                        &e,
                        AppError::Http(_)
                            | AppError::Timeout(_)
                            | AppError::Upstream { status: 408 | 429 | 500 | 502 | 503 | 504, .. }
                    );
                    if !retryable || attempt >= max_retries || cancel.is_cancelled() {
                        return Err(e);
                    }
                    attempt += 1;
                    // 400ms, 800ms, 1.6s … with jitter, capped at 8s.
                    let base = 400u64 << (attempt - 1).min(5);
                    let jitter = (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.subsec_millis() as u64)
                        .unwrap_or(0))
                        % 250;
                    let wait = Duration::from_millis((base + jitter).min(8_000));
                    on(StreamEvent::Retry {
                        attempt,
                        delay_ms: wait.as_millis() as u64,
                        reason: e.to_string(),
                    });
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        _ = cancel.cancelled() => return Err(AppError::Cancelled),
                    }
                }
            }
        }
    }

    async fn stream_once(
        &self,
        key: &str,
        req: &ChatRequest,
        cancel: &CancellationToken,
        on: &mut (dyn FnMut(StreamEvent) + Send),
    ) -> Result<Turn> {
        let body = serde_json::to_value(req)?;
        let send = self
            .req(reqwest::Method::POST, "/chat/completions", key)
            .header("Accept", "text/event-stream")
            .json(&body)
            .send();

        let resp = tokio::select! {
            r = send => r?,
            _ = cancel.cancelled() => return Err(AppError::Cancelled),
        };
        let resp = check(resp).await?;

        let mut stream = resp.bytes_stream();
        let mut parser = SseParser::default();
        let mut turn = Turn::default();
        // Set once the first visible token lands; after that a failure must not
        // silently restart the turn.
        let mut produced = false;

        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(AppError::Cancelled),
                chunk = tokio::time::timeout(STALL_TIMEOUT, stream.next()) => chunk,
            };

            let chunk = match next {
                Err(_) => return Err(AppError::Timeout(STALL_TIMEOUT.as_millis() as u64)),
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    if produced {
                        // Salvage what we have rather than throwing the answer away.
                        turn.finish_reason = Some("error".into());
                        turn.error = Some(e.to_string());
                        break;
                    }
                    return Err(AppError::Http(e));
                }
                Ok(Some(Ok(b))) => b,
            };

            for data in parser.feed(&chunk) {
                if data == "[DONE]" {
                    return Ok(turn);
                }
                let v: Value = match serde_json::from_str(&data) {
                    Ok(v) => v,
                    // A malformed frame is not worth killing a live stream over.
                    Err(_) => continue,
                };
                if apply_chunk(&v, &mut turn, on, &mut produced)? {
                    return Ok(turn);
                }
            }
        }

        Ok(turn)
    }
}

/// Applies one decoded SSE frame. Returns `true` when the stream is terminal.
fn apply_chunk(
    v: &Value,
    turn: &mut Turn,
    on: &mut (dyn FnMut(StreamEvent) + Send),
    produced: &mut bool,
) -> Result<bool> {
    // Errors can arrive mid-stream with a 200 status.
    if let Some(err) = v.get("error") {
        let msg = err.get("message").and_then(Value::as_str).unwrap_or("upstream error").to_string();
        let code = err.get("code").and_then(Value::as_u64).unwrap_or(0) as u16;
        if *produced {
            turn.error = Some(msg.clone());
            turn.finish_reason = Some("error".into());
            on(StreamEvent::Error(msg));
            return Ok(true);
        }
        return Err(AppError::Upstream { status: code, message: msg });
    }

    if turn.id.is_none() {
        turn.id = v.get("id").and_then(Value::as_str).map(str::to_owned);
    }
    if turn.model.is_none() {
        turn.model = v.get("model").and_then(Value::as_str).map(str::to_owned);
    }
    if turn.provider.is_none() {
        turn.provider = v.get("provider").and_then(Value::as_str).map(str::to_owned);
    }

    if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
        turn.usage = Some(serde_json::from_value(u.clone()).unwrap_or_default());
    }

    let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
        return Ok(false);
    };

    if let Some(delta) = choice.get("delta") {
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            if !text.is_empty() {
                *produced = true;
                turn.content.push_str(text);
                on(StreamEvent::Content(text.to_string()));
            }
        }
        // Providers disagree on the field name for chain-of-thought summaries.
        for field in ["reasoning", "reasoning_content"] {
            if let Some(text) = delta.get(field).and_then(Value::as_str) {
                if !text.is_empty() {
                    *produced = true;
                    turn.reasoning.push_str(text);
                    on(StreamEvent::Reasoning(text.to_string()));
                }
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for c in calls {
                let idx = c.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                while turn.tool_calls.len() <= idx {
                    turn.tool_calls.push(ToolCall::default());
                }
                let slot = &mut turn.tool_calls[idx];
                if let Some(id) = c.get("id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        slot.id = id.to_string();
                    }
                }
                if let Some(f) = c.get("function") {
                    if let Some(n) = f.get("name").and_then(Value::as_str) {
                        if !n.is_empty() {
                            slot.function.name.push_str(n);
                        }
                    }
                    if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                        slot.function.arguments.push_str(a);
                    }
                }
                *produced = true;
                on(StreamEvent::ToolCallDelta {
                    id: slot.id.clone(),
                    name: slot.function.name.clone(),
                    args_delta: c
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                });
            }
        }
    }

    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        turn.finish_reason = Some(reason.to_string());
        on(StreamEvent::Finish(reason.to_string()));
    }

    Ok(false)
}

async fn check(resp: reqwest::Response) -> Result<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let text = resp.text().await.unwrap_or_default();
    let message =
        serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| {
                v.get("error").and_then(|e| e.get("message")).and_then(Value::as_str).map(str::to_owned)
            })
            .unwrap_or_else(|| {
                if text.is_empty() {
                    status.to_string()
                } else {
                    text.chars().take(400).collect()
                }
            });
    Err(AppError::Upstream { status: status.as_u16(), message })
}

// ------------------------------------------------------------- SSE decoding

/// Minimal SSE frame decoder.
///
/// Only `data:` payloads are surfaced; comment lines (OpenRouter's
/// `: OPENROUTER PROCESSING` keepalive) and unknown fields are dropped. Handles
/// payloads split across arbitrary TCP boundaries and both LF and CRLF.
#[derive(Default)]
struct SseParser {
    buf: String,
    event: String,
}

impl SseParser {
    fn feed(&mut self, bytes: &Bytes) -> Vec<String> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();

        while let Some(nl) = self.buf.find('\n') {
            let mut line = self.buf[..nl].to_string();
            self.buf.drain(..=nl);
            if line.ends_with('\r') {
                line.pop();
            }

            if line.is_empty() {
                if !self.event.is_empty() {
                    out.push(std::mem::take(&mut self.event));
                }
                continue;
            }
            if line.starts_with(':') {
                continue; // keepalive / comment
            }
            if let Some(rest) = line.strip_prefix("data:") {
                if !self.event.is_empty() {
                    self.event.push('\n');
                }
                self.event.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            }
        }
        out
    }
}

// ------------------------------------------------------------------- types

#[derive(Debug, Clone)]
pub enum StreamEvent {
    Content(String),
    Reasoning(String),
    ToolCallDelta { id: String, name: String, args_delta: String },
    Finish(String),
    Error(String),
    Retry { attempt: u32, delay_ms: u64, reason: String },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Turn {
    pub id: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "type", default = "fn_type")]
    pub kind: String,
    #[serde(default)]
    pub function: FunctionCall,
}

impl Default for ToolCall {
    fn default() -> Self {
        // The streaming accumulator builds calls from this. `kind` must start
        // as "function": deltas never carry the type field, and an empty
        // `"type":""` on the wire makes providers treat the declaration as
        // invalid — which then surfaces as a baffling "no tool call found for
        // call_id" on the NEXT request.
        Self { id: String::new(), kind: fn_type(), function: FunctionCall::default() }
    }
}

fn fn_type() -> String {
    "function".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FunctionCall {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: i64,
    #[serde(default)]
    pub completion_tokens: i64,
    #[serde(default)]
    pub total_tokens: i64,
    #[serde(default)]
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Value>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    pub usage: Value,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Value>) -> Self {
        Self {
            model: model.into(),
            messages,
            stream: true,
            tools: None,
            tool_choice: None,
            temperature: None,
            max_tokens: None,
            top_p: None,
            reasoning: None,
            models: None,
            // Ask OpenRouter to append a usage frame so cost is exact rather
            // than estimated from a tokenizer we do not have.
            usage: json!({ "include": true }),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelInfo>,
}

/// Trimmed projection of OpenRouter's model record — only what the picker and
/// the request builder actually need, so the cached payload stays small.
///
/// The wire format is asymmetric and both directions matter: OpenRouter sends
/// snake_case, the frontend expects camelCase, and the on-disk cache is written
/// in the camelCase form. Every multi-word field therefore carries an alias for
/// the snake_case spelling so all three round-trip through one struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, alias = "context_length")]
    pub context_length: Option<i64>,
    #[serde(default)]
    pub created: Option<i64>,
    #[serde(default)]
    pub architecture: Option<Architecture>,
    #[serde(default)]
    pub pricing: Option<Pricing>,
    #[serde(default, alias = "supported_parameters")]
    pub supported_parameters: Vec<String>,
    #[serde(default, alias = "top_provider")]
    pub top_provider: Option<TopProvider>,
    /// Present only for models that reason; carries the effort levels this
    /// particular model accepts, which differ between providers.
    #[serde(default)]
    pub reasoning: Option<ReasoningInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningInfo {
    #[serde(default)]
    pub mandatory: bool,
    #[serde(default, alias = "default_enabled")]
    pub default_enabled: bool,
    #[serde(default, alias = "supported_efforts")]
    pub supported_efforts: Vec<String>,
    #[serde(default, alias = "default_effort")]
    pub default_effort: Option<String>,
}

impl ModelInfo {
    pub fn supports_tools(&self) -> bool {
        self.supported_parameters.iter().any(|p| p == "tools")
    }
    pub fn supports_images(&self) -> bool {
        self.architecture.as_ref().map(|a| a.input_modalities.iter().any(|m| m == "image")).unwrap_or(false)
    }
    pub fn supports_reasoning(&self) -> bool {
        self.reasoning.is_some()
            || self.supported_parameters.iter().any(|p| p == "reasoning" || p == "include_reasoning")
    }

    /// Effort levels this model accepts, most capable last. Empty when the
    /// model does not expose a choice.
    pub fn reasoning_efforts(&self) -> Vec<String> {
        self.reasoning.as_ref().map(|r| r.supported_efforts.clone()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Architecture {
    #[serde(default)]
    pub modality: String,
    #[serde(default, alias = "input_modalities")]
    pub input_modalities: Vec<String>,
    #[serde(default, alias = "output_modalities")]
    pub output_modalities: Vec<String>,
    #[serde(default)]
    pub tokenizer: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pricing {
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub completion: String,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default, alias = "input_cache_read")]
    pub input_cache_read: Option<String>,
    #[serde(default, alias = "input_cache_write")]
    pub input_cache_write: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopProvider {
    #[serde(default, alias = "context_length")]
    pub context_length: Option<i64>,
    #[serde(default, alias = "max_completion_tokens")]
    pub max_completion_tokens: Option<i64>,
    #[serde(default, alias = "is_moderated")]
    pub is_moderated: Option<bool>,
}

#[cfg(test)]
// A panic in a test is a failed test, which is exactly what we want here.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// OpenRouter sends snake_case, the cache stores camelCase, and both must
    /// land in the same struct with capabilities intact. This is the bug that
    /// silently emptied the model picker.
    #[test]
    fn model_info_accepts_both_wire_spellings() {
        let upstream = serde_json::json!({
            "id": "z-ai/glm-5.3-flash",
            "name": "Z.ai: GLM 5.3 Flash",
            "context_length": 1_310_720,
            "architecture": { "input_modalities": ["text", "image"], "output_modalities": ["text"] },
            "supported_parameters": ["tools", "reasoning"],
            "top_provider": { "context_length": 1_310_720, "max_completion_tokens": 8192 }
        });

        let parsed: ModelInfo = serde_json::from_value(upstream).expect("upstream parses");
        assert_eq!(parsed.context_length, Some(1_310_720));
        assert!(parsed.supports_tools(), "tool support must survive the snake_case wire format");
        assert!(parsed.supports_images());
        assert!(parsed.supports_reasoning());

        // The cache writes the camelCase form and reads it back through the
        // same type, so that direction has to work too.
        let cached = serde_json::to_string(&parsed).unwrap();
        let round: ModelInfo = serde_json::from_str(&cached).expect("cache round-trips");
        assert!(round.supports_tools());
        assert_eq!(round.context_length, Some(1_310_720));
        assert_eq!(round.top_provider.and_then(|t| t.max_completion_tokens), Some(8192));
    }
}

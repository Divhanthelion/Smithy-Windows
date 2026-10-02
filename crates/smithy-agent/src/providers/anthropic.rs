//! Claude, through Anthropic's own Messages API.
//!
//! Plain HTTP, like every other backend here: Anthropic has no Rust SDK, and
//! the surface the loop needs is one streaming request.
//!
//! ## What differs from the OpenAI-shaped backends
//!
//! - **The conversation is translated.** Smithy keeps an OpenAI-shaped
//!   [`History`]; Claude takes `system` at the top level, tool calls as
//!   `tool_use` blocks, and every result of one turn as `tool_result` blocks
//!   in one user message. [`to_messages`] is that translation, and it is a
//!   pure function of the history, so the same history always produces the
//!   same bytes — which is what both the prompt cache and the next point need.
//! - **Thinking goes back verbatim.** Claude's thinking blocks carry a
//!   signature bound to the conversation that produced them; editing an
//!   earlier turn invalidates every later block (enforced, as a 400, on
//!   accounts created since 31 August 2026). So an assistant turn is sent back
//!   from the blocks it arrived as ([`crate::message::Message::provider_blocks`]),
//!   this provider asks the session not to rewrite earlier file snapshots
//!   ([`Provider::stub_superseded_snapshots`] is off), and every request sets
//!   `prefix_mismatch_behavior: "drop_block"` so that a block the API cannot
//!   accept is dropped rather than failing the turn. Compact already replaces
//!   the whole history with a summary, which is the form the API accepts.
//! - **Caching is asked for.** A top-level `cache_control` caches the whole
//!   prefix on every request; an agent resends its history each step, so this
//!   is most of the cost.
//! - **Refusals are a stop reason, not an error.** On the current models a
//!   safety classifier can decline with `stop_reason: "refusal"`; the request
//!   opts into Anthropic's server-side fallback (`fallbacks: "default"`) where
//!   the model supports it, and a refusal that still comes back is shown as a
//!   plain sentence rather than an empty answer.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use smithy_tools::ToolCall;

use crate::message::{History, Message, Role};
use crate::provider::{Completion, CompletionRequest, Delta, Provider, ProviderError};
use crate::providers::lmstudio::ModelInfo;

pub const DEFAULT_URL: &str = "https://api.anthropic.com/v1";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
pub const DEFAULT_EFFORT: &str = "high";
const API_VERSION: &str = "2023-06-01";
const THINKING_BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(1800);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Anthropic pings an open stream; three minutes of nothing is a hang, even
/// for a model that thinks before it writes.
const STREAM_IDLE: Duration = Duration::from_secs(180);
/// Output budget for one reply. Streaming, so no HTTP timeout to fear, and
/// thinking counts against it: a cut-off reply costs a retry.
const MAX_TOKENS: i64 = 64_000;
/// For a model that refuses the above as too large (the older ones).
const MAX_TOKENS_SMALL: i64 = 8_192;

/// What this model accepts, decided from its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Adaptive thinking and `output_config.effort`. Off for Haiku 4.5 and
    /// the 4.5-and-earlier generation, which take neither in this form.
    pub adaptive: bool,
    /// Server-side refusal fallback (`fallbacks: "default"`).
    pub fallbacks: bool,
}

pub fn capabilities(model: &str) -> Capabilities {
    let m = model.to_ascii_lowercase();
    let legacy = m.contains("haiku")
        || m.starts_with("claude-3")
        || ["-4-5", "-4-1", "-4-0", "-4-2"]
            .iter()
            .any(|old| m.contains(old));
    let fallbacks = [
        "claude-opus-5-5",
        "claude-opus-5",
        "claude-fable-5-1",
        "claude-sonnet-5-5",
    ]
    .iter()
    .any(|id| m == *id);
    Capabilities {
        adaptive: !legacy,
        fallbacks,
    }
}

pub struct Anthropic {
    http: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
    /// `low`, `medium`, `high`, `xhigh` or `max`.
    effort: String,
    /// Set once the server refused [`MAX_TOKENS`] as too large.
    small_max_tokens: AtomicBool,
}

impl Anthropic {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
        effort: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| ProviderError::Other(format!("could not build HTTP client: {e}")))?;
        let effort = effort.into();
        Ok(Anthropic {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            api_key: api_key.into(),
            effort: if effort.trim().is_empty() {
                DEFAULT_EFFORT.to_string()
            } else {
                effort.trim().to_lowercase()
            },
            small_max_tokens: AtomicBool::new(false),
        })
    }

    fn messages_url(&self) -> String {
        format!("{}/messages", self.base_url)
    }

    fn model_url(&self) -> String {
        format!("{}/models/{}", self.base_url, self.model)
    }

    fn headers(&self, builder: reqwest::RequestBuilder, betas: &[&str]) -> reqwest::RequestBuilder {
        let builder = builder
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION);
        if betas.is_empty() {
            builder
        } else {
            builder.header("anthropic-beta", betas.join(","))
        }
    }

    /// The beta headers this model's requests need.
    fn betas(&self) -> Vec<&'static str> {
        let caps = capabilities(&self.model);
        let mut betas = Vec::new();
        if caps.adaptive {
            betas.push(THINKING_BINDING_BETA);
        }
        if caps.fallbacks {
            betas.push(FALLBACK_BETA);
        }
        betas
    }

    fn body_with(&self, request: &CompletionRequest<'_>, small: bool) -> Value {
        let caps = capabilities(&self.model);
        let (system, messages) = to_messages(request.history);
        let mut body = json!({
            "model": self.model,
            "max_tokens": if small { MAX_TOKENS_SMALL } else { MAX_TOKENS },
            "stream": true,
            "messages": messages,
            // Cache the whole prefix: an agent resends its history every step.
            "cache_control": { "type": "ephemeral" },
        });
        if let Some(system) = system {
            body["system"] = json!([{ "type": "text", "text": system }]);
        }
        let tools = to_tools(request.tools);
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools);
        }
        if caps.adaptive {
            body["thinking"] = json!({
                "type": "adaptive",
                "display": "summarized",
                "block_binding": { "prefix_mismatch_behavior": "drop_block" },
            });
            // Thinking cannot be turned off on the current models; where the
            // loop asks for none (quick lookups), the least of it.
            let effort = if request.sampling.thinking {
                self.effort.as_str()
            } else {
                "low"
            };
            body["output_config"] = json!({ "effort": effort });
        }
        if caps.fallbacks {
            body["fallbacks"] = json!("default");
        }
        body
    }

    async fn send(
        &self,
        request: &CompletionRequest<'_>,
        small: bool,
        on_delta: Option<&(dyn Fn(Delta) + Send + Sync)>,
    ) -> Result<Completion, ProviderError> {
        let response = self
            .headers(self.http.post(self.messages_url()), &self.betas())
            .json(&self.body_with(request, small))
            .timeout(request.http_timeout(REQUEST_TIMEOUT))
            .send()
            .await
            .map_err(|e| ProviderError::Unreachable {
                endpoint: self.messages_url(),
                source: Box::new(e),
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body: error_message(&body),
            });
        }
        consume_stream(response.bytes_stream(), on_delta, STREAM_IDLE).await
    }
}

#[async_trait]
impl Provider for Anthropic {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn model(&self) -> &str {
        &self.model
    }

    /// Off: rewriting an earlier turn would invalidate every later thinking
    /// block. See the module docs.
    fn stub_superseded_snapshots(&self) -> bool {
        false
    }

    async fn preflight(&self) -> Result<(), ProviderError> {
        if self.api_key.trim().is_empty() {
            return Err(ProviderError::Other(
                "Claude needs an Anthropic API key: add one in Agent > Backend Settings.".into(),
            ));
        }
        let response = self
            .headers(self.http.get(self.model_url()), &[])
            .send()
            .await
            .map_err(|e| ProviderError::Unreachable {
                endpoint: self.model_url(),
                source: Box::new(e),
            })?;
        match response.status().as_u16() {
            401 | 403 => Err(ProviderError::Http {
                status: response.status().as_u16(),
                body: format!(
                    "Anthropic rejected the API key: {}",
                    error_message(&response.text().await.unwrap_or_default())
                ),
            }),
            404 => Err(ProviderError::ModelNotLoaded {
                model: self.model.clone(),
                endpoint: self.base_url.clone(),
            }),
            _ => Ok(()),
        }
    }

    /// The context window, from `GET /v1/models/{id}` (`max_input_tokens`).
    async fn probe_model(&self) -> Result<Option<ModelInfo>, ProviderError> {
        let Ok(response) = self
            .headers(self.http.get(self.model_url()), &[])
            .send()
            .await
        else {
            return Ok(None);
        };
        if !response.status().is_success() {
            return Ok(None);
        }
        let Ok(body) = response.json::<Value>().await else {
            return Ok(None);
        };
        let context = body["max_input_tokens"].as_i64();
        Ok(Some(ModelInfo {
            key: self.model.clone(),
            found: true,
            loaded: true,
            context_length: context,
            max_context_length: context,
            trained_for_tool_use: true,
            format: "cloud".to_string(),
            quantization: "api".to_string(),
        }))
    }

    async fn complete(
        &self,
        request: CompletionRequest<'_>,
        on_delta: Option<&(dyn Fn(Delta) + Send + Sync)>,
    ) -> Result<Completion, ProviderError> {
        let small = self.small_max_tokens.load(Ordering::Relaxed);
        match self.send(&request, small, on_delta).await {
            // An older model with a smaller output cap: once, then remember.
            Err(ProviderError::Http { status: 400, body })
                if !small && body.contains("max_tokens") =>
            {
                self.small_max_tokens.store(true, Ordering::Relaxed);
                self.send(&request, true, on_delta).await
            }
            other => other,
        }
    }

    fn build_body(&self, request: &CompletionRequest<'_>) -> Value {
        self.body_with(request, self.small_max_tokens.load(Ordering::Relaxed))
    }
}

/// Smithy's tool schemas (OpenAI's `{type: "function", function: {...}}`) as
/// Claude's `{name, description, input_schema}`, in the same order every time.
///
/// `eager_input_streaming` streams large inputs (a whole file for `write`) as
/// they are written instead of after; the input then arrives unvalidated, and
/// a reply cut off mid-call leaves invalid JSON, which the loop's parser
/// reports back as a malformed call.
pub fn to_tools(tools: &Value) -> Vec<Value> {
    tools
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| {
                    let f = tool.get("function").unwrap_or(tool);
                    let name = f["name"].as_str()?;
                    Some(json!({
                        "name": name,
                        "description": f["description"].as_str().unwrap_or(""),
                        "input_schema": f
                            .get("parameters")
                            .filter(|p| p.is_object())
                            .cloned()
                            .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                        "eager_input_streaming": true,
                    }))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The history as Claude's `system` and `messages`.
///
/// Deterministic: the same history gives the same bytes, so the cached prefix
/// and the thinking blocks' bindings hold from one request to the next.
pub fn to_messages(history: &History) -> (Option<String>, Vec<Value>) {
    let mut system = None;
    let mut turns: Vec<(&'static str, Vec<Value>)> = Vec::new();
    let mut push = |role: &'static str, blocks: Vec<Value>| {
        if blocks.is_empty() {
            return;
        }
        // Consecutive turns of one role become one message: every result of
        // a step goes back together, and a note after them joins them.
        match turns.last_mut() {
            Some((last, content)) if *last == role => content.extend(blocks),
            _ => turns.push((role, blocks)),
        }
    };

    for (i, message) in history.messages().iter().enumerate() {
        match message.role {
            Role::System if i == 0 => system = Some(message.content.clone()),
            // A system message later in the history is a note to the model;
            // older Claude models take no system turn mid-conversation.
            Role::System => push(
                "user",
                vec![text_block(&format!("[system] {}", message.content))],
            ),
            Role::User => push("user", vec![text_block(&message.content)]),
            Role::Tool => push(
                "user",
                vec![json!({
                    "type": "tool_result",
                    "tool_use_id": message.tool_call_id.clone().unwrap_or_default(),
                    "content": if message.content.is_empty() {
                        "(no output)".to_string()
                    } else {
                        message.content.clone()
                    },
                })],
            ),
            Role::Assistant => push("assistant", assistant_blocks(message)),
        }
    }

    // The API wants a user turn first.
    if turns.first().is_some_and(|(role, _)| *role == "assistant") {
        turns.insert(0, ("user", vec![text_block("(continue)")]));
    }
    let messages = turns
        .into_iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect();
    (system, messages)
}

fn text_block(text: &str) -> Value {
    // An empty text block is refused; a message that would have one gets a
    // placeholder, the same one every time.
    let text = if text.trim().is_empty() {
        "(empty)"
    } else {
        text
    };
    json!({ "type": "text", "text": text })
}

/// One assistant turn's content blocks.
///
/// From the blocks it arrived as when there are any, so thinking goes back
/// unchanged, with two removals the API requires:
///
/// - a `tool_use` the session did not answer (a malformed call, a reply cut
///   off mid-call) — a call without its result is refused;
/// - after a mid-reply fallback to another model, the declined model's
///   thinking and tool calls before the last `fallback` marker (its text
///   stays), and the marker itself.
///
/// A turn with no blocks (from before this backend, or a synthesized note) is
/// built from its text and tool calls.
pub fn assistant_blocks(message: &Message) -> Vec<Value> {
    if message.provider_blocks.is_empty() {
        let mut blocks = Vec::new();
        if !message.content.trim().is_empty() {
            blocks.push(text_block(&message.content));
        }
        for call in &message.tool_calls {
            blocks.push(json!({
                "type": "tool_use",
                "id": call.id,
                "name": call.name,
                "input": serde_json::from_str::<Value>(&call.arguments)
                    .ok()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({})),
            }));
        }
        return blocks;
    }

    let answered = |id: &str| message.tool_calls.iter().any(|c| c.id == id);
    let boundary = message
        .provider_blocks
        .iter()
        .rposition(|b| b["type"] == "fallback");
    message
        .provider_blocks
        .iter()
        .enumerate()
        .filter(|(i, block)| {
            let kind = block["type"].as_str().unwrap_or_default();
            if kind == "fallback" {
                return false;
            }
            if boundary.is_some_and(|b| *i < b) && kind != "text" {
                return false;
            }
            if kind == "tool_use" {
                return answered(block["id"].as_str().unwrap_or_default());
            }
            true
        })
        .map(|(_, block)| block.clone())
        .collect()
}

/// The message out of an error body, or the body itself.
fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| {
            let mut s = body.to_string();
            s.truncate(500);
            s
        })
}

/// One content block being assembled from the stream.
struct Block {
    value: Value,
    /// `input_json_delta` fragments of a `tool_use`, joined at its stop.
    partial_json: String,
}

/// Drain Claude's SSE stream into a [`Completion`].
///
/// Bytes are buffered, not text: a multi-byte character split across two
/// network chunks must not be decoded in halves.
pub async fn consume_stream<S, B, E>(
    stream: S,
    on_delta: Option<&(dyn Fn(Delta) + Send + Sync)>,
    idle: Duration,
) -> Result<Completion, ProviderError>
where
    S: futures_util::Stream<Item = Result<B, E>>,
    B: AsRef<[u8]>,
    E: std::fmt::Display,
{
    tokio::pin!(stream);
    let mut state = StreamState::default();
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        let chunk = match tokio::time::timeout(idle, stream.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(e))) => {
                return Err(ProviderError::BadResponse(format!(
                    "stream read error: {e}"
                )))
            }
            Ok(None) => break,
            Err(_) => {
                return Err(ProviderError::BadResponse(
                    "Claude stopped sending (stream stalled)".into(),
                ))
            }
        };
        buffer.extend_from_slice(chunk.as_ref());
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let Some(data) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<Value>(data.trim()) else {
                continue;
            };
            if state.apply(&event, on_delta)? {
                return Ok(state.finish());
            }
        }
    }
    Ok(state.finish())
}

#[derive(Default)]
struct StreamState {
    blocks: Vec<Option<Block>>,
    out: Completion,
    stop_reason: String,
    refusal_category: Option<String>,
    input_tokens: i64,
    cache_creation: i64,
    cache_read: i64,
}

impl StreamState {
    /// Apply one event; `Ok(true)` at the end of the message.
    fn apply(
        &mut self,
        event: &Value,
        on_delta: Option<&(dyn Fn(Delta) + Send + Sync)>,
    ) -> Result<bool, ProviderError> {
        match event["type"].as_str().unwrap_or_default() {
            "message_start" => self.usage(&event["message"]["usage"]),
            "content_block_start" => {
                let index = event["index"].as_u64().unwrap_or(0) as usize;
                if self.blocks.len() <= index {
                    self.blocks.resize_with(index + 1, || None);
                }
                self.blocks[index] = Some(Block {
                    value: event["content_block"].clone(),
                    partial_json: String::new(),
                });
            }
            "content_block_delta" => {
                let index = event["index"].as_u64().unwrap_or(0) as usize;
                let delta = &event["delta"];
                let Some(Some(block)) = self.blocks.get_mut(index) else {
                    return Ok(false);
                };
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let text = delta["text"].as_str().unwrap_or_default();
                        append(&mut block.value, "text", text);
                        if let Some(f) = on_delta {
                            f(Delta::Content(text.to_string()));
                        }
                    }
                    "thinking_delta" => {
                        let text = delta["thinking"].as_str().unwrap_or_default();
                        append(&mut block.value, "thinking", text);
                        if let Some(f) = on_delta {
                            f(Delta::Reasoning(text.to_string()));
                        }
                    }
                    "signature_delta" => {
                        let sig = delta["signature"].as_str().unwrap_or_default();
                        append(&mut block.value, "signature", sig);
                    }
                    "input_json_delta" => {
                        block
                            .partial_json
                            .push_str(delta["partial_json"].as_str().unwrap_or_default());
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = event["index"].as_u64().unwrap_or(0) as usize;
                if let Some(Some(block)) = self.blocks.get_mut(index) {
                    if block.value["type"] == "tool_use" && !block.partial_json.is_empty() {
                        // Unparseable input (cut off mid-call) stays as the
                        // raw text on the call, for the parser to report.
                        if let Ok(input) = serde_json::from_str::<Value>(&block.partial_json) {
                            block.value["input"] = input;
                        }
                    }
                }
            }
            "message_delta" => {
                if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                    self.stop_reason = reason.to_string();
                }
                if let Some(category) = event["delta"]["stop_details"]["category"].as_str() {
                    self.refusal_category = Some(category.to_string());
                }
                self.usage(&event["usage"]);
            }
            "message_stop" => return Ok(true),
            "error" => {
                let error = &event["error"];
                let status = match error["type"].as_str().unwrap_or_default() {
                    "overloaded_error" => 529,
                    "rate_limit_error" => 429,
                    "invalid_request_error" => 400,
                    _ => 500,
                };
                return Err(ProviderError::Http {
                    status,
                    body: error["message"]
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| error.to_string()),
                });
            }
            _ => {} // ping, and anything newer
        }
        Ok(false)
    }

    fn usage(&mut self, usage: &Value) {
        if let Some(n) = usage["input_tokens"].as_i64() {
            self.input_tokens = n;
        }
        if let Some(n) = usage["cache_creation_input_tokens"].as_i64() {
            self.cache_creation = n;
        }
        if let Some(n) = usage["cache_read_input_tokens"].as_i64() {
            self.cache_read = n;
        }
        if let Some(n) = usage["output_tokens"].as_i64() {
            self.out.completion_tokens = n;
        }
    }

    fn finish(mut self) -> Completion {
        // The whole prompt, cached or not: the context budget counts all of it.
        self.out.prompt_tokens = self.input_tokens + self.cache_creation + self.cache_read;
        self.out.cached_tokens = self.cache_read;

        let mut blocks: Vec<Block> = self.blocks.into_iter().flatten().collect();
        // After a mid-reply fallback, only the model that finished made the
        // tool calls that count.
        let boundary = blocks.iter().rposition(|b| b.value["type"] == "fallback");
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls = Vec::new();
        for (i, block) in blocks.iter_mut().enumerate() {
            match block.value["type"].as_str().unwrap_or_default() {
                "text" => content.push_str(block.value["text"].as_str().unwrap_or_default()),
                "thinking" => {
                    reasoning.push_str(block.value["thinking"].as_str().unwrap_or_default())
                }
                "tool_use" if boundary.is_none_or(|b| i > b) => {
                    let arguments = if block.value["input"].is_object()
                        && (block.partial_json.is_empty()
                            || serde_json::from_str::<Value>(&block.partial_json).is_ok())
                    {
                        block.value["input"].to_string()
                    } else {
                        block.partial_json.clone()
                    };
                    calls.push(ToolCall::new(
                        block.value["id"].as_str().unwrap_or_default(),
                        block.value["name"].as_str().unwrap_or_default(),
                        arguments,
                    ));
                }
                _ => {}
            }
        }

        if self.stop_reason == "refusal" {
            // Declined, by a safety classifier or the model itself, and no
            // fallback took over. The partial output is not an answer.
            let why = self
                .refusal_category
                .map(|c| format!(" (category: {c})"))
                .unwrap_or_default();
            self.out.content = format!(
                "Claude declined to continue{why}. Rephrase the request, or choose another \
                 model in Agent > Backend Settings."
            );
            self.out.finish_reason = "stop".into();
            return self.out;
        }

        self.out.content = content;
        self.out.reasoning = reasoning;
        self.out.tool_calls = calls;
        self.out.finish_reason = match self.stop_reason.as_str() {
            "tool_use" => "tool_calls",
            "max_tokens" | "model_context_window_exceeded" => "length",
            _ => "stop",
        }
        .to_string();
        self.out.provider_blocks = blocks.into_iter().map(|b| b.value).collect();
        self.out
    }
}

fn append(block: &mut Value, field: &str, text: &str) {
    let current = block[field].as_str().unwrap_or_default().to_string();
    block[field] = Value::String(current + text);
}

/// Anthropic's list prices, dollars per million tokens: input, output, cache
/// read. A snapshot (25 September 2026); the console is the authority.
pub fn pricing_for(model: &str) -> Option<(f64, f64, f64)> {
    let table: [(&str, f64, f64, f64); 12] = [
        ("claude-fable-5-1", 10.0, 50.0, 0.25),
        ("claude-mythos-5-1", 10.0, 50.0, 1.0),
        ("claude-fable-5", 10.0, 50.0, 1.0),
        ("claude-opus-5-5", 4.0, 20.0, 0.20),
        ("claude-opus-5", 5.0, 25.0, 0.50),
        ("claude-opus-4-8", 5.0, 25.0, 0.50),
        ("claude-opus-4-7", 5.0, 25.0, 0.50),
        ("claude-opus-4-6", 5.0, 25.0, 0.50),
        ("claude-sonnet-5-5", 2.0, 10.0, 0.20),
        ("claude-sonnet-5", 2.0, 10.0, 0.20),
        ("claude-sonnet-4-6", 3.0, 15.0, 0.30),
        ("claude-haiku-4-5", 1.0, 5.0, 0.10),
    ];
    table
        .iter()
        .find(|(id, ..)| *id == model)
        .map(|(_, i, o, c)| (*i, *o, *c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::Sampling;
    use smithy_tools::ToolResult;

    fn call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall::new(id, name, args)
    }

    /// Tool calls become `tool_use`, results become `tool_result` in one user
    /// message, a note after them joins that message, and `system` moves out.
    #[test]
    fn the_history_translates_to_claudes_shape() {
        let mut h = History::new("be careful");
        h.push(Message::user("fix it"));
        let calls = vec![
            call("t1", "read", r#"{"path":"a.rs"}"#),
            call("t2", "read", r#"{"path":"b.rs"}"#),
        ];
        h.push(Message::assistant_with_calls("Looking.", calls.clone()));
        h.push(Message::tool_result(&ToolResult::ok(
            &calls[0],
            "fn a() {}",
        )));
        h.push(Message::tool_result(&ToolResult::ok(&calls[1], "")));
        h.push(Message::user("[supervisor] keep going"));

        let (system, messages) = to_messages(&h);
        assert_eq!(system.as_deref(), Some("be careful"));
        assert_eq!(messages.len(), 3, "user, assistant, user: {messages:#?}");
        let assistant = &messages[1]["content"];
        assert_eq!(assistant[0], json!({"type": "text", "text": "Looking."}));
        assert_eq!(assistant[1]["type"], "tool_use");
        assert_eq!(assistant[1]["input"], json!({"path": "a.rs"}));
        let results = messages[2]["content"].as_array().unwrap();
        assert_eq!(results[0]["type"], "tool_result");
        assert_eq!(results[0]["tool_use_id"], "t1");
        assert_eq!(
            results[1]["content"], "(no output)",
            "an empty result is not refused"
        );
        assert_eq!(results[2]["type"], "text", "the note follows the results");
        // The same history, the same bytes: the cache and the thinking
        // bindings depend on it.
        assert_eq!(to_messages(&h).1, messages);
    }

    /// A turn that arrived as Claude's blocks goes back as those blocks,
    /// thinking and signature untouched; a call the session never answered is
    /// left out, and so is everything the declined model did before a
    /// mid-reply fallback (except its text).
    #[test]
    fn stored_blocks_go_back_verbatim_with_only_the_required_removals() {
        let thinking = json!({"type": "thinking", "thinking": "plan", "signature": "sig=="});
        let answered =
            json!({"type": "tool_use", "id": "t1", "name": "read", "input": {"path": "a"}});
        let unanswered = json!({"type": "tool_use", "id": "t9", "name": "read", "input": {}});
        let message =
            Message::assistant_with_calls("", vec![call("t1", "read", r#"{"path":"a"}"#)])
                .with_provider_blocks(vec![thinking.clone(), answered.clone(), unanswered]);
        assert_eq!(
            assistant_blocks(&message),
            vec![thinking.clone(), answered.clone()]
        );

        let declined_text = json!({"type": "text", "text": "Partial "});
        let marker = json!({"type": "fallback", "from": {"model": "a"}, "to": {"model": "b"}});
        let after = json!({"type": "thinking", "thinking": "again", "signature": "s2"});
        let message =
            Message::assistant_with_calls("", vec![call("t1", "read", r#"{"path":"a"}"#)])
                .with_provider_blocks(vec![
                    thinking,
                    declined_text.clone(),
                    marker,
                    after.clone(),
                    answered.clone(),
                ]);
        assert_eq!(
            assistant_blocks(&message),
            vec![declined_text, after, answered]
        );
    }

    /// Server-sent events split every seven bytes, mid-event and mid-character,
    /// as a network would.
    fn sse(events: &[Value]) -> Vec<Result<Vec<u8>, String>> {
        let text: String = events
            .iter()
            .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e))
            .collect();
        text.into_bytes()
            .chunks(7)
            .map(|c| Ok(c.to_vec()))
            .collect()
    }

    #[tokio::test]
    async fn a_streamed_reply_with_thinking_text_and_a_tool_call_is_assembled() {
        let events = [
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 10, "cache_creation_input_tokens": 100, "cache_read_input_tokens": 1000, "output_tokens": 1}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "I should read ä file"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "abc=="}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "ping"}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Reading — "}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "read", "input": {}}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":"}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "input_json_delta", "partial_json": "\"src/ü.rs\"}"}}),
            json!({"type": "content_block_stop", "index": 2}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 42}}),
            json!({"type": "message_stop"}),
        ];
        let c = consume_stream(futures_util::stream::iter(sse(&events)), None, STREAM_IDLE)
            .await
            .unwrap();
        assert_eq!(c.content, "Reading — ");
        assert_eq!(c.reasoning, "I should read ä file");
        assert_eq!(c.finish_reason, "tool_calls");
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].id, "toolu_1");
        assert_eq!(
            c.tool_calls[0].parsed_arguments().unwrap()["path"],
            "src/ü.rs"
        );
        assert_eq!(
            c.prompt_tokens, 1110,
            "cached and uncached prompt tokens together"
        );
        assert_eq!(c.cached_tokens, 1000);
        assert_eq!(c.completion_tokens, 42);
        assert_eq!(c.provider_blocks.len(), 3);
        assert_eq!(c.provider_blocks[0]["signature"], "abc==");
        assert_eq!(c.provider_blocks[2]["input"], json!({"path": "src/ü.rs"}));
    }

    /// A refusal is a sentence, not an empty answer or the declined partial;
    /// an error inside the stream is an error.
    #[tokio::test]
    async fn refusals_and_stream_errors_are_reported() {
        let refused = [
            json!({"type": "message_start", "message": {"usage": {"input_tokens": 5}}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Sure, here is"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "refusal", "stop_details": {"type": "refusal", "category": "cyber"}}, "usage": {"output_tokens": 3}}),
            json!({"type": "message_stop"}),
        ];
        let c = consume_stream(futures_util::stream::iter(sse(&refused)), None, STREAM_IDLE)
            .await
            .unwrap();
        assert!(
            c.content.contains("declined") && c.content.contains("cyber"),
            "{}",
            c.content
        );
        assert!(c.provider_blocks.is_empty() && c.tool_calls.is_empty());

        let overloaded = [
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        ];
        let err = consume_stream(
            futures_util::stream::iter(sse(&overloaded)),
            None,
            STREAM_IDLE,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, ProviderError::Http { status: 529, .. }),
            "{err}"
        );
    }

    fn body_for(model: &str, thinking: bool) -> Value {
        let provider = Anthropic::new(DEFAULT_URL, model, "k", "xhigh").unwrap();
        let mut history = History::new("sys");
        history.push(Message::user("hi"));
        let tools = json!([{"type": "function", "function": {"name": "read", "description": "Read a file", "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}}]);
        let sampling = Sampling {
            thinking,
            ..Sampling::default()
        };
        provider.build_body(&CompletionRequest {
            history: &history,
            tools: &tools,
            sampling: &sampling,
            timeout: None,
        })
    }

    /// The current models think adaptively at the chosen effort, keep their
    /// thinking valid with `drop_block`, cache, and opt into fallbacks where
    /// documented; Haiku gets none of what it would refuse. No sampling
    /// parameters for anyone.
    #[test]
    fn each_model_family_gets_only_what_it_accepts() {
        let opus = body_for("claude-opus-5-5", true);
        assert_eq!(opus["thinking"]["type"], "adaptive");
        assert_eq!(
            opus["thinking"]["block_binding"]["prefix_mismatch_behavior"],
            "drop_block"
        );
        assert_eq!(opus["output_config"]["effort"], "xhigh");
        assert_eq!(opus["fallbacks"], "default");
        assert_eq!(opus["cache_control"]["type"], "ephemeral");
        assert_eq!(opus["system"][0]["text"], "sys");
        assert_eq!(
            opus["tools"][0]["input_schema"]["properties"]["path"]["type"],
            "string"
        );
        assert!(opus.get("temperature").is_none() && opus.get("top_p").is_none());

        assert_eq!(
            body_for("claude-opus-5-5", false)["output_config"]["effort"],
            "low"
        );

        let haiku = body_for("claude-haiku-4-5", true);
        assert!(haiku.get("thinking").is_none() && haiku.get("output_config").is_none());
        assert!(haiku.get("fallbacks").is_none());

        let sonnet46 = body_for("claude-sonnet-4-6", true);
        assert_eq!(sonnet46["thinking"]["type"], "adaptive");
        assert!(
            sonnet46.get("fallbacks").is_none(),
            "fallbacks only where documented"
        );
    }

    #[test]
    fn prices_are_known_for_the_current_models() {
        assert_eq!(pricing_for("claude-opus-5-5"), Some((4.0, 20.0, 0.20)));
        assert_eq!(pricing_for("claude-sonnet-5-5"), Some((2.0, 10.0, 0.20)));
        assert_eq!(pricing_for("gpt-anything"), None);
    }
}

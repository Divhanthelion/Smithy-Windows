//! Any OpenAI-compatible Chat Completions API: OpenAI, Groq, Mistral, xAI,
//! Together, Gemini's compatibility endpoint, and whatever comes next.
//!
//! The wire format is the one OpenRouter and DeepSeek already use, so this is
//! their client without anything provider-specific: an address, a model, and
//! an optional Bearer key, all chosen by the user.
//!
//! ## Adapting to what a server refuses
//!
//! "Compatible" covers servers that disagree about parameters. OpenAI's newer
//! models refuse `max_tokens` (they want `max_completion_tokens`) and refuse a
//! `temperature` other than the default; some servers reject
//! `stream_options`; OpenAI's reasoning models call tools on this API only
//! with `reasoning_effort: "none"`. None of that can be known before asking,
//! and the refusal names the parameter, so the refusal is the signal: a 400
//! that names something this request sent is retried once without it (or, for
//! `reasoning_effort`, with it set to `none`), and the adjustment sticks for
//! the rest of this provider's life — the same approach as `min_p` in
//! [`crate::providers::lmstudio`].
//!
//! Whether to retry is decided by what *this* request sent, never by a shared
//! flag alone: several Sessions share one provider, and a request refused at
//! the same moment as another must still get its retry.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::provider::{Completion, CompletionRequest, Delta, Provider, ProviderError, Sampling};
use crate::providers::lmstudio::ModelInfo;
use crate::providers::sse::consume_sse_stream;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(900);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// One way of reshaping a request that a server refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adjustment {
    /// Send `max_completion_tokens` instead of `max_tokens`.
    MaxCompletionTokens,
    /// Send no output limit at all, when the one sent was refused as too large
    /// (or under either name): the server's own default applies.
    OmitMaxTokens,
    /// Leave `temperature` out, so the model's default applies.
    OmitTemperature,
    /// Leave `top_p` out.
    OmitTopP,
    /// Leave `stream_options` out (usage then goes unreported).
    OmitStreamOptions,
    /// Send `reasoning_effort: "none"`, which OpenAI's reasoning models need
    /// before they will call tools on Chat Completions.
    ReasoningEffortNone,
}

/// The adjustments in force, as one value: what a request was sent with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Adjustments {
    pub max_completion_tokens: bool,
    pub omit_max_tokens: bool,
    pub omit_temperature: bool,
    pub omit_top_p: bool,
    pub omit_stream_options: bool,
    pub reasoning_effort_none: bool,
}

impl Adjustments {
    fn has(&self, a: Adjustment) -> bool {
        match a {
            Adjustment::MaxCompletionTokens => self.max_completion_tokens,
            Adjustment::OmitMaxTokens => self.omit_max_tokens,
            Adjustment::OmitTemperature => self.omit_temperature,
            Adjustment::OmitTopP => self.omit_top_p,
            Adjustment::OmitStreamOptions => self.omit_stream_options,
            Adjustment::ReasoningEffortNone => self.reasoning_effort_none,
        }
    }
}

/// What to change, given a 400's body and what the refused request was sent
/// with. `None` when the refusal names nothing this module knows how to fix,
/// or only things already adjusted (no loop).
pub fn adjustment_for(error_body: &str, sent: &Adjustments) -> Option<Adjustment> {
    let body = error_body.to_lowercase();
    // `max_tokens` named on its own, not as part of `max_completion_tokens`.
    let names_max_tokens = body
        .replace("max_completion_tokens", "")
        .contains("max_tokens");
    let candidates = [
        // "Use 'max_completion_tokens' instead": the name is wrong.
        (
            names_max_tokens && body.contains("max_completion_tokens"),
            Adjustment::MaxCompletionTokens,
        ),
        // Any other complaint about the limit (too large, say): drop it.
        (
            names_max_tokens || body.contains("max_completion_tokens"),
            Adjustment::OmitMaxTokens,
        ),
        (
            body.contains("reasoning_effort"),
            Adjustment::ReasoningEffortNone,
        ),
        (body.contains("temperature"), Adjustment::OmitTemperature),
        (body.contains("top_p"), Adjustment::OmitTopP),
        (
            body.contains("stream_options"),
            Adjustment::OmitStreamOptions,
        ),
    ];
    candidates
        .into_iter()
        .find(|(named, a)| *named && !sent.has(*a))
        .map(|(_, a)| a)
}

pub struct Compatible {
    http: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
    max_completion_tokens: AtomicBool,
    omit_max_tokens: AtomicBool,
    omit_temperature: AtomicBool,
    omit_top_p: AtomicBool,
    omit_stream_options: AtomicBool,
    reasoning_effort_none: AtomicBool,
}

impl Compatible {
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Result<Self, ProviderError> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| ProviderError::Other(format!("could not build HTTP client: {e}")))?;
        Ok(Compatible {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            api_key: api_key.into(),
            max_completion_tokens: AtomicBool::new(false),
            omit_max_tokens: AtomicBool::new(false),
            omit_temperature: AtomicBool::new(false),
            omit_top_p: AtomicBool::new(false),
            omit_stream_options: AtomicBool::new(false),
            reasoning_effort_none: AtomicBool::new(false),
        })
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn models_url(&self) -> String {
        format!("{}/models", self.base_url)
    }

    fn adjustments(&self) -> Adjustments {
        Adjustments {
            max_completion_tokens: self.max_completion_tokens.load(Ordering::Relaxed),
            omit_max_tokens: self.omit_max_tokens.load(Ordering::Relaxed),
            omit_temperature: self.omit_temperature.load(Ordering::Relaxed),
            omit_top_p: self.omit_top_p.load(Ordering::Relaxed),
            omit_stream_options: self.omit_stream_options.load(Ordering::Relaxed),
            reasoning_effort_none: self.reasoning_effort_none.load(Ordering::Relaxed),
        }
    }

    fn adopt(&self, a: Adjustment) {
        let flag = match a {
            Adjustment::MaxCompletionTokens => &self.max_completion_tokens,
            Adjustment::OmitMaxTokens => &self.omit_max_tokens,
            Adjustment::OmitTemperature => &self.omit_temperature,
            Adjustment::OmitTopP => &self.omit_top_p,
            Adjustment::OmitStreamOptions => &self.omit_stream_options,
            Adjustment::ReasoningEffortNone => &self.reasoning_effort_none,
        };
        flag.store(true, Ordering::Relaxed);
    }

    fn body_with(&self, request: &CompletionRequest<'_>, a: &Adjustments) -> Value {
        let s: &Sampling = request.sampling;
        let mut body = json!({
            "model": self.model,
            "messages": request.history.to_api_with_reasoning(false),
            "tools": request.tools,
            "stream": true,
        });
        if !a.omit_temperature {
            body["temperature"] = json!(s.temperature);
        }
        if !a.omit_top_p && !a.omit_temperature {
            // A server that only accepts the default temperature usually says
            // the same of top_p; leaving both out together saves a refusal.
            body["top_p"] = json!(s.top_p);
        }
        if a.omit_max_tokens {
            // The server's default output limit.
        } else if a.max_completion_tokens {
            body["max_completion_tokens"] = json!(s.max_tokens);
        } else {
            body["max_tokens"] = json!(s.max_tokens);
        }
        if !a.omit_stream_options {
            body["stream_options"] = json!({ "include_usage": true });
        }
        if a.reasoning_effort_none {
            body["reasoning_effort"] = json!("none");
        }
        body
    }

    fn authorized(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.api_key.trim().is_empty() {
            builder
        } else {
            builder.bearer_auth(&self.api_key)
        }
    }

    async fn send(
        &self,
        request: &CompletionRequest<'_>,
        adjustments: &Adjustments,
        on_delta: Option<&(dyn Fn(Delta) + Send + Sync)>,
    ) -> Result<Completion, ProviderError> {
        let response = self
            .authorized(self.http.post(self.chat_url()))
            .json(&self.body_with(request, adjustments))
            .timeout(request.http_timeout(REQUEST_TIMEOUT))
            .send()
            .await
            .map_err(|e| ProviderError::Unreachable {
                endpoint: self.chat_url(),
                source: Box::new(e),
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body: truncate(&body, 500),
            });
        }
        consume_sse_stream(response.bytes_stream(), on_delta).await
    }
}

#[async_trait]
impl Provider for Compatible {
    fn name(&self) -> &str {
        "compatible"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn stub_superseded_snapshots(&self) -> bool {
        true
    }

    async fn preflight(&self) -> Result<(), ProviderError> {
        if self.model.trim().is_empty() {
            return Err(ProviderError::Other(
                "No model is chosen for the OpenAI-compatible backend: pick one in Agent > \
                 Backend Settings."
                    .to_string(),
            ));
        }
        // `/models` is the one read every compatible API offers. A refused
        // key shows up here, before the first turn, rather than as a failed
        // one; a server without `/models` is not an error.
        let response = self
            .authorized(self.http.get(self.models_url()))
            .send()
            .await
            .map_err(|e| ProviderError::Unreachable {
                endpoint: self.models_url(),
                source: Box::new(e),
            })?;
        let status = response.status().as_u16();
        if status == 401 || status == 403 {
            let body = response.text().await.unwrap_or_default();
            return Err(ProviderError::Http {
                status,
                body: format!(
                    "{} rejected the API key{}: {}",
                    self.base_url,
                    if self.api_key.trim().is_empty() {
                        " (none is saved for this address)"
                    } else {
                        ""
                    },
                    truncate(&body, 300)
                ),
            });
        }
        Ok(())
    }

    /// The context window, when the server's `/models` says. Field names vary:
    /// `context_length` (OpenRouter-style), `context_window` (Groq),
    /// `max_context_length` (Mistral), `max_model_len` (vLLM). OpenAI reports
    /// none, and the session then uses its conservative defaults.
    async fn probe_model(&self) -> Result<Option<ModelInfo>, ProviderError> {
        let Ok(response) = self
            .authorized(self.http.get(self.models_url()))
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
        let context = body["data"].as_array().and_then(|models| {
            models
                .iter()
                .find(|m| m["id"].as_str() == Some(self.model.as_str()))
                .and_then(context_of)
        });
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
        let mut sent = self.adjustments();
        // Each refusal can name one more parameter; six is every adjustment
        // there is, so this cannot loop.
        for _ in 0..6 {
            match self.send(&request, &sent, on_delta).await {
                Err(ProviderError::Http { status: 400, body }) => {
                    let Some(fix) = adjustment_for(&body, &sent) else {
                        return Err(ProviderError::Http { status: 400, body });
                    };
                    self.adopt(fix);
                    sent = self.adjustments();
                    // Whatever another Session adopted meanwhile is included,
                    // and this request's own fix is guaranteed to be.
                    match fix {
                        Adjustment::MaxCompletionTokens => sent.max_completion_tokens = true,
                        Adjustment::OmitMaxTokens => sent.omit_max_tokens = true,
                        Adjustment::OmitTemperature => sent.omit_temperature = true,
                        Adjustment::OmitTopP => sent.omit_top_p = true,
                        Adjustment::OmitStreamOptions => sent.omit_stream_options = true,
                        Adjustment::ReasoningEffortNone => sent.reasoning_effort_none = true,
                    }
                }
                other => return other,
            }
        }
        self.send(&request, &sent, on_delta).await
    }

    fn build_body(&self, request: &CompletionRequest<'_>) -> Value {
        self.body_with(request, &self.adjustments())
    }
}

/// A model's context window from a `/models` entry, under any of the names
/// compatible servers use for it.
pub fn context_of(model: &Value) -> Option<i64> {
    [
        "context_length",
        "context_window",
        "max_context_length",
        "max_model_len",
    ]
    .iter()
    .find_map(|field| model[*field].as_i64())
}

fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        let mut end = max_bytes;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}... (truncated)", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_naming_max_tokens_switches_to_max_completion_tokens() {
        let body = r#"{"error":{"message":"Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.","param":"max_tokens"}}"#;
        assert_eq!(
            adjustment_for(body, &Adjustments::default()),
            Some(Adjustment::MaxCompletionTokens)
        );
    }

    #[test]
    fn a_refusal_naming_temperature_leaves_it_out() {
        let body = "Unsupported value: 'temperature' does not support 0.6 with this model. Only the default (1) value is supported.";
        assert_eq!(
            adjustment_for(body, &Adjustments::default()),
            Some(Adjustment::OmitTemperature)
        );
    }

    #[test]
    fn reasoning_effort_is_set_to_none_when_tools_need_it() {
        let body = "Function tools are only supported with reasoning_effort set to 'none'.";
        assert_eq!(
            adjustment_for(body, &Adjustments::default()),
            Some(Adjustment::ReasoningEffortNone)
        );
    }

    /// A limit refused as too large is dropped rather than renamed.
    #[test]
    fn a_limit_refused_as_too_large_is_left_out() {
        let body = "max_tokens must be less than or equal to 8192";
        assert_eq!(
            adjustment_for(body, &Adjustments::default()),
            Some(Adjustment::OmitMaxTokens)
        );
        let after_rename = Adjustments {
            max_completion_tokens: true,
            ..Adjustments::default()
        };
        assert_eq!(
            adjustment_for("max_completion_tokens is too large: 16384", &after_rename),
            Some(Adjustment::OmitMaxTokens)
        );
    }

    /// A refusal naming only what was already adjusted is returned as an
    /// error, not retried forever.
    #[test]
    fn an_adjustment_already_sent_is_not_retried() {
        let sent = Adjustments {
            max_completion_tokens: true,
            omit_max_tokens: true,
            ..Adjustments::default()
        };
        assert_eq!(
            adjustment_for("max_completion_tokens is too large", &sent),
            None
        );
        assert_eq!(
            adjustment_for("model not found", &Adjustments::default()),
            None
        );
    }

    /// A server that behaves like OpenAI's newer models on Chat Completions:
    /// 401 without the right key; 400 naming `max_tokens` (use
    /// `max_completion_tokens`); 400 naming `temperature` (only the default);
    /// otherwise a one-word streamed answer. Each refusal waits a moment, so
    /// concurrent requests are all in flight before the first one lands.
    async fn openai_like_server() -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    loop {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&buf);
                        if let Some(end) = text.find("\r\n\r\n") {
                            let len = text[..end]
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .and_then(|v| v.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if buf.len() >= end + 4 + len {
                                break;
                            }
                        }
                    }
                    let request = String::from_utf8_lossy(&buf).to_string();
                    let refuse = |status: &str, message: &str| {
                        let body = format!(r#"{{"error":{{"message":"{message}"}}}}"#);
                        format!(
                            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    };
                    let response = if !request
                        .to_ascii_lowercase()
                        .contains("authorization: bearer sk-test")
                    {
                        refuse("401 Unauthorized", "Incorrect API key provided.")
                    } else if request.contains("\"max_tokens\"") {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        refuse("400 Bad Request", "Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.")
                    } else if request.contains("\"temperature\"") {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        refuse("400 Bad Request", "Unsupported value: 'temperature' does not support 0.6 with this model. Only the default (1) value is supported.")
                    } else {
                        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    };
                    let _ = sock.write_all(response.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        format!("http://{addr}/v1")
    }

    /// Three Sessions share one provider and are refused together, twice
    /// over: every request must adapt and get its answer, and the adjustments
    /// stick for the next one.
    #[tokio::test]
    async fn concurrent_requests_adapt_to_what_the_server_refuses() {
        let provider = Compatible::new(openai_like_server().await, "m", "sk-test").unwrap();
        let history = {
            let mut h = crate::message::History::new("sys");
            h.push(crate::message::Message::user("hi"));
            h
        };
        let tools = Value::Array(Vec::new());
        let sampling = Sampling::default();
        let ask = || {
            provider.complete(
                CompletionRequest {
                    history: &history,
                    tools: &tools,
                    sampling: &sampling,
                    timeout: None,
                },
                None,
            )
        };
        let (a, b, c) = tokio::join!(ask(), ask(), ask());
        for (name, r) in [("a", a), ("b", b), ("c", c)] {
            let r = r.unwrap_or_else(|e| panic!("request {name} failed: {e}"));
            assert_eq!(r.content, "ok", "request {name}");
        }
        let after = provider.adjustments();
        assert!(after.max_completion_tokens && after.omit_temperature);
        assert_eq!(
            ask().await.unwrap().content,
            "ok",
            "no refusal to pay again"
        );
    }

    /// The key is sent as a Bearer token; a wrong one is an error the user
    /// sees, not something to retry.
    #[tokio::test]
    async fn a_refused_key_is_reported() {
        let provider = Compatible::new(openai_like_server().await, "m", "sk-wrong").unwrap();
        let history = crate::message::History::new("sys");
        let tools = Value::Array(Vec::new());
        let sampling = Sampling::default();
        let err = provider
            .complete(
                CompletionRequest {
                    history: &history,
                    tools: &tools,
                    sampling: &sampling,
                    timeout: None,
                },
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, ProviderError::Http { status: 401, .. }),
            "{err}"
        );
    }

    #[test]
    fn the_context_window_is_read_under_any_of_its_names() {
        assert_eq!(context_of(&json!({"context_window": 131072})), Some(131072));
        assert_eq!(
            context_of(&json!({"max_context_length": 32768})),
            Some(32768)
        );
        assert_eq!(context_of(&json!({"id": "gpt"})), None);
    }
}

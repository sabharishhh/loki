//! Anthropic behind [`ModelProvider`].
//!
//! Raw HTTP against `POST /v1/messages`. There is no official Rust SDK, so the wire types below
//! mirror the documented request and SSE shapes.

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::core::vocab::{Cents, CostModel, Locality};
use crate::ports::model::{
    Caps, Chunk, ChunkStream, Message, ModelError, ModelProvider, Request, Role, StopReason,
    ToolSupport, Usage,
};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
const DEFAULT_MODEL: &str = "claude-opus-5";
const CONTEXT_WINDOW: usize = 1_000_000;

/// Pricing for `claude-opus-5`, in cents per million tokens.
const INPUT_PER_MTOK: Cents = Cents::new(500);
const OUTPUT_PER_MTOK: Cents = Cents::new(2500);

pub struct Anthropic {
    http: reqwest::Client,
    api_key: String,
    model: String,
}

impl Anthropic {
    /// # Errors
    /// Fails if the HTTP client cannot be built.
    pub fn new(api_key: impl Into<String>) -> Result<Self, ModelError> {
        Ok(Self {
            http: reqwest::Client::builder()
                .build()
                .map_err(|e| ModelError::Transport(e.to_string()))?,
            api_key: api_key.into(),
            model: DEFAULT_MODEL.to_owned(),
        })
    }

    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }
}

impl std::fmt::Debug for Anthropic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Anthropic")
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ModelProvider for Anthropic {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn caps(&self) -> Caps {
        Caps {
            locality: Locality::Cloud,
            prompt_cache: true,
            max_context: CONTEXT_WINDOW,
            tools: ToolSupport::Native,
            cost: CostModel::PerToken {
                input_per_mtok: INPUT_PER_MTOK,
                output_per_mtok: OUTPUT_PER_MTOK,
            },
        }
    }

    async fn complete(
        &self,
        req: Request,
        cancel: CancellationToken,
    ) -> Result<ChunkStream, ModelError> {
        let body = WireRequest::from_request(&self.model, &req);

        let response = tokio::select! {
            () = cancel.cancelled() => return Err(ModelError::Cancelled),
            result = self
                .http
                .post(API_URL)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .json(&body)
                .send() => result.map_err(|e| ModelError::Transport(e.to_string()))?,
        };

        let response = check_status(response).await?;
        Ok(Box::pin(decode(response, cancel)))
    }
}

async fn check_status(response: reqwest::Response) -> Result<reqwest::Response, ModelError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .map(std::time::Duration::from_secs);
    let body = response.text().await.unwrap_or_default();

    Err(match status.as_u16() {
        400 => ModelError::BadRequest(body),
        401 | 403 => ModelError::Unauthorized,
        429 => ModelError::RateLimited(retry_after),
        status => ModelError::Upstream { status, body },
    })
}

/// Turns the SSE body into chunks, ending as soon as the token is cancelled.
fn decode(
    response: reqwest::Response,
    cancel: CancellationToken,
) -> impl futures_core::Stream<Item = Result<Chunk, ModelError>> + Send + 'static {
    async_stream::stream! {
        let mut events = response.bytes_stream().eventsource();

        loop {
            let event = tokio::select! {
                () = cancel.cancelled() => {
                    yield Ok(Chunk::Done(StopReason::Cancelled));
                    return;
                }
                event = events.next() => event,
            };

            let Some(event) = event else { return };
            let event = match event {
                Ok(event) => event,
                Err(e) => {
                    yield Err(ModelError::Transport(e.to_string()));
                    return;
                }
            };

            match serde_json::from_str::<WireEvent>(&event.data) {
                Ok(wire) => {
                    for chunk in wire.into_chunks() {
                        let done = matches!(chunk, Chunk::Done(_));
                        yield Ok(chunk);
                        if done {
                            return;
                        }
                    }
                }
                Err(e) => {
                    yield Err(ModelError::Protocol(e.to_string()));
                    return;
                }
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    system: Vec<WireSystem<'a>>,
    messages: Vec<WireMessage<'a>>,
}

impl<'a> WireRequest<'a> {
    fn from_request(model: &'a str, req: &'a Request) -> Self {
        Self {
            model,
            max_tokens: req.max_tokens,
            stream: true,
            system: req.system.iter().map(WireSystem::from).collect(),
            messages: req.messages.iter().map(WireMessage::from).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct WireSystem<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<CacheControl>,
}

impl<'a> From<&'a crate::ports::model::SystemBlock> for WireSystem<'a> {
    fn from(block: &'a crate::ports::model::SystemBlock) -> Self {
        Self {
            kind: "text",
            text: &block.text,
            cache_control: block.cache.then_some(CacheControl { kind: "ephemeral" }),
        }
    }
}

#[derive(Debug, Serialize)]
struct CacheControl {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Debug, Serialize)]
struct WireMessage<'a> {
    role: &'static str,
    content: &'a str,
}

impl<'a> From<&'a Message> for WireMessage<'a> {
    fn from(message: &'a Message) -> Self {
        Self {
            role: match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            },
            content: &message.content,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireEvent {
    MessageStart {
        message: WireMessageStart,
    },
    ContentBlockDelta {
        delta: WireDelta,
    },
    MessageDelta {
        delta: WireStop,
        usage: WireUsage,
    },
    MessageStop,
    #[serde(other)]
    Ignored,
}

impl WireEvent {
    fn into_chunks(self) -> Vec<Chunk> {
        match self {
            Self::MessageStart { message } => vec![Chunk::Usage(message.usage.into())],
            Self::ContentBlockDelta { delta } => match delta {
                WireDelta::TextDelta { text } => vec![Chunk::Text(text)],
                WireDelta::ThinkingDelta { thinking } => vec![Chunk::Thinking(thinking)],
                WireDelta::Ignored => vec![],
            },
            Self::MessageDelta { delta, usage } => {
                vec![Chunk::Usage(usage.into()), Chunk::Done(delta.into())]
            }
            Self::MessageStop => vec![Chunk::Done(StopReason::EndTurn)],
            Self::Ignored => vec![],
        }
    }
}

#[derive(Debug, Deserialize)]
struct WireMessageStart {
    #[serde(default)]
    usage: WireUsage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireDelta {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        thinking: String,
    },
    #[serde(other)]
    Ignored,
}

#[derive(Debug, Default, Deserialize)]
struct WireStop {
    stop_reason: Option<String>,
}

impl From<WireStop> for StopReason {
    fn from(stop: WireStop) -> Self {
        match stop.stop_reason.as_deref() {
            Some("max_tokens") => Self::MaxTokens,
            Some("tool_use") => Self::ToolUse,
            Some("refusal") => Self::Refusal,
            _ => Self::EndTurn,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct WireUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: u32,
}

impl From<WireUsage> for Usage {
    fn from(usage: WireUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_input_tokens,
            cache_write_tokens: usage.cache_creation_input_tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::vocab::ModelRole;
    use crate::ports::model::SystemBlock;

    fn request() -> Request {
        Request {
            role: ModelRole::Primary,
            system: vec![
                SystemBlock::new("You are Loki."),
                SystemBlock::cached("Working set."),
            ],
            messages: vec![Message::user("hello")],
            max_tokens: 64_000,
        }
    }

    #[test]
    fn cache_control_marks_only_the_last_stable_block() {
        let req = request();
        let wire = WireRequest::from_request("claude-opus-5", &req);
        let json = serde_json::to_value(&wire).unwrap();

        assert_eq!(json["system"][0].get("cache_control"), None);
        assert_eq!(json["system"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(json["stream"], true);
        assert_eq!(json["messages"][0]["role"], "user");
    }

    #[test]
    fn text_deltas_become_text_chunks() {
        let event: WireEvent = serde_json::from_str(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
        )
        .unwrap();
        assert_eq!(event.into_chunks(), vec![Chunk::Text("Hi".into())]);
    }

    #[test]
    fn message_delta_reports_usage_then_stop() {
        let event: WireEvent = serde_json::from_str(
            r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":12}}"#,
        )
        .unwrap();
        assert_eq!(
            event.into_chunks(),
            vec![
                Chunk::Usage(Usage {
                    output_tokens: 12,
                    ..Usage::default()
                }),
                Chunk::Done(StopReason::MaxTokens),
            ]
        );
    }

    #[test]
    fn message_start_carries_cache_usage() {
        let event: WireEvent = serde_json::from_str(
            r#"{"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":40,"cache_read_input_tokens":900}}}"#,
        )
        .unwrap();
        assert_eq!(
            event.into_chunks(),
            vec![Chunk::Usage(Usage {
                input_tokens: 40,
                cache_read_tokens: 900,
                ..Usage::default()
            })]
        );
    }

    #[test]
    fn unknown_events_are_skipped_not_errors() {
        let event: WireEvent =
            serde_json::from_str(r#"{"type":"content_block_start","index":0}"#).unwrap();
        assert!(event.into_chunks().is_empty());
    }

    #[test]
    fn caps_report_cloud_and_native_tools() {
        let provider = Anthropic::new("test-key").unwrap();
        let caps = provider.caps();
        assert_eq!(caps.locality, Locality::Cloud);
        assert_eq!(caps.tools, ToolSupport::Native);
        assert!(caps.prompt_cache);
        assert_eq!(provider.id(), "anthropic");
    }
}

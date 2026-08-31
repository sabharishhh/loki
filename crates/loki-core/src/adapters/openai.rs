//! OpenAI behind [`ModelProvider`].
//!
//! Raw HTTP against the Chat Completions endpoint. There is no official Rust SDK.
//!
//! Two differences from the Anthropic adapter shape the mapping. OpenAI has no separate system
//! field, so the frozen prefix becomes leading `system` messages. And prompt caching is automatic
//! on a prefix match rather than marked per block, so `SystemBlock::cache` is ignored here. The
//! two-zone ordering still matters, because that is what keeps the prefix byte-identical.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::pricing;
use super::sse::{self, EventParser};
use crate::core::vocab::{Cents, CostModel, Locality};
use crate::ports::model::{
    Caps, Chunk, ChunkStream, ModelError, ModelProvider, Request, Role, StopReason, ToolSupport,
    Usage,
};

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "gpt-5.6-terra";
const DEFAULT_CONTEXT_WINDOW: usize = 400_000;

pub struct Openai {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    /// Set explicitly by `with_pricing`. Otherwise looked up from the model name.
    cost: Option<CostModel>,
    max_context: usize,
}

impl Openai {
    /// # Errors
    /// Fails if the HTTP client cannot be built.
    pub fn new(api_key: impl Into<String>) -> Result<Self, ModelError> {
        Ok(Self {
            http: reqwest::Client::builder()
                .build()
                .map_err(|e| ModelError::Transport(e.to_string()))?,
            api_key: api_key.into(),
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: DEFAULT_MODEL.to_owned(),
            cost: None,
            max_context: DEFAULT_CONTEXT_WINDOW,
        })
    }

    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Points at an OpenAI-compatible endpoint instead of OpenAI itself.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Overrides pricing, for a model the table does not know.
    #[must_use]
    pub const fn with_pricing(mut self, input_per_mtok: Cents, output_per_mtok: Cents) -> Self {
        self.cost = Some(CostModel::PerToken {
            input_per_mtok,
            output_per_mtok,
        });
        self
    }

    #[must_use]
    pub const fn with_max_context(mut self, tokens: usize) -> Self {
        self.max_context = tokens;
        self
    }
}

impl std::fmt::Debug for Openai {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Openai")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ModelProvider for Openai {
    fn id(&self) -> &str {
        "openai"
    }

    fn caps(&self) -> Caps {
        Caps {
            locality: Locality::Cloud,
            prompt_cache: true,
            max_context: self.max_context,
            tools: ToolSupport::Native,
            cost: self
                .cost
                .or_else(|| pricing::openai(&self.model))
                .unwrap_or(CostModel::Free),
        }
    }

    async fn complete(
        &self,
        req: Request,
        cancel: CancellationToken,
    ) -> Result<ChunkStream, ModelError> {
        let body = WireRequest::from_request(&self.model, &req);
        let url = format!("{}/chat/completions", self.base_url);

        let response = tokio::select! {
            () = cancel.cancelled() => return Err(ModelError::Cancelled),
            result = self
                .http
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send() => result.map_err(|e| ModelError::Transport(e.to_string()))?,
        };

        let response = sse::check_status(response).await?;
        Ok(Box::pin(sse::decode(response, cancel, Parser)))
    }
}

/// OpenAI's SSE payloads.
struct Parser;

impl EventParser for Parser {
    fn parse(&mut self, data: &str) -> Result<Vec<Chunk>, ModelError> {
        if data.trim() == "[DONE]" {
            return Ok(vec![Chunk::Done(StopReason::EndTurn)]);
        }
        serde_json::from_str::<WireChunk>(data)
            .map(WireChunk::into_chunks)
            .map_err(|e| ModelError::Protocol(e.to_string()))
    }
}

#[derive(Debug, Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    max_completion_tokens: u32,
    stream: bool,
    stream_options: StreamOptions,
    messages: Vec<WireMessage<'a>>,
}

impl<'a> WireRequest<'a> {
    fn from_request(model: &'a str, req: &'a Request) -> Self {
        let system = req.system.iter().map(|block| WireMessage {
            role: "system",
            content: &block.text,
        });
        let turn = req.messages.iter().map(|message| WireMessage {
            role: match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            },
            content: &message.content,
        });

        Self {
            model,
            max_completion_tokens: req.max_tokens,
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
            messages: system.chain(turn).collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Debug, Serialize)]
struct WireMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct WireChunk {
    #[serde(default)]
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

impl WireChunk {
    fn into_chunks(self) -> Vec<Chunk> {
        let mut chunks = Vec::new();

        for choice in self.choices {
            if let Some(text) = choice.delta.content
                && !text.is_empty()
            {
                chunks.push(Chunk::Text(text));
            }
            if let Some(reasoning) = choice.delta.reasoning_content
                && !reasoning.is_empty()
            {
                chunks.push(Chunk::Thinking(reasoning));
            }
            if let Some(reason) = choice.finish_reason {
                chunks.push(Chunk::Done(stop_reason(&reason)));
            }
        }

        if let Some(usage) = self.usage {
            // Usage arrives on a trailing chunk after finish_reason, so it goes first to keep
            // Done last. A Done chunk ends the stream.
            chunks.insert(0, Chunk::Usage(usage.into()));
        }

        chunks
    }
}

fn stop_reason(raw: &str) -> StopReason {
    match raw {
        "length" => StopReason::MaxTokens,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::Refusal,
        _ => StopReason::EndTurn,
    }
}

#[derive(Debug, Deserialize)]
struct WireChoice {
    #[serde(default)]
    delta: WireDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct WireDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    prompt_tokens_details: Option<WirePromptDetails>,
}

#[derive(Debug, Deserialize)]
struct WirePromptDetails {
    #[serde(default)]
    cached_tokens: u32,
}

impl From<WireUsage> for Usage {
    fn from(usage: WireUsage) -> Self {
        let cached = usage.prompt_tokens_details.map_or(0, |d| d.cached_tokens);
        Self {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            cache_read_tokens: cached,
            // OpenAI caches automatically and does not report a write count.
            cache_write_tokens: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::vocab::ModelRole;
    use crate::ports::model::{Message, SystemBlock};

    fn request() -> Request {
        Request {
            role: ModelRole::Primary,
            system: vec![
                SystemBlock::new("You are Loki."),
                SystemBlock::cached("Working set."),
            ],
            messages: vec![Message::user("hello")],
            max_tokens: 4096,
        }
    }

    #[test]
    fn the_frozen_prefix_becomes_leading_system_messages() {
        let req = request();
        let wire = WireRequest::from_request("gpt-5", &req);
        let json = serde_json::to_value(&wire).unwrap();

        assert_eq!(json["messages"][0]["role"], "system");
        assert_eq!(json["messages"][1]["role"], "system");
        assert_eq!(json["messages"][2]["role"], "user");
        assert_eq!(json["messages"][2]["content"], "hello");
        assert_eq!(json["stream_options"]["include_usage"], true);
    }

    #[test]
    fn content_deltas_become_text_chunks() {
        let chunk: WireChunk = serde_json::from_str(
            r#"{"choices":[{"index":0,"delta":{"content":"Hi"},"finish_reason":null}]}"#,
        )
        .unwrap();
        assert_eq!(chunk.into_chunks(), vec![Chunk::Text("Hi".into())]);
    }

    #[test]
    fn empty_deltas_produce_nothing() {
        let chunk: WireChunk =
            serde_json::from_str(r#"{"choices":[{"index":0,"delta":{},"finish_reason":null}]}"#)
                .unwrap();
        assert!(chunk.into_chunks().is_empty());
    }

    #[test]
    fn finish_reason_maps_to_a_stop_reason() {
        let chunk: WireChunk = serde_json::from_str(
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"length"}]}"#,
        )
        .unwrap();
        assert_eq!(
            chunk.into_chunks(),
            vec![Chunk::Done(StopReason::MaxTokens)]
        );
    }

    #[test]
    fn usage_is_emitted_before_done() {
        let chunk: WireChunk = serde_json::from_str(
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":40,"completion_tokens":12,
                         "prompt_tokens_details":{"cached_tokens":32}}}"#,
        )
        .unwrap();
        assert_eq!(
            chunk.into_chunks(),
            vec![
                Chunk::Usage(Usage {
                    input_tokens: 40,
                    output_tokens: 12,
                    cache_read_tokens: 32,
                    cache_write_tokens: 0,
                }),
                Chunk::Done(StopReason::EndTurn),
            ]
        );
    }

    #[test]
    fn the_done_sentinel_ends_the_stream() {
        assert_eq!(
            Parser.parse("[DONE]").unwrap(),
            vec![Chunk::Done(StopReason::EndTurn)]
        );
    }

    #[test]
    fn pricing_follows_the_model() {
        let provider = Openai::new("k").unwrap();
        assert_eq!(
            provider.caps().cost,
            CostModel::PerToken {
                input_per_mtok: Cents::new(400),
                output_per_mtok: Cents::new(500),
            },
            "the default model should be priced"
        );

        let mini = Openai::new("k").unwrap().with_model("gpt-5-mini");
        assert_eq!(
            mini.caps().cost,
            CostModel::PerToken {
                input_per_mtok: Cents::new(45),
                output_per_mtok: Cents::new(360),
            }
        );
    }

    #[test]
    fn an_unpriced_model_reports_free_rather_than_a_guess() {
        let unknown = Openai::new("k").unwrap().with_model("some-local-model");
        assert_eq!(unknown.caps().cost, CostModel::Free);
        assert_eq!(unknown.id(), "openai");
    }

    #[test]
    fn explicit_pricing_wins_over_the_table() {
        let forced = Openai::new("k")
            .unwrap()
            .with_model("gpt-5.6-terra")
            .with_pricing(Cents::new(1), Cents::new(2));
        assert_eq!(
            forced.caps().cost,
            CostModel::PerToken {
                input_per_mtok: Cents::new(1),
                output_per_mtok: Cents::new(2),
            }
        );
    }
}

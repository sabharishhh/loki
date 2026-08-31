//! The model provider port.
//!
//! One adapter per backend. The core knows this interface and nothing about who implements it.

use std::pin::Pin;

use async_trait::async_trait;
use futures_core::Stream;
use tokio_util::sync::CancellationToken;

use crate::core::vocab::{CostModel, Locality, ModelRole};

/// A stream of response pieces. Boxed so the trait stays usable as `dyn ModelProvider`.
pub type ChunkStream = Pin<Box<dyn Stream<Item = Result<Chunk, ModelError>> + Send + 'static>>;

/// A backend that can answer a request.
#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn id(&self) -> &str;

    fn caps(&self) -> Caps;

    /// Streams rather than returning a string, so a local backend does not have to buffer a whole
    /// response before the interface sees anything.
    ///
    /// # Errors
    /// Fails if the request is rejected before any output is produced. Failures partway through
    /// arrive as an error item on the stream.
    async fn complete(
        &self,
        req: Request,
        cancel: CancellationToken,
    ) -> Result<ChunkStream, ModelError>;
}

/// What a backend can do.
///
/// `locality` is a capability rather than a setting because the prompt gate has to check it in
/// code, and a boolean in a config file cannot be checked by a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub locality: Locality,
    /// Cloud prefix caching, or local KV reuse. A backend with neither should not pay the cost of
    /// maintaining a frozen prefix.
    pub prompt_cache: bool,
    pub max_context: usize,
    pub tools: ToolSupport,
    pub cost: CostModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSupport {
    Native,
    PromptEmulated,
    None,
}

/// One call to a model.
///
/// `system` is the frozen prefix and must not change within a session. `messages` is turn content
/// and changes every turn. Reversing that ordering misses the provider cache on every call.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub role: ModelRole,
    pub system: Vec<SystemBlock>,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
}

/// One block of the frozen prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemBlock {
    pub text: String,
    /// Marks the end of the cacheable prefix. Set it on the last stable block only.
    pub cache: bool,
}

impl SystemBlock {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            cache: false,
        }
    }

    #[must_use]
    pub fn cached(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            cache: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
        }
    }

    #[must_use]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// One piece of a streamed response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk {
    Text(String),
    Thinking(String),
    Usage(Usage),
    Done(StopReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    ToolUse,
    Refusal,
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("request rejected: {0}")]
    BadRequest(String),
    #[error("authentication failed")]
    Unauthorized,
    #[error("rate limited, retry after {0:?}")]
    RateLimited(Option<std::time::Duration>),
    #[error("provider returned {status}: {body}")]
    Upstream { status: u16, body: String },
    #[error("transport failed: {0}")]
    Transport(String),
    #[error("could not read the provider's response: {0}")]
    Protocol(String),
    #[error("cancelled")]
    Cancelled,
}

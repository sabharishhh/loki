//! Shared plumbing for providers that stream Server-Sent Events.

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::ports::model::{Chunk, ModelError, StopReason};

/// Maps one SSE `data:` payload to zero or more chunks.
///
/// Returning an empty vector skips the event, which is how an unknown event type stays harmless.
pub trait EventParser: Send + 'static {
    /// # Errors
    /// Fails only when the payload is malformed enough that continuing would be wrong.
    fn parse(&mut self, data: &str) -> Result<Vec<Chunk>, ModelError>;
}

/// Decodes an SSE response into chunks, stopping as soon as the token is cancelled.
pub fn decode<P: EventParser>(
    response: reqwest::Response,
    cancel: CancellationToken,
    mut parser: P,
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

            match parser.parse(&event.data) {
                Ok(chunks) => {
                    for chunk in chunks {
                        let done = matches!(chunk, Chunk::Done(_));
                        yield Ok(chunk);
                        if done {
                            return;
                        }
                    }
                }
                Err(e) => {
                    yield Err(e);
                    return;
                }
            }
        }
    }
}

/// Turns a non-success HTTP response into the right [`ModelError`].
///
/// # Errors
/// Returns an error for every non-success status.
pub async fn check_status(response: reqwest::Response) -> Result<reqwest::Response, ModelError> {
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
        // Keep the provider's own words. It knows why it refused and we do not.
        401 | 403 => ModelError::Unauthorized(body),
        429 => ModelError::RateLimited(retry_after),
        status => ModelError::Upstream { status, body },
    })
}

//! Shared plumbing for providers that stream Server-Sent Events.

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::ports::egress::Landed;
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
    response: Landed,
    cancel: CancellationToken,
    mut parser: P,
) -> impl futures_core::Stream<Item = Result<Chunk, ModelError>> + Send + 'static {
    async_stream::stream! {
        // `Vec<u8>` is `AsRef<[u8]>`, which is all the decoder wants, so the port's byte stream
        // goes straight in and no transport type appears here.
        let mut events = response.body.eventsource();

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

/// Turns a non-success response into the right [`ModelError`], draining the body for its words.
///
/// A status is not an error at the port (§12.4 needs to say a page could not be read rather than
/// return it as empty), so the judgement is here, where a 429 is known to mean something.
///
/// # Errors
/// Returns an error for every non-success status.
pub async fn check_status(response: Landed) -> Result<Landed, ModelError> {
    if (200..300).contains(&response.status) {
        return Ok(response);
    }

    let Landed {
        status,
        retry_after,
        mut body,
    } = response;
    let mut raw = Vec::new();
    while let Some(Ok(chunk)) = body.next().await {
        raw.extend_from_slice(&chunk);
    }
    let body = String::from_utf8_lossy(&raw).into_owned();

    Err(match status {
        400 => ModelError::BadRequest(body),
        // Keep the provider's own words. It knows why it refused and we do not.
        401 | 403 => ModelError::Unauthorized(body),
        429 => ModelError::RateLimited(retry_after),
        status => ModelError::Upstream { status, body },
    })
}

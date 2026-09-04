//! The only transport in the tree (§7.1, §21.7).
//!
//! One `reqwest::Client`, and `tests/rings.rs` fails if anything else builds one. That is what
//! makes §21.7's assertion sayable at all: a request nobody can make outside this file is a
//! request the event stream cannot miss.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use tokio_util::sync::CancellationToken;

use crate::core::event::Event;
use crate::core::sink::EventSink;
use crate::ports::egress::{ByteStream, Egress, EgressError, Landed, Method, Outbound};

/// One HTTP client for the whole process.
pub struct Http {
    client: reqwest::Client,
    events: Arc<dyn EventSink>,
}

impl Http {
    /// # Errors
    /// Fails if the client cannot be built.
    pub fn new(events: Arc<dyn EventSink>) -> Result<Self, EgressError> {
        Ok(Self {
            client: reqwest::Client::builder()
                .build()
                .map_err(|e| EgressError::Transport(e.to_string()))?,
            events,
        })
    }
}

impl std::fmt::Debug for Http {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Http").finish_non_exhaustive()
    }
}

#[async_trait]
impl Egress for Http {
    async fn send(
        &self,
        request: Outbound,
        cancel: CancellationToken,
    ) -> Result<Landed, EgressError> {
        // **Before the send, never after.** An event emitted on the way back describes only the
        // requests that came back, so a call that hangs or is cancelled leaves no trace of having
        // happened. Failure point 88 is that shape.
        let (host, path) = request.destination();
        self.events.emit(&Event::Egress {
            host,
            path,
            bytes: request.body.len(),
        });

        let mut built = match request.method {
            Method::Get => self.client.get(&request.url),
            Method::Post => self.client.post(&request.url),
        };
        for (name, value) in &request.headers {
            built = built.header(name, value);
        }
        if !request.body.is_empty() {
            built = built.body(request.body);
        }

        let response = tokio::select! {
            () = cancel.cancelled() => return Err(EgressError::Cancelled),
            result = built.send() => result.map_err(|e| EgressError::Transport(e.to_string()))?,
        };

        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .map(std::time::Duration::from_secs);
        let status = response.status().as_u16();
        let body: ByteStream = Box::pin(response.bytes_stream().map(|chunk| {
            chunk
                .map(|bytes| bytes.to_vec())
                .map_err(|e| EgressError::Transport(e.to_string()))
        }));

        Ok(Landed {
            status,
            retry_after,
            body,
        })
    }
}

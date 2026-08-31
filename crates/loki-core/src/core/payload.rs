//! Tool call payloads carried by the event stream.
//!
//! Thin wrappers over JSON for now. The `Tool` port will refine these when it lands; the event
//! stream only needs to carry and render them.

use serde::{Deserialize, Serialize};

/// Arguments a model supplied for a tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Args(serde_json::Value);

impl Args {
    #[must_use]
    pub const fn new(value: serde_json::Value) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_value(&self) -> &serde_json::Value {
        &self.0
    }
}

impl Default for Args {
    fn default() -> Self {
        Self(serde_json::Value::Null)
    }
}

/// What a tool returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput(serde_json::Value);

impl ToolOutput {
    #[must_use]
    pub const fn new(value: serde_json::Value) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_value(&self) -> &serde_json::Value {
        &self.0
    }
}

/// A chunk of output streamed while a tool is still running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialOutput(String);

impl PartialOutput {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

//! The tool port.
//!
//! Everything the model can act through, whether native Rust, a WASM component, or an MCP client.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::core::payload::{Args, ToolOutput};
use crate::core::vocab::Tier;

/// Something the model can call.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;

    fn schema(&self) -> Schema;

    /// What this tool needs to reach. The host derives the tier from these.
    fn capabilities(&self) -> &[Capability];

    /// # Errors
    /// See [`ToolError`]. Every variant is actionable by the loop.
    async fn call(
        &self,
        args: Args,
        grant: &Grant,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError>;
}

/// JSON Schema describing a tool's arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schema(serde_json::Value);

impl Schema {
    #[must_use]
    pub const fn new(value: serde_json::Value) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn as_value(&self) -> &serde_json::Value {
        &self.0
    }
}

/// What a tool is allowed to reach.
///
/// Deny by default: a tool with no grant can do nothing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    ReadFile {
        root: String,
    },
    WriteFile {
        root: String,
    },
    /// Delete outside the trash. Trash-first deletes are `WriteFile`, because they are reversible.
    DeleteFile {
        root: String,
    },
    ReadMemory,
    WriteMemory,
    Network {
        hosts: Vec<String>,
    },
    /// Run code inside the sandbox. Contained, because it cannot reach out.
    Sandbox,
    /// Spawn a native subprocess under Seatbelt.
    Process,
    ReadConnector {
        connector: ConnectorId,
    },
    /// Create a draft or an event. Reversible.
    DraftConnector {
        connector: ConnectorId,
    },
    /// Send, publish, merge, or otherwise commit through a third party.
    CommitConnector {
        connector: ConnectorId,
    },
    SpendMoney,
}

impl Capability {
    /// The tier this capability alone implies.
    #[must_use]
    pub const fn tier(&self) -> Tier {
        match self {
            Self::ReadFile { .. }
            | Self::ReadMemory
            | Self::Network { .. }
            | Self::Sandbox
            | Self::ReadConnector { .. } => Tier::Contained,
            Self::WriteFile { .. } | Self::WriteMemory | Self::DraftConnector { .. } => {
                Tier::Reversible
            }
            Self::DeleteFile { .. }
            | Self::Process
            | Self::CommitConnector { .. }
            | Self::SpendMoney => Tier::Irreversible,
        }
    }
}

/// The tier a set of capabilities implies, which is the worst of them.
///
/// Assigned by the host. A tool never declares its own tier, because a careless or malicious one
/// would mark everything reversible and skip every confirm.
#[must_use]
pub fn tier_of(capabilities: &[Capability]) -> Tier {
    capabilities
        .iter()
        .map(Capability::tier)
        .max()
        .unwrap_or(Tier::Contained)
}

/// What the user actually granted a tool.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    granted: Vec<Capability>,
}

impl Grant {
    #[must_use]
    pub const fn new(granted: Vec<Capability>) -> Self {
        Self { granted }
    }

    #[must_use]
    pub fn allows(&self, needed: &Capability) -> bool {
        self.granted.contains(needed)
    }

    /// The first capability a tool needs that this grant does not cover.
    #[must_use]
    pub fn missing<'a>(&self, needed: &'a [Capability]) -> Option<&'a Capability> {
        needed.iter().find(|c| !self.allows(c))
    }

    #[must_use]
    pub fn capabilities(&self) -> &[Capability] {
        &self.granted
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectorId(String);

impl ConnectorId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ComponentId(String);

impl ComponentId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a WASM component stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrapKind {
    Unreachable,
    OutOfBounds,
    StackOverflow,
    IntegerOverflow,
    Other,
}

/// Why a tool call failed.
///
/// Typed so the loop can react: retry one rate-limited tool while others continue, ask for a
/// grant, refresh a token, or stop calling a faulting component. An opaque string means the model
/// guesses, and guessing is how agents get stuck in loops.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    #[error("rate limited, retry after {retry_after:?}")]
    RateLimited { retry_after: Duration },
    #[error("not granted: {needed:?}")]
    PermissionDenied { needed: Capability },
    #[error("not found")]
    NotFound,
    #[error("timed out")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
    #[error("budget exceeded")]
    BudgetExceeded,
    #[error("{connector} needs reauthorizing")]
    AuthExpired { connector: ConnectorId },
    /// A component trapped. Distinct from `Timeout`, because a trap and a hang need different
    /// handling and collapsing them makes a faulting component look like a slow one.
    #[error("component {component} trapped: {trap:?}")]
    ComponentFault {
        component: ComponentId,
        trap: TrapKind,
    },
    #[error("upstream returned {0}")]
    Upstream(u16),
}

impl std::fmt::Display for ConnectorId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::fmt::Display for ComponentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_is_the_worst_capability() {
        let read_only = [
            Capability::ReadFile {
                root: "/tmp".into(),
            },
            Capability::ReadMemory,
        ];
        assert_eq!(tier_of(&read_only), Tier::Contained);

        let writes = [
            Capability::ReadFile {
                root: "/tmp".into(),
            },
            Capability::WriteFile {
                root: "/tmp".into(),
            },
        ];
        assert_eq!(tier_of(&writes), Tier::Reversible);

        let sends = [
            Capability::ReadConnector {
                connector: ConnectorId::new("google"),
            },
            Capability::CommitConnector {
                connector: ConnectorId::new("google"),
            },
        ];
        assert_eq!(tier_of(&sends), Tier::Irreversible);
    }

    #[test]
    fn no_capabilities_means_contained() {
        assert_eq!(tier_of(&[]), Tier::Contained);
    }

    #[test]
    fn a_draft_is_reversible_but_sending_is_not() {
        let connector = ConnectorId::new("google");
        assert_eq!(
            Capability::DraftConnector {
                connector: connector.clone()
            }
            .tier(),
            Tier::Reversible
        );
        assert_eq!(
            Capability::CommitConnector { connector }.tier(),
            Tier::Irreversible
        );
    }

    #[test]
    fn grant_reports_the_first_missing_capability() {
        let grant = Grant::new(vec![Capability::ReadMemory]);
        let needed = [Capability::ReadMemory, Capability::WriteMemory];
        assert_eq!(grant.missing(&needed), Some(&Capability::WriteMemory));
        assert_eq!(grant.missing(&[Capability::ReadMemory]), None);
    }

    #[test]
    fn an_empty_grant_allows_nothing() {
        assert!(!Grant::default().allows(&Capability::ReadMemory));
    }
}

//! The core's error type.
//!
//! One enum at the crate boundary, so a caller can react to a class of failure without matching
//! every subsystem's own error. Each variant keeps its source, so nothing is lost on the way up.

use crate::core::cycle::LoopError;
use crate::core::ledger::LedgerError;
use crate::ports::model::ModelError;
use crate::ports::tool::ToolError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Loop(#[from] LoopError),
    #[error(transparent)]
    Model(#[from] ModelError),
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("could not start the runtime: {0}")]
    Runtime(#[source] std::io::Error),
}

impl Error {
    /// Whether retrying the same thing could plausibly succeed.
    ///
    /// A caller that retries a `PermissionDenied` loops forever; one that gives up on a
    /// `RateLimited` gives up too early.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Model(e) => matches!(
                e,
                ModelError::RateLimited(_) | ModelError::Transport(_) | ModelError::Upstream { .. }
            ),
            Self::Tool(e) => matches!(
                e,
                ToolError::RateLimited { .. } | ToolError::Timeout | ToolError::Upstream(_)
            ),
            Self::Loop(_) | Self::Ledger(_) | Self::Runtime(_) => false,
        }
    }

    /// Whether the user has to do something before this can work.
    #[must_use]
    pub const fn needs_user(&self) -> bool {
        match self {
            Self::Model(ModelError::Unauthorized(_)) | Self::Loop(LoopError::OverBudget { .. }) => {
                true
            }
            Self::Tool(e) => matches!(
                e,
                ToolError::PermissionDenied { .. } | ToolError::AuthExpired { .. }
            ),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::tool::{Capability, ConnectorId};
    use std::time::Duration;

    #[test]
    fn rate_limits_are_worth_retrying_and_bad_keys_are_not() {
        assert!(Error::Model(ModelError::RateLimited(None)).is_transient());
        assert!(!Error::Model(ModelError::Unauthorized(String::new())).is_transient());
        assert!(
            Error::Tool(ToolError::RateLimited {
                retry_after: Duration::from_secs(1)
            })
            .is_transient()
        );
    }

    #[test]
    fn things_only_the_user_can_fix_are_marked() {
        assert!(Error::Model(ModelError::Unauthorized(String::new())).needs_user());
        assert!(
            Error::Tool(ToolError::AuthExpired {
                connector: ConnectorId::new("google")
            })
            .needs_user()
        );
        assert!(
            Error::Tool(ToolError::PermissionDenied {
                needed: Capability::ReadMemory
            })
            .needs_user()
        );
        assert!(!Error::Model(ModelError::Cancelled).needs_user());
    }
}

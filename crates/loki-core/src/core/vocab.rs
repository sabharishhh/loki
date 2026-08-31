//! The small closed sets the event stream is described in.
//!
//! Every one of these is an enum rather than a string, so a renderer that forgets a case fails to
//! compile instead of printing something wrong.

/// How reversible an action is.
///
/// Assigned by the host from the capabilities a tool holds. A tool never declares its own tier,
/// because a careless or malicious one would mark everything reversible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tier {
    /// No effect outside the sandbox or the app. Runs with no prompt and no journal entry.
    Contained,
    /// An effect Loki can undo. Runs, journaled, one click to reverse.
    Reversible,
    /// Cannot be taken back. Needs a deliberate confirm before the commit point.
    Irreversible,
}

impl Tier {
    #[must_use]
    pub const fn needs_confirm(self) -> bool {
        matches!(self, Self::Irreversible)
    }

    #[must_use]
    pub const fn is_journaled(self) -> bool {
        matches!(self, Self::Reversible)
    }
}

/// Whether a tool was called directly by the loop or from inside a code-mode script.
///
/// Keeps nested calls visible in the trace rather than hidden behind one opaque script step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallPath {
    Direct,
    Script,
}

/// Where a model runs. A capability, not a setting.
///
/// The prompt gate reads this to decide whether a `private` claim is allowed into a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Locality {
    Cloud,
    OnDevice,
}

/// Which model a call belongs to.
///
/// Routing is by task, not by turn. `Primary` owns the cached conversation prefix for the whole
/// session, so utility work must never share that prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelRole {
    /// The conversation. One model per session.
    Primary,
    /// Bounded structured calls: consolidation, import, entity matching, resume classification.
    Utility,
}

/// Money, in whole cents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Cents(u64);

impl Cents {
    pub const ZERO: Self = Self(0);

    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Saturates at `u64::MAX` rather than wrapping, so a ledger total can never silently
    /// roll over to a small number.
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }
}

/// How a call is priced.
///
/// A model rather than a number, so local inference records free instead of a wrong estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CostModel {
    Free,
    PerToken {
        input_per_mtok: Cents,
        output_per_mtok: Cents,
    },
}

impl CostModel {
    #[must_use]
    pub const fn charge(self, tokens_in: u32, tokens_out: u32) -> Cents {
        match self {
            Self::Free => Cents::ZERO,
            Self::PerToken {
                input_per_mtok,
                output_per_mtok,
            } => {
                const PER_MTOK: u64 = 1_000_000;
                let input = input_per_mtok.get().saturating_mul(tokens_in as u64) / PER_MTOK;
                let output = output_per_mtok.get().saturating_mul(tokens_out as u64) / PER_MTOK;
                Cents::new(input.saturating_add(output))
            }
        }
    }
}

/// What a scope is holding while it is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeKind {
    Tool,
    Search,
    Model,
    Memory,
    Script,
}

/// How a task ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskStatus {
    Completed,
    Interrupted,
    Failed,
    Blocked,
}

/// Why the loop stopped and handed control back to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockReason {
    /// The spend ceiling was reached. Checked before a model call, never during one.
    BudgetCeiling { spent: Cents, ceiling: Cents },
    /// A Tier 3 action is waiting on a confirm.
    AwaitingConfirm { action: String },
    /// Two conflicting claims, neither clearly newer. Neither is used until resolved.
    ConflictUnresolved { concept: String },
    /// A connector's credentials expired and need reauthorizing.
    AuthExpired { connector: String },
}

/// What an action did to the world. Determines how it is reversed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind {
    FileWrite,
    FileDelete,
    FileMove,
    MemoryWrite,
    ConnectorWrite,
}

/// What a memory write did to a concept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WriteOp {
    Created,
    Appended,
    Edited,
    Promoted,
    Invalidated,
    Deprecated,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_irreversible_needs_a_confirm() {
        assert!(!Tier::Contained.needs_confirm());
        assert!(!Tier::Reversible.needs_confirm());
        assert!(Tier::Irreversible.needs_confirm());
    }

    #[test]
    fn only_reversible_is_journaled() {
        assert!(!Tier::Contained.is_journaled());
        assert!(Tier::Reversible.is_journaled());
        assert!(!Tier::Irreversible.is_journaled());
    }

    #[test]
    fn tiers_order_by_severity() {
        assert!(Tier::Contained < Tier::Reversible);
        assert!(Tier::Reversible < Tier::Irreversible);
    }

    #[test]
    fn local_inference_is_free() {
        assert_eq!(CostModel::Free.charge(50_000, 10_000), Cents::ZERO);
    }

    #[test]
    fn per_token_charges_both_directions() {
        let pricing = CostModel::PerToken {
            input_per_mtok: Cents::new(300),
            output_per_mtok: Cents::new(1500),
        };
        assert_eq!(pricing.charge(1_000_000, 1_000_000), Cents::new(1800));
        assert_eq!(pricing.charge(500_000, 0), Cents::new(150));
    }

    #[test]
    fn cents_saturate_instead_of_wrapping() {
        let max = Cents::new(u64::MAX);
        assert_eq!(max.saturating_add(Cents::new(1)), max);
    }
}

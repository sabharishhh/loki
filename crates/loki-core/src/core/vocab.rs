//! The small closed sets the event stream is described in.
//!
//! Every one of these is an enum rather than a string, so a renderer that forgets a case fails to
//! compile instead of printing something wrong.

use serde::{Deserialize, Serialize};

/// How reversible an action is.
///
/// Assigned by the host from the capabilities a tool holds. A tool never declares its own tier,
/// because a careless or malicious one would mark everything reversible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallPath {
    Direct,
    Script,
}

/// Which lane a retrieval ran on (§10.1, §10.8).
///
/// Counted separately in §10.6's log because they are different evidence: lane 1 chose the claim
/// out of a ranked corpus, and on lane 2 the agent went looking for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    /// Automatic recall, on every turn, no model call.
    Automatic,
    /// The agent searching memory directly, under a budget.
    Deliberate,
}

impl Lane {
    /// The word written into the recall log.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Deliberate => "deliberate",
        }
    }
}

/// Which step of §12.2's ladder answered a fetch.
///
/// Recorded on every fetch because §12.9 requires it and §21.5 scores the distribution: the share
/// of fetches each rung answers is the only honest early warning that the free path is failing.
/// Nothing emits it until Phase 5, and adding the field afterwards is a Ring 1 change made twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rung {
    /// `wreq`. TLS and HTTP2 parity, no browser.
    Direct,
    /// CDP over the user's installed Chromium, behind our own exit (§12.3).
    Rendered,
    /// Interaction: a login, a form, a multi-step flow. Designed, not shipped (§12.4).
    Interactive,
}

/// Why a rung was not satisfied, which is what decides where the ladder goes next (§12.2).
///
/// **A reason rather than a boolean, and this is the load-bearing decision of the ladder.** A rung
/// that only says "failed" leaves the caller with one move, which is to try harder, and trying
/// harder against a block is what turns a soft flag into a ban. Every rung returns one of these,
/// and the verdict rides in `Fetched` so §12.9 can say why the ladder climbed rather than only how
/// often.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Real content that passed §21.5's readiness check.
    Ok,
    /// A 200 carrying a shell where content should be. Escalates to a browser.
    JsRequired,
    /// Recognised and refused: 403, a challenge page, a 429 with nothing to wait for.
    ///
    /// Escalates, and is **never** retried on the same rung. The response already said the request
    /// was recognised, and repeating it is the behaviour that hardens a host against this address.
    Blocked,
    /// A 429 that said how long to wait.
    ///
    /// Backs off the whole domain and never escalates. A browser arriving seconds after a rate
    /// limit reads as evasion rather than as retrieval, which is the opposite of §12.5's posture.
    RateLimited,
    /// 404 or 410. The ladder stops: another rung cannot make a page exist.
    NotFound,
    /// A login wall or a form gate. §12.4's rung when it exists, honest exhaustion until then.
    InteractionRequired,
    /// Every rung tried, nothing readable. §12.4's honest failure.
    Exhausted,
}

impl Verdict {
    /// Whether climbing to the next rung is the right response.
    ///
    /// Deliberately not the negation of `Ok`. Three of these are terminal for opposite reasons: a
    /// page that does not exist, a host asking to be left alone, and a ladder already spent.
    #[must_use]
    pub const fn should_escalate(self) -> bool {
        matches!(self, Self::JsRequired | Self::Blocked)
    }

    /// Whether the same rung may try again.
    ///
    /// Only ever true for a transport failure, which is not a verdict at all, so this is always
    /// false. It exists to be the one place the question is answered, because "should I retry"
    /// asked at a call site is answered optimistically (§12.2).
    #[must_use]
    pub const fn may_retry(self) -> bool {
        false
    }
}

/// Where a model runs. A capability, not a setting.
///
/// The prompt gate reads this to decide whether a `private` claim is allowed into a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Locality {
    Cloud,
    OnDevice,
}

/// Which model a call belongs to.
///
/// Routing is by task, not by turn. `Primary` owns the cached conversation prefix for the whole
/// session, so utility work must never share that prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// The conversation. One model per session.
    Primary,
    /// Bounded structured calls: consolidation, import, entity matching, resume classification.
    Utility,
}

/// Money, in whole cents.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostModel {
    Free,
    PerToken {
        input_per_mtok: Cents,
        output_per_mtok: Cents,
    },
}

/// One millionth of a cent. What a rate times a token count naturally produces.
///
/// Exists because whole cents cannot hold the cost of one call. A typical turn costs a few
/// tenths of a cent, so rounding each one to whole cents records zero and the budget ceiling
/// never trips. Accumulate in these and convert once, at the point a human reads the number.
pub const MICRO_CENTS_PER_CENT: u64 = 1_000_000;

impl CostModel {
    /// Exact cost of one call, in millionths of a cent.
    ///
    /// A rate is cents per million tokens, so tokens times rate is already micro-cents. No
    /// division, so nothing is lost.
    #[must_use]
    pub const fn charge_micros(self, tokens_in: u32, tokens_out: u32) -> u64 {
        match self {
            Self::Free => 0,
            Self::PerToken {
                input_per_mtok,
                output_per_mtok,
            } => {
                let input = input_per_mtok.get().saturating_mul(tokens_in as u64);
                let output = output_per_mtok.get().saturating_mul(tokens_out as u64);
                input.saturating_add(output)
            }
        }
    }

    /// Cost of one call rounded down to whole cents.
    ///
    /// Lossy for a single call. Use [`Self::charge_micros`] for anything that accumulates.
    #[must_use]
    pub const fn charge(self, tokens_in: u32, tokens_out: u32) -> Cents {
        Cents::new(self.charge_micros(tokens_in, tokens_out) / MICRO_CENTS_PER_CENT)
    }
}

/// What a scope is holding while it is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Tool,
    Search,
    Model,
    Memory,
    Script,
}

/// How a task ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Completed,
    Interrupted,
    Failed,
    Blocked,
}

/// Why the loop stopped and handed control back to the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    /// The monthly ceiling was reached. Checked before a model call, never during one.
    BudgetCeiling { spent: Cents, ceiling: Cents },
    /// The per-session ceiling was reached. A runaway run, not a spent month.
    SessionCeiling { spent: Cents, ceiling: Cents },
    /// A Tier 3 action is waiting on a confirm.
    AwaitingConfirm { action: String },
    /// Two conflicting claims, neither clearly newer. Neither is used until resolved.
    ConflictUnresolved { concept: String },
    /// A connector's credentials expired and need reauthorizing.
    AuthExpired { connector: String },
    /// The model provider refused or failed. `detail` is already plain enough to show.
    ProviderFailed { provider: String, detail: String },
}

/// What an action did to the world. Determines how it is reversed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    FileWrite,
    FileDelete,
    FileMove,
    MemoryWrite,
    ConnectorWrite,
}

/// What a memory write did to a concept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
        assert_eq!(CostModel::Free.charge_micros(50_000, 10_000), 0);
    }

    #[test]
    fn the_two_directions_are_summed_before_rounding() {
        // gpt-5.6-terra: 400 in, 500 out per million.
        // 12k in is 4.8 cents, 800 out is 0.4, so 5.2 total.
        let terra = CostModel::PerToken {
            input_per_mtok: Cents::new(400),
            output_per_mtok: Cents::new(500),
        };
        assert_eq!(terra.charge_micros(12_000, 800), 5_200_000);
        // Rounding each direction separately first would give 4 + 0, not 5.
        assert_eq!(terra.charge(12_000, 800), Cents::new(5));
    }

    #[test]
    fn a_cheap_model_still_accrues() {
        // gpt-5.6-luna: 40 in, 50 out. One turn is well under a cent and must not record zero.
        let luna = CostModel::PerToken {
            input_per_mtok: Cents::new(40),
            output_per_mtok: Cents::new(50),
        };
        assert_eq!(luna.charge(12_000, 800), Cents::ZERO, "whole cents lose it");
        assert_eq!(luna.charge_micros(12_000, 800), 520_000, "micros keep it");
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

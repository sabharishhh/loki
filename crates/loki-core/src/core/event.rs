//! The one event stream.
//!
//! Every consumer reads from here: both renderers, the ledger, the undo journal, the session
//! summary, and the checkpoint recorder. Nothing acts outside it.

use serde::{Deserialize, Serialize};

use super::ids::{ActionId, ClaimId, ConceptId, ContentHash, QueryHash, ScopeId, StepId, TaskId};
use super::payload::{Args, PartialOutput, ToolOutput};
use super::vocab::{
    ActionKind, BlockReason, CallPath, Cents, CostModel, Lane, Locality, ModelRole, Rung,
    ScopeKind, TaskStatus, Tier, WriteOp,
};

/// Something the system did.
///
/// Serialized as `{"event": "tool_called", ...}` so the Swift side can switch on one field.
/// The tag is `event` rather than `kind` because `ScopeOpened` already has a `kind` field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    TaskStarted {
        id: TaskId,
        summary: String,
    },
    ScopeOpened {
        id: ScopeId,
        parent: Option<ScopeId>,
        kind: ScopeKind,
    },
    ScopeClosed {
        id: ScopeId,
        ms: u64,
    },
    ToolCalled {
        tool: String,
        args: Args,
        tier: Tier,
        via: CallPath,
    },
    ToolProgress {
        tool: String,
        partial: PartialOutput,
    },
    ToolReturned {
        tool: String,
        result: ToolOutput,
        ms: u64,
    },
    Searched {
        task: TaskId,
        query: String,
        provider: String,
        hits: u32,
        cost: CostModel,
    },
    Fetched {
        task: TaskId,
        url: String,
        hash: ContentHash,
        /// Which step of §12.2's ladder answered. §21.5 scores the distribution, and nothing
        /// emits this until Phase 5: adding the field later is a Ring 1 change made twice.
        rung: Rung,
        cost: CostModel,
    },
    ActionJournaled {
        action: ActionId,
        what: ActionKind,
        reversible: bool,
    },
    ActionUndone {
        action: ActionId,
    },
    MemoryRecalled {
        /// Claims, not concepts. §10.6's log counts a claim, and a concept is too coarse to say
        /// which line answered.
        claim_ids: Vec<ClaimId>,
        lane: Lane,
        query_hash: QueryHash,
    },
    MemoryWritten {
        op: WriteOp,
        concept_id: ConceptId,
    },
    ModelCall {
        task: TaskId,
        provider: String,
        role: ModelRole,
        locality: Locality,
        tokens_in: u32,
        tokens_out: u32,
        cost: CostModel,
    },
    BudgetWarning {
        spent: Cents,
        ceiling: Cents,
    },
    Blocked {
        reason: BlockReason,
    },
    Interrupted {
        id: TaskId,
        at_step: u32,
        kept: Vec<StepId>,
        dropped: Vec<StepId>,
    },
    Resumed {
        id: TaskId,
        reused: Vec<StepId>,
    },
    TaskFinished {
        id: TaskId,
        status: TaskStatus,
    },
}

impl Event {
    /// The task this event belongs to, where the event names one.
    ///
    /// Every billable event carries it, so cost can be attributed without inferring the task from
    /// the order events arrived in. Scope and tool events do not: the loop knows the task from
    /// context and duplicating it everywhere would be noise.
    #[must_use]
    pub const fn task(&self) -> Option<TaskId> {
        match self {
            Self::TaskStarted { id, .. }
            | Self::Interrupted { id, .. }
            | Self::Resumed { id, .. }
            | Self::TaskFinished { id, .. } => Some(*id),
            Self::Searched { task, .. }
            | Self::Fetched { task, .. }
            | Self::ModelCall { task, .. } => Some(*task),
            _ => None,
        }
    }

    /// Whether this event moves money, so the ledger knows what to record.
    #[must_use]
    pub const fn is_billable(&self) -> bool {
        matches!(
            self,
            Self::Searched { .. } | Self::Fetched { .. } | Self::ModelCall { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Event> {
        vec![
            Event::TaskStarted {
                id: TaskId::new(0),
                summary: "pull the infra tickets".into(),
            },
            Event::ScopeOpened {
                id: ScopeId::new(0),
                parent: None,
                kind: ScopeKind::Tool,
            },
            Event::ToolCalled {
                tool: "github.read_issues".into(),
                args: Args::new(serde_json::json!({ "assignee": "me" })),
                tier: Tier::Contained,
                via: CallPath::Direct,
            },
            Event::ToolReturned {
                tool: "github.read_issues".into(),
                result: ToolOutput::new(serde_json::json!({ "count": 3 })),
                ms: 410,
            },
            Event::ScopeClosed {
                id: ScopeId::new(0),
                ms: 1240,
            },
            Event::MemoryRecalled {
                claim_ids: vec![ClaimId::new(ConceptId::new("people/meera.md"), 0)],
                lane: Lane::Automatic,
                query_hash: QueryHash::new("0f1e2d3c4b5a6978"),
            },
            Event::MemoryWritten {
                op: WriteOp::Invalidated,
                concept_id: ConceptId::new("people/meera.md"),
            },
            Event::ModelCall {
                task: TaskId::new(0),
                provider: "anthropic".into(),
                role: ModelRole::Primary,
                locality: Locality::Cloud,
                tokens_in: 1200,
                tokens_out: 340,
                cost: CostModel::PerToken {
                    input_per_mtok: Cents::new(300),
                    output_per_mtok: Cents::new(1500),
                },
            },
            Event::Fetched {
                rung: Rung::Direct,
                task: TaskId::new(0),
                url: "https://example.com".into(),
                hash: ContentHash::new("deadbeef"),
                cost: CostModel::Free,
            },
            Event::ActionJournaled {
                action: ActionId::new(0),
                what: ActionKind::FileWrite,
                reversible: true,
            },
            Event::ActionUndone {
                action: ActionId::new(0),
            },
            Event::BudgetWarning {
                spent: Cents::new(800),
                ceiling: Cents::new(1000),
            },
            Event::Blocked {
                reason: BlockReason::AwaitingConfirm {
                    action: "gmail.send_message".into(),
                },
            },
            Event::Interrupted {
                id: TaskId::new(0),
                at_step: 4,
                kept: vec![StepId::new(0), StepId::new(1)],
                dropped: vec![StepId::new(2)],
            },
            Event::Resumed {
                id: TaskId::new(0),
                reused: vec![StepId::new(0), StepId::new(1)],
            },
            Event::TaskFinished {
                id: TaskId::new(0),
                status: TaskStatus::Completed,
            },
            Event::Searched {
                task: TaskId::new(0),
                query: "colocation tariff".into(),
                provider: "rquest".into(),
                hits: 9,
                cost: CostModel::Free,
            },
            Event::ToolProgress {
                tool: "shell".into(),
                partial: PartialOutput::new("compiling"),
            },
        ]
    }

    #[test]
    fn every_variant_survives_a_json_round_trip() {
        for event in sample() {
            let json = serde_json::to_string(&event).expect("serialize");
            let back: Event = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(event, back, "round trip changed the event");
        }
    }

    #[test]
    fn serializes_with_a_kind_tag() {
        let event = Event::ScopeClosed {
            id: ScopeId::new(3),
            ms: 12,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["event"], "scope_closed");
        assert_eq!(json["ms"], 12);
    }

    #[test]
    fn every_billable_event_names_its_task() {
        for event in sample().into_iter().filter(Event::is_billable) {
            assert!(
                event.task().is_some(),
                "billable event with no task: {event:?}"
            );
        }
    }

    #[test]
    fn task_events_carry_their_task_id() {
        let started = Event::TaskStarted {
            id: TaskId::new(4),
            summary: String::new(),
        };
        assert_eq!(started.task(), Some(TaskId::new(4)));

        let closed = Event::ScopeClosed {
            id: ScopeId::new(0),
            ms: 0,
        };
        assert_eq!(closed.task(), None);
    }

    #[test]
    fn only_spending_events_are_billable() {
        let billable = sample().into_iter().filter(Event::is_billable).count();
        assert_eq!(billable, 3);
    }
}

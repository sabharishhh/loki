//! The two renderers.
//!
//! Both read the same typed events, which is why the plain view and the trace view cannot drift.
//! Neither can skip a variant, because the compiler checks the match.

use super::event::Event;
use super::vocab::{ActionKind, BlockReason, CallPath, Lane, ScopeKind, Tier, WriteOp};

/// One line of plain language, or nothing when an event is not worth showing.
///
/// Returns `None` for events the default view deliberately hides. A confidence bump is not news,
/// and neither is the internal bookkeeping of a scope closing.
#[must_use]
pub fn plain(event: &Event) -> Option<String> {
    Some(match event {
        Event::ToolCalled { tool, tier, .. } => match tier {
            Tier::Irreversible => format!("Waiting for you to confirm {tool}."),
            _ => format!("Using {tool}."),
        },
        Event::Searched { query, .. } => format!("Searching for {query}."),
        Event::Fetched { url, .. } => format!("Reading {url}."),
        // Lane 2 carries no claim ids: it returns file lines, not addressed claims. It still has
        // to say something, because it is the one retrieval the user waits on.
        Event::MemoryRecalled {
            claim_ids, lane, ..
        } => match (lane, claim_ids.len()) {
            (Lane::Deliberate, _) => "Searching my memory.".into(),
            (_, 0) => return None,
            (_, 1) => "Recalling one thing I know.".into(),
            (_, n) => format!("Recalling {n} things I know."),
        },
        Event::MemoryWritten { op, concept_id } => {
            let what = concept_id.as_str();
            match op {
                WriteOp::Created => format!("Noting something new about {what}."),
                WriteOp::Invalidated => {
                    format!("Marking what I had about {what} as no longer true.")
                }
                WriteOp::Deprecated => format!("Retiring {what}."),
                WriteOp::Appended | WriteOp::Edited | WriteOp::Promoted => return None,
            }
        }
        Event::ActionJournaled { what, .. } => match what {
            ActionKind::FileWrite => "Saved the file. You can undo this.".into(),
            ActionKind::FileDelete => "Moved the file to the trash. You can undo this.".into(),
            ActionKind::FileMove => "Moved the file. You can undo this.".into(),
            ActionKind::ConnectorWrite => "Saved a draft. You can undo this.".into(),
            ActionKind::MemoryWrite => return None,
        },
        Event::ActionUndone { .. } => "Undone.".into(),
        Event::BudgetWarning { spent, ceiling } => format!(
            "Spend is at {} of {} cents for this month.",
            spent.get(),
            ceiling.get()
        ),
        Event::Blocked { reason } => match reason {
            BlockReason::BudgetCeiling { spent, .. } => {
                format!(
                    "Paused at your monthly spending limit, {} cents used.",
                    spent.get()
                )
            }
            BlockReason::SessionCeiling { spent, ceiling } => format!(
                "Paused. This session has spent {} of its {} cent limit.",
                spent.get(),
                ceiling.get()
            ),
            BlockReason::AwaitingConfirm { action } => format!("Waiting on you before {action}."),
            BlockReason::ConflictUnresolved { concept } => {
                format!("Two things I know about {concept} disagree. Which is right?")
            }
            BlockReason::AuthExpired { connector } => {
                format!("The connection to {connector} expired. Reconnect it.")
            }
            BlockReason::ProviderFailed { provider, detail } => {
                format!("{provider} could not answer: {detail}")
            }
        },
        Event::Interrupted { kept, dropped, .. } => format!(
            "Stopped. Kept {} steps, dropped {}.",
            kept.len(),
            dropped.len()
        ),
        Event::Resumed { reused, .. } => {
            format!("Carrying on, reusing {} steps.", reused.len())
        }
        // A failure is always preceded by a `Blocked` saying why, so there is nothing to add.
        Event::TaskFinished { .. } => return None,
        Event::TaskStarted { .. }
        | Event::ScopeOpened { .. }
        | Event::ScopeClosed { .. }
        | Event::ToolProgress { .. }
        | Event::ToolReturned { .. }
        | Event::ModelCall { .. } => return None,
    })
}

/// One dense line for the trace view. Every event renders.
#[must_use]
pub fn trace(event: &Event) -> String {
    match event {
        Event::TaskStarted { id, summary } => {
            format!("TaskStarted task={} {summary:?}", id.get())
        }
        Event::ScopeOpened { id, parent, kind } => {
            let parent = parent.map_or_else(|| "-".into(), |p| p.get().to_string());
            format!(
                "ScopeOpened scope={} parent={parent} kind={}",
                id.get(),
                scope_kind(*kind)
            )
        }
        Event::ScopeClosed { id, ms } => format!("ScopeClosed scope={} {ms}ms", id.get()),
        Event::ToolCalled {
            tool,
            args,
            tier,
            via,
        } => format!(
            "ToolCalled {tool} tier={} via={} args={}",
            tier_name(*tier),
            call_path(*via),
            args.as_value()
        ),
        Event::ToolProgress { tool, partial } => {
            format!("ToolProgress {tool} {:?}", partial.as_str())
        }
        Event::ToolReturned { tool, result, ms } => {
            format!("ToolReturned {tool} {ms}ms {}", result.as_value())
        }
        Event::Searched {
            query,
            provider,
            hits,
            ..
        } => format!("Searched {provider} hits={hits} {query:?}"),
        Event::Fetched { url, hash, .. } => format!("Fetched {url} hash={}", hash.as_str()),
        Event::ActionJournaled {
            action,
            what,
            reversible,
        } => format!(
            "ActionJournaled action={} what={what:?} reversible={reversible}",
            action.get()
        ),
        Event::ActionUndone { action } => format!("ActionUndone action={}", action.get()),
        Event::MemoryRecalled {
            claim_ids,
            lane,
            query_hash,
        } => {
            let ids: Vec<String> = claim_ids.iter().map(ToString::to_string).collect();
            format!(
                "MemoryRecalled lane={} query={} {}",
                lane.name(),
                query_hash.as_str(),
                ids.join(", ")
            )
        }
        Event::MemoryWritten { op, concept_id } => {
            format!("MemoryWritten {op:?} {}", concept_id.as_str())
        }
        Event::ModelCall {
            provider,
            role,
            locality,
            tokens_in,
            tokens_out,
            cost,
            ..
        } => format!(
            "ModelCall {provider} role={role:?} locality={locality:?} in={tokens_in} out={tokens_out} cost={}c",
            cost.charge(*tokens_in, *tokens_out).get()
        ),
        Event::BudgetWarning { spent, ceiling } => {
            format!("BudgetWarning {}c of {}c", spent.get(), ceiling.get())
        }
        Event::Blocked { reason } => format!("Blocked {reason:?}"),
        Event::Interrupted {
            id,
            at_step,
            kept,
            dropped,
        } => format!(
            "Interrupted task={} step={at_step} kept={} dropped={}",
            id.get(),
            kept.len(),
            dropped.len()
        ),
        Event::Resumed { id, reused } => {
            format!("Resumed task={} reused={}", id.get(), reused.len())
        }
        Event::TaskFinished { id, status } => {
            format!("TaskFinished task={} {status:?}", id.get())
        }
    }
}

const fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Contained => "1",
        Tier::Reversible => "2",
        Tier::Irreversible => "3",
    }
}

const fn call_path(via: CallPath) -> &'static str {
    match via {
        CallPath::Direct => "direct",
        CallPath::Script => "script",
    }
}

const fn scope_kind(kind: ScopeKind) -> &'static str {
    match kind {
        ScopeKind::Tool => "tool",
        ScopeKind::Search => "search",
        ScopeKind::Model => "model",
        ScopeKind::Memory => "memory",
        ScopeKind::Script => "script",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ids::{ActionId, ConceptId, ScopeId, TaskId};
    use crate::core::payload::Args;

    #[test]
    fn a_failure_says_why_rather_than_that_it_failed() {
        let blocked = Event::Blocked {
            reason: BlockReason::ProviderFailed {
                provider: "openai".into(),
                detail: "the API key was rejected".into(),
            },
        };
        assert_eq!(
            plain(&blocked).unwrap(),
            "openai could not answer: the API key was rejected"
        );
        // The finish carries no message of its own, so the reason is not repeated.
        assert!(
            plain(&Event::TaskFinished {
                id: TaskId::new(0),
                status: crate::core::vocab::TaskStatus::Failed,
            })
            .is_none()
        );
    }

    #[test]
    fn plain_hides_internal_bookkeeping() {
        assert!(
            plain(&Event::ScopeClosed {
                id: ScopeId::new(0),
                ms: 10
            })
            .is_none()
        );
        assert!(
            plain(&Event::MemoryWritten {
                op: WriteOp::Promoted,
                concept_id: ConceptId::new("people/meera.md"),
            })
            .is_none()
        );
    }

    #[test]
    fn plain_speaks_without_jargon() {
        let line = plain(&Event::ActionJournaled {
            action: ActionId::new(0),
            what: ActionKind::FileDelete,
            reversible: true,
        })
        .unwrap();
        assert_eq!(line, "Moved the file to the trash. You can undo this.");
    }

    #[test]
    fn plain_flags_a_tier_three_call_differently() {
        let call = |tier| Event::ToolCalled {
            tool: "gmail.send".into(),
            args: Args::default(),
            tier,
            via: CallPath::Direct,
        };
        assert_eq!(plain(&call(Tier::Contained)).unwrap(), "Using gmail.send.");
        assert_eq!(
            plain(&call(Tier::Irreversible)).unwrap(),
            "Waiting for you to confirm gmail.send."
        );
    }

    #[test]
    fn trace_renders_every_event() {
        let events = [
            Event::TaskStarted {
                id: TaskId::new(0),
                summary: "s".into(),
            },
            Event::ScopeOpened {
                id: ScopeId::new(1),
                parent: Some(ScopeId::new(0)),
                kind: ScopeKind::Script,
            },
            Event::ActionUndone {
                action: ActionId::new(2),
            },
        ];
        for event in &events {
            assert!(!trace(event).is_empty());
        }
        assert_eq!(
            trace(&events[1]),
            "ScopeOpened scope=1 parent=0 kind=script"
        );
    }
}

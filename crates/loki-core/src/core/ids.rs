//! Identifiers used across the event stream.
//!
//! Newtypes rather than bare integers, so the compiler rejects passing one where another is
//! expected. Ids are per run and are not stable across restarts. Nothing on disk refers to them.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

macro_rules! counter_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(u64);

        impl $name {
            #[must_use]
            pub const fn new(raw: u64) -> Self {
                Self(raw)
            }

            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}

counter_id! {
    /// One thing the user asked for, from the message that started it to the final answer.
    ///
    /// Interrupt and resume both act on a task, and the Activity screen shows one row per task.
    TaskId
}

counter_id! {
    /// A stretch of work that holds resources while it runs. Scopes nest.
    ///
    /// A scope that never closes is a leaked resource, visible as an unclosed rail in the thread.
    ScopeId
}

counter_id! {
    /// One step inside a task, usually a single tool call.
    ///
    /// Checkpoints record results per step, which is what lets a resume keep work rather than
    /// redo it.
    StepId
}

counter_id! {
    /// One action that changed something outside the app.
    ///
    /// Tier 2 actions are journaled under this id and undone by it.
    ActionId
}

/// Names one concept document in the memory bundle, by path relative to the bundle root.
///
/// For example `people/meera.md`. Unlike the counter ids this is stable, because it is what the
/// file is actually called.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConceptId(String);

impl ConceptId {
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The memory bundle as it stood at a point in time.
///
/// A git revision of the bundle. A checkpoint records one so a resume knows what memory looked
/// like when the task started, rather than assuming it is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SnapshotId(String);

impl SnapshotId {
    #[must_use]
    pub fn new(revision: impl Into<String>) -> Self {
        Self(revision.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Content hash of a fetched page, used to address it in the evidence store.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContentHash(String);

impl ContentHash {
    #[must_use]
    pub fn new(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Hands out unique ids for one run.
///
/// Shared across threads, so every counter is atomic. Counters are separate per type, which keeps
/// each sequence dense and readable in a trace.
#[derive(Debug, Default)]
pub struct IdGen {
    next_task: AtomicU64,
    next_scope: AtomicU64,
    next_step: AtomicU64,
    next_action: AtomicU64,
}

impl IdGen {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_task: AtomicU64::new(0),
            next_scope: AtomicU64::new(0),
            next_step: AtomicU64::new(0),
            next_action: AtomicU64::new(0),
        }
    }

    pub fn task(&self) -> TaskId {
        TaskId::new(self.next_task.fetch_add(1, Ordering::Relaxed))
    }

    pub fn scope(&self) -> ScopeId {
        ScopeId::new(self.next_scope.fetch_add(1, Ordering::Relaxed))
    }

    pub fn step(&self) -> StepId {
        StepId::new(self.next_step.fetch_add(1, Ordering::Relaxed))
    }

    pub fn action(&self) -> ActionId {
        ActionId::new(self.next_action.fetch_add(1, Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_unwraps() {
        assert_eq!(TaskId::new(7).get(), 7);
        assert_eq!(
            ConceptId::new("people/meera.md").as_str(),
            "people/meera.md"
        );
    }

    #[test]
    fn ids_are_sequential_from_zero() {
        let ids = IdGen::new();
        assert_eq!(ids.task(), TaskId::new(0));
        assert_eq!(ids.task(), TaskId::new(1));
        assert_eq!(ids.scope(), ScopeId::new(0));
    }

    #[test]
    fn counters_are_independent() {
        let ids = IdGen::new();
        ids.task();
        ids.task();
        assert_eq!(ids.scope(), ScopeId::new(0));
        assert_eq!(ids.step(), StepId::new(0));
        assert_eq!(ids.action(), ActionId::new(0));
    }

    #[test]
    fn usable_as_a_map_key() {
        use std::collections::HashMap;
        let mut open = HashMap::new();
        open.insert(ScopeId::new(1), "github");
        assert_eq!(open.get(&ScopeId::new(1)), Some(&"github"));
    }

    #[test]
    fn no_duplicates_across_threads() {
        use std::collections::HashSet;
        use std::sync::Arc;

        let ids = Arc::new(IdGen::new());
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let ids = Arc::clone(&ids);
                std::thread::spawn(move || (0..1000).map(|_| ids.task()).collect::<Vec<_>>())
            })
            .collect();

        let all: Vec<TaskId> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        let unique: HashSet<TaskId> = all.iter().copied().collect();

        assert_eq!(all.len(), 8000);
        assert_eq!(unique.len(), 8000, "an id was handed out twice");
    }
}

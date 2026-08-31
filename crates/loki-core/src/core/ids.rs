//! Identifiers used across the event stream.
//!
//! Each is a newtype over `u64` rather than a bare `u64`, so the compiler refuses to let one be
//! passed where another is expected. Ids are generated per run and are not stable across
//! restarts. Nothing on disk refers to them.
//!
//! [`IdGen`] hands them out. Take ids from it rather than building them by hand.

use std::sync::atomic::{AtomicU64, Ordering};

/// Identifies one task, meaning one thing the user asked for.
///
/// A task begins when a message arrives and ends when the loop produces a final answer, or when
/// the user starts something different. The Activity screen shows one row per task, and
/// interrupting and resuming both act on a task.
///
/// A task record belongs only to the current run. Nothing outside it can reach in and change it
/// mid-run, which is what makes checkpoints trustworthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(u64);

impl TaskId {
    /// Wraps a raw number as a task id.
    ///
    /// Prefer taking ids from the generator rather than building them by hand. This exists for
    /// tests and for rebuilding an id that crossed the bridge.
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw number inside, for serializing or for crossing the C ABI.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Identifies one scope, meaning a stretch of work that holds resources while it runs.
///
/// Scopes nest. A tool call opens one, and a code-mode script opens one containing the calls it
/// makes. Each renders as a vertical rail in the thread, drawing while open and closing when the
/// resources are released.
///
/// A scope that never closes is a leaked resource, and because it is drawn, the user sees it
/// before we find it in a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeId(u64);

impl ScopeId {
    /// Wraps a raw number as a scope id.
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw number inside, for serializing or for crossing the C ABI.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Hands out unique ids for one run.
///
/// Shared by every part of the core that opens a task or a scope, so it is used from many threads
/// at once. Each counter is atomic, meaning an increment cannot be interleaved with another and
/// produce a duplicate.
///
/// Counters are separate per type, so task ids and scope ids each run 0, 1, 2 and stay easy to
/// read in a trace. Two ids of different types sharing a number is harmless, because the compiler
/// will not let the two types meet.
///
/// Numbering restarts at zero on every run. Nothing persisted refers to these.
#[derive(Debug, Default)]
pub struct IdGen {
    next_task: AtomicU64,
    next_scope: AtomicU64,
}

impl IdGen {
    /// A generator starting from zero.
    pub const fn new() -> Self {
        Self {
            next_task: AtomicU64::new(0),
            next_scope: AtomicU64::new(0),
        }
    }

    /// The next task id. Never returns the same value twice.
    pub fn task(&self) -> TaskId {
        TaskId::new(self.next_task.fetch_add(1, Ordering::Relaxed))
    }

    /// The next scope id. Never returns the same value twice.
    pub fn scope(&self) -> ScopeId {
        ScopeId::new(self.next_scope.fetch_add(1, Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_and_unwraps() {
        assert_eq!(TaskId::new(7).get(), 7);
        assert_eq!(ScopeId::new(7).get(), 7);
    }

    #[test]
    fn same_type_compares_by_value() {
        assert_eq!(TaskId::new(1), TaskId::new(1));
        assert_ne!(TaskId::new(1), TaskId::new(2));
    }

    #[test]
    fn usable_as_a_map_key() {
        use std::collections::HashMap;
        let mut open = HashMap::new();
        open.insert(ScopeId::new(1), "github");
        assert_eq!(open.get(&ScopeId::new(1)), Some(&"github"));
    }

    // Note: `TaskId::new(1) == ScopeId::new(1)` does not compile, which is the whole point of
    // these newtypes. There is no test for it here because a compile error cannot be asserted
    // with an ordinary test.

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
        // Scope numbering is unaffected by how many tasks were handed out.
        assert_eq!(ids.scope(), ScopeId::new(0));
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

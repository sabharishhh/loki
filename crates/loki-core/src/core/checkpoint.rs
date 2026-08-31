//! Session-scoped resume points.
//!
//! A checkpoint answers "where was I in this task". The undo journal answers "what did this change
//! in the world". They look similar and do different jobs, so they stay separate: a checkpoint
//! lives for the session and is not persisted, the journal survives a restart.

use super::ids::{ScopeId, StepId, TaskId};
use super::payload::ToolOutput;

/// Where a task had reached.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Checkpoint {
    pub task: Option<TaskId>,
    pub step: u32,
    pub results: Vec<(StepId, ToolOutput)>,
    pub open_scopes: Vec<ScopeId>,
}

impl Checkpoint {
    #[must_use]
    pub fn new(task: TaskId) -> Self {
        Self {
            task: Some(task),
            ..Self::default()
        }
    }

    /// Records a completed step so a resume can reuse it instead of redoing it.
    pub fn record(&mut self, step: StepId, output: ToolOutput) {
        self.step += 1;
        self.results.push((step, output));
    }

    pub fn open_scope(&mut self, scope: ScopeId) {
        self.open_scopes.push(scope);
    }

    pub fn close_scope(&mut self, scope: ScopeId) {
        self.open_scopes.retain(|s| *s != scope);
    }

    #[must_use]
    pub fn steps(&self) -> Vec<StepId> {
        self.results.iter().map(|(step, _)| *step).collect()
    }

    /// Output of an already-completed step, if it is still valid to reuse.
    #[must_use]
    pub fn reuse(&self, step: StepId) -> Option<&ToolOutput> {
        self.results
            .iter()
            .find(|(id, _)| *id == step)
            .map(|(_, output)| output)
    }

    /// Drops steps from `first` onward, for a resume where an input changed.
    ///
    /// Everything before it stays, which is what makes redoing work acceptable and losing it not.
    pub fn invalidate_from(&mut self, first: StepId) -> Vec<StepId> {
        let Some(at) = self.results.iter().position(|(id, _)| *id == first) else {
            return Vec::new();
        };
        let dropped = self.results.split_off(at);
        self.step = u32::try_from(self.results.len()).unwrap_or(u32::MAX);
        dropped.into_iter().map(|(id, _)| id).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(n: u64) -> ToolOutput {
        ToolOutput::new(serde_json::json!({ "n": n }))
    }

    fn filled() -> Checkpoint {
        let mut checkpoint = Checkpoint::new(TaskId::new(0));
        for n in 0..4 {
            checkpoint.record(StepId::new(n), output(n));
        }
        checkpoint
    }

    #[test]
    fn records_steps_in_order() {
        let checkpoint = filled();
        assert_eq!(checkpoint.step, 4);
        assert_eq!(checkpoint.steps().len(), 4);
    }

    #[test]
    fn completed_steps_can_be_reused() {
        let checkpoint = filled();
        assert_eq!(checkpoint.reuse(StepId::new(2)), Some(&output(2)));
        assert_eq!(checkpoint.reuse(StepId::new(9)), None);
    }

    #[test]
    fn invalidating_keeps_everything_before_the_change() {
        let mut checkpoint = filled();
        let dropped = checkpoint.invalidate_from(StepId::new(2));

        assert_eq!(dropped, vec![StepId::new(2), StepId::new(3)]);
        assert_eq!(checkpoint.steps(), vec![StepId::new(0), StepId::new(1)]);
        assert_eq!(checkpoint.step, 2);
        assert!(checkpoint.reuse(StepId::new(0)).is_some());
    }

    #[test]
    fn invalidating_an_unknown_step_changes_nothing() {
        let mut checkpoint = filled();
        assert!(checkpoint.invalidate_from(StepId::new(99)).is_empty());
        assert_eq!(checkpoint.steps().len(), 4);
    }

    #[test]
    fn scopes_open_and_close() {
        let mut checkpoint = Checkpoint::new(TaskId::new(0));
        checkpoint.open_scope(ScopeId::new(0));
        checkpoint.open_scope(ScopeId::new(1));
        checkpoint.close_scope(ScopeId::new(0));
        assert_eq!(checkpoint.open_scopes, vec![ScopeId::new(1)]);
    }
}

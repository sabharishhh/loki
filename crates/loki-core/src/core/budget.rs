//! The spend ceiling.
//!
//! Checked before a model call, never during one. Killing a task mid-flight to save money is
//! losing work, and losing work is the one thing interruption is not allowed to do.

use super::vocab::{BlockReason, Cents};

/// Fraction of the ceiling at which a warning fires. Early enough to act on, late enough not to
/// be noise.
const WARN_AT: u64 = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Proceed,
    Warn { spent: Cents, ceiling: Cents },
    Stop(BlockReason),
}

/// A monthly ceiling and what has been spent against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    spent: Cents,
    ceiling: Cents,
    warned: bool,
}

impl Budget {
    #[must_use]
    pub const fn new(ceiling: Cents) -> Self {
        Self {
            spent: Cents::ZERO,
            ceiling,
            warned: false,
        }
    }

    #[must_use]
    pub const fn spent(self) -> Cents {
        self.spent
    }

    #[must_use]
    pub const fn ceiling(self) -> Cents {
        self.ceiling
    }

    pub const fn record(&mut self, amount: Cents) {
        self.spent = self.spent.saturating_add(amount);
    }

    /// Whether the next model call may run.
    ///
    /// Warns once per crossing rather than on every call, so a long session does not repeat it.
    pub fn check(&mut self) -> Verdict {
        if self.spent.get() >= self.ceiling.get() {
            return Verdict::Stop(BlockReason::BudgetCeiling {
                spent: self.spent,
                ceiling: self.ceiling,
            });
        }

        let threshold = self.ceiling.get() / 100 * WARN_AT;
        if !self.warned && self.spent.get() >= threshold {
            self.warned = true;
            return Verdict::Warn {
                spent: self.spent,
                ceiling: self.ceiling,
            };
        }

        Verdict::Proceed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proceeds_while_under_the_warning_line() {
        let mut budget = Budget::new(Cents::new(1000));
        budget.record(Cents::new(500));
        assert_eq!(budget.check(), Verdict::Proceed);
    }

    #[test]
    fn warns_once_then_stays_quiet() {
        let mut budget = Budget::new(Cents::new(1000));
        budget.record(Cents::new(850));
        assert!(matches!(budget.check(), Verdict::Warn { .. }));
        assert_eq!(budget.check(), Verdict::Proceed);
    }

    #[test]
    fn stops_at_the_ceiling() {
        let mut budget = Budget::new(Cents::new(1000));
        budget.record(Cents::new(1000));
        assert!(matches!(
            budget.check(),
            Verdict::Stop(BlockReason::BudgetCeiling { .. })
        ));
    }

    #[test]
    fn stopping_outranks_warning() {
        let mut budget = Budget::new(Cents::new(1000));
        budget.record(Cents::new(2000));
        assert!(matches!(budget.check(), Verdict::Stop(_)));
    }
}

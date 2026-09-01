//! The spend ceiling.
//!
//! Checked before a model call, never during one. Killing a task mid-flight to save money is
//! losing work, and losing work is the one thing interruption is not allowed to do.

use super::vocab::{BlockReason, Cents, MICRO_CENTS_PER_CENT};

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
///
/// Spend accumulates in micro-cents. A single call costs a fraction of a cent, so rounding each
/// one to whole cents would record zero on a cheap model and the ceiling would never trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    spent_micros: u64,
    ceiling: Cents,
    warned: bool,
}

impl Budget {
    #[must_use]
    pub const fn new(ceiling: Cents) -> Self {
        Self {
            spent_micros: 0,
            ceiling,
            warned: false,
        }
    }

    /// A budget that already knows what this month cost.
    ///
    /// Without this the ceiling resets on every launch, which makes a monthly limit meaningless.
    #[must_use]
    pub const fn resuming(ceiling: Cents, spent_micros: u64) -> Self {
        Self {
            spent_micros,
            ceiling,
            // A restart mid-month should still warn once, so the flag starts clear.
            warned: false,
        }
    }

    /// Spend so far, rounded down for display.
    #[must_use]
    pub const fn spent(self) -> Cents {
        Cents::new(self.spent_micros / MICRO_CENTS_PER_CENT)
    }

    /// Spend so far, exact.
    #[must_use]
    pub const fn spent_micros(self) -> u64 {
        self.spent_micros
    }

    #[must_use]
    pub const fn ceiling(self) -> Cents {
        self.ceiling
    }

    pub const fn record_micros(&mut self, micros: u64) {
        self.spent_micros = self.spent_micros.saturating_add(micros);
    }

    /// Whether the next model call may run.
    ///
    /// Warns once per crossing rather than on every call, so a long session does not repeat it.
    pub fn check(&mut self) -> Verdict {
        let ceiling_micros = self.ceiling.get().saturating_mul(MICRO_CENTS_PER_CENT);

        if self.spent_micros >= ceiling_micros {
            return Verdict::Stop(BlockReason::BudgetCeiling {
                spent: self.spent(),
                ceiling: self.ceiling,
            });
        }

        if !self.warned && self.spent_micros >= ceiling_micros / 100 * WARN_AT {
            self.warned = true;
            return Verdict::Warn {
                spent: self.spent(),
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
        budget.record_micros(500 * MICRO_CENTS_PER_CENT);
        assert_eq!(budget.check(), Verdict::Proceed);
    }

    #[test]
    fn warns_once_then_stays_quiet() {
        let mut budget = Budget::new(Cents::new(1000));
        budget.record_micros(850 * MICRO_CENTS_PER_CENT);
        assert!(matches!(budget.check(), Verdict::Warn { .. }));
        assert_eq!(budget.check(), Verdict::Proceed);
    }

    #[test]
    fn stops_at_the_ceiling() {
        let mut budget = Budget::new(Cents::new(1000));
        budget.record_micros(1000 * MICRO_CENTS_PER_CENT);
        assert!(matches!(
            budget.check(),
            Verdict::Stop(BlockReason::BudgetCeiling { .. })
        ));
    }

    #[test]
    fn a_resumed_budget_starts_from_what_the_month_already_cost() {
        let mut budget = Budget::resuming(Cents::new(1000), 900 * MICRO_CENTS_PER_CENT);
        assert_eq!(budget.spent(), Cents::new(900));
        // Already past the warning line, so the first check warns rather than proceeding.
        assert!(matches!(budget.check(), Verdict::Warn { .. }));
    }

    #[test]
    fn a_resumed_budget_can_already_be_over() {
        let mut budget = Budget::resuming(Cents::new(1000), 1500 * MICRO_CENTS_PER_CENT);
        assert!(matches!(budget.check(), Verdict::Stop(_)));
    }

    #[test]
    fn many_sub_cent_calls_still_reach_the_ceiling() {
        // Ten thousand turns at half a cent each is 5000 cents. Rounding each to whole cents
        // would record zero and the ceiling would never trip.
        let mut budget = Budget::new(Cents::new(1000));
        for _ in 0..10_000 {
            budget.record_micros(500_000);
        }
        assert_eq!(budget.spent(), Cents::new(5000));
        assert!(matches!(budget.check(), Verdict::Stop(_)));
    }

    #[test]
    fn stopping_outranks_warning() {
        let mut budget = Budget::new(Cents::new(1000));
        budget.record_micros(2000 * MICRO_CENTS_PER_CENT);
        assert!(matches!(budget.check(), Verdict::Stop(_)));
    }
}

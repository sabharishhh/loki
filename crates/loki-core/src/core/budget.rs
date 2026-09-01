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

/// A monthly ceiling, an optional per-session one, and what has been spent against each.
///
/// Two ceilings because they answer different questions. The monthly one is the bill. The session
/// one is a runaway guard: an import or a long agentic run can spend a month's budget in an
/// afternoon, and noticing at the month boundary is too late.
///
/// Spend accumulates in micro-cents. A single call costs a fraction of a cent, so rounding each
/// one to whole cents would record zero on a cheap model and the ceiling would never trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    spent_micros: u64,
    ceiling: Cents,
    /// Spend since this process started, against `session_ceiling`.
    session_micros: u64,
    session_ceiling: Option<Cents>,
    warned: bool,
}

impl Budget {
    #[must_use]
    pub const fn new(ceiling: Cents) -> Self {
        Self {
            spent_micros: 0,
            ceiling,
            session_micros: 0,
            session_ceiling: None,
            warned: false,
        }
    }

    /// Adds a per-session cap on top of the monthly one.
    ///
    /// Optional, per section 20.2. Without it a single run can spend the whole month.
    #[must_use]
    pub const fn with_session_ceiling(mut self, ceiling: Cents) -> Self {
        self.session_ceiling = Some(ceiling);
        self
    }

    /// A budget that already knows what this month cost.
    ///
    /// Without this the ceiling resets on every launch, which makes a monthly limit meaningless.
    #[must_use]
    pub const fn resuming(ceiling: Cents, spent_micros: u64) -> Self {
        Self {
            spent_micros,
            ceiling,
            // A new process is a new session, so session spend restarts at zero.
            session_micros: 0,
            session_ceiling: None,
            // A restart mid-month should still warn once, so the flag starts clear.
            warned: false,
        }
    }

    /// Spend this session so far.
    #[must_use]
    pub const fn spent_this_session(self) -> Cents {
        Cents::new(self.session_micros / MICRO_CENTS_PER_CENT)
    }

    #[must_use]
    pub const fn session_ceiling(self) -> Option<Cents> {
        self.session_ceiling
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
        self.session_micros = self.session_micros.saturating_add(micros);
    }

    /// Whether the next model call may run.
    ///
    /// Warns once per crossing rather than on every call, so a long session does not repeat it.
    pub fn check(&mut self) -> Verdict {
        // The session cap is checked first. It is the tighter of the two by definition, and a
        // runaway run should be named as such rather than reported as the month running out.
        if let Some(session) = self.session_ceiling {
            let session_micros = session.get().saturating_mul(MICRO_CENTS_PER_CENT);
            if self.session_micros >= session_micros {
                return Verdict::Stop(BlockReason::SessionCeiling {
                    spent: self.spent_this_session(),
                    ceiling: session,
                });
            }
        }

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
    fn a_session_ceiling_stops_a_runaway_before_the_month_does() {
        // A month's worth of headroom, but one session may only spend a tenth of it.
        let mut budget = Budget::new(Cents::new(10_000)).with_session_ceiling(Cents::new(1000));
        budget.record_micros(1000 * MICRO_CENTS_PER_CENT);

        match budget.check() {
            Verdict::Stop(BlockReason::SessionCeiling { ceiling, .. }) => {
                assert_eq!(ceiling, Cents::new(1000));
            }
            other => panic!("expected a session stop, got {other:?}"),
        }
    }

    #[test]
    fn the_session_cap_is_reported_as_itself_not_as_the_month() {
        let mut budget = Budget::new(Cents::new(10_000)).with_session_ceiling(Cents::new(100));
        budget.record_micros(200 * MICRO_CENTS_PER_CENT);
        // The month is nowhere near spent, so calling this a monthly stop would mislead.
        assert!(budget.spent() < budget.ceiling());
        assert!(matches!(
            budget.check(),
            Verdict::Stop(BlockReason::SessionCeiling { .. })
        ));
    }

    #[test]
    fn without_a_session_ceiling_only_the_month_applies() {
        let mut budget = Budget::new(Cents::new(1000));
        budget.record_micros(5000 * MICRO_CENTS_PER_CENT);
        assert!(matches!(
            budget.check(),
            Verdict::Stop(BlockReason::BudgetCeiling { .. })
        ));
    }

    #[test]
    fn a_resumed_budget_starts_a_fresh_session() {
        // The month carries over. The session does not.
        let budget = Budget::resuming(Cents::new(10_000), 9000 * MICRO_CENTS_PER_CENT);
        assert_eq!(budget.spent(), Cents::new(9000));
        assert_eq!(budget.spent_this_session(), Cents::ZERO);
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

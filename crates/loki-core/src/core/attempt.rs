//! A bounded loop that stops for a reason it can name (§10.5).
//!
//! Narrow, look, refine. Three subsystems run that shape: lane 2 searching memory (§10.8), §12.7's
//! search to completion, and §13's tool loop. This is the shape itself, so the three are callers
//! rather than copies.
//!
//! **A budget bounds a runaway. It does not notice a loop going nowhere.** Eight steps spent
//! re-running one grep cost the same as eight spent narrowing, and only one of those is work. So
//! every step carries a progress signature and two identical signatures in a row end the attempt
//! early, with the reason recorded rather than the budget quietly draining.
//!
//! **Two limits, because they catch different failures.** A step budget catches a loop that is
//! working and getting nowhere. A wall clock catches one step that hangs, which §12.6 needs
//! because a page fetch can take seconds and a loop bounded only on steps still spends a minute
//! before giving up.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use async_trait::async_trait;

use crate::ports::clock::Clock;

/// Lines one observation may carry back.
///
/// A grep over a store with a busy word in it returns hundreds. Unbounded, eight of those is the
/// context window, which is the failure §10.5 avoids by taking only the slice.
pub const OBSERVATION_LINES: usize = 40;

/// Why an attempt ended.
///
/// Five, not a boolean. A stall reported as an exhausted budget is a lie of the kind §10.8
/// forbids, and so is either reported as having found nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Ending {
    /// The caller stopped on its own, which is the ordinary end.
    #[default]
    Stopped,
    /// Every step was spent.
    OutOfBudget,
    /// The wall clock ran out first.
    OutOfTime,
    /// Two identical progress signatures in a row.
    Stalled,
    /// It could not run at all, so nothing was attempted. Constructed by the caller, never here:
    /// this module only reports on loops that actually ran.
    Failed,
}

/// What one attempt may spend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub steps: usize,
    pub wall: Duration,
}

impl Budget {
    /// A budget in steps with a wall clock that will not realistically fire.
    ///
    /// For a caller whose steps are local and fast, where the step count is the real bound. Lane 2
    /// is that caller: a grep over a personal store does not hang.
    #[must_use]
    pub const fn of_steps(steps: usize) -> Self {
        Self {
            steps,
            wall: Duration::from_secs(3600),
        }
    }

    #[must_use]
    pub const fn within(mut self, wall: Duration) -> Self {
        self.wall = wall;
        self
    }
}

/// What an attempt produced, and why it stopped.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Outcome {
    /// What actually answered, dead ends dropped.
    pub found: Vec<String>,
    /// How much of the budget went.
    pub steps: usize,
    pub ending: Ending,
}

/// What a caller supplies: how to choose a step, describe it, and perform it.
///
/// Generic over the step rather than over a fixed operation type, because lane 2's steps are file
/// reads, §12.7's are fetches and §13's are tool calls, and only the caller knows how to name one.
#[async_trait]
pub trait Steps: Send + Sync {
    /// One unit of work. Opaque here.
    type Step: Send + Sync;
    /// Whatever the caller's operations fail with.
    type Error: Send + Sync;

    /// The next step, given every step so far and what each returned, or `None` to stop.
    ///
    /// # Errors
    /// Fails if the choice itself could not be made. That ends the attempt rather than being
    /// recorded as a dead end, because a caller that cannot choose cannot continue.
    async fn next(&self, seen: &[String]) -> Result<Option<Self::Step>, Self::Error>;

    /// A one-line record of a step. Goes into `seen` and into the progress signature.
    fn describe(&self, step: &Self::Step) -> String;

    /// Performs one step.
    ///
    /// # Errors
    /// A failure here is an ordinary dead end, not the end of the attempt: a path that is not
    /// there is a normal move in narrow, look, refine.
    async fn run(&self, step: &Self::Step) -> Result<String, Self::Error>;
}

/// Runs a bounded attempt.
///
/// Never fails on a step failing. Only a caller that cannot choose its next step ends the attempt
/// early, because everything else is a dead end worth telling the caller about and continuing from.
///
/// # Errors
/// Fails only if [`Steps::next`] does.
pub async fn run<S>(steps: &S, budget: Budget, clock: &dyn Clock) -> Result<Outcome, S::Error>
where
    S: Steps + ?Sized,
{
    // What the caller sees, dead ends included, and what actually answered. Separate on purpose: a
    // caller narrowing its search needs to know a path was empty, and the turn does not.
    let mut seen: Vec<String> = Vec::new();
    let mut found: Vec<String> = Vec::new();
    let mut used = 0usize;
    let mut previous: Option<u64> = None;
    let mut ending = Ending::Stopped;
    let started = clock.now();

    while used < budget.steps {
        // Checked before the step rather than after, so a budget that has already run out cannot
        // start one more piece of work it has no time to finish.
        if elapsed(clock, started) >= budget.wall {
            ending = Ending::OutOfTime;
            break;
        }
        let Some(step) = steps.next(&seen).await? else {
            break;
        };
        used += 1;
        // The step is recorded beside its result, not just the result. A caller that cannot see
        // what it already ran repeats it, and repeating a step costs one of the budget.
        let line = steps.describe(&step);
        let outcome = steps.run(&step).await;
        let now = signature(&line, &outcome);
        match outcome {
            Ok(output) if output.trim().is_empty() => seen.push(format!("{line}\nnothing there")),
            Ok(output) => {
                let output = clip(&output);
                seen.push(format!("{line}\n{output}"));
                found.push(output);
            }
            Err(_) => seen.push(format!("{line}\nthat did not work")),
        }
        if previous == Some(now) {
            ending = Ending::Stalled;
            break;
        }
        previous = Some(now);
    }

    // A stall or a timeout is the more specific reason, so a loop that repeats itself on the last
    // step of the budget is reported as stuck rather than as thorough. A budget of zero lands here
    // too, and should: it had none rather than having looked and chosen to stop.
    if ending == Ending::Stopped && used >= budget.steps && found.is_empty() {
        ending = Ending::OutOfBudget;
    }
    Ok(Outcome {
        found,
        steps: used,
        ending,
    })
}

/// How long the attempt has been running.
///
/// Through [`Clock`] rather than [`std::time::Instant`] so §21.2's corpus and this module's own
/// tests can move the clock instead of sleeping. A sleep in a test is a flake waiting to happen.
fn elapsed(clock: &dyn Clock, started: jiff::Timestamp) -> Duration {
    let micros = clock.now().as_microsecond() - started.as_microsecond();
    u64::try_from(micros).map_or(Duration::ZERO, Duration::from_micros)
}

/// One step's progress signature: the operation, how it ended, and how much it produced (§10.5).
///
/// **Over the raw byte count, never the rendered slice.** [`clip`] truncates, so a step producing
/// steadily more output looks stationary through the slice, and a genuinely stuck loop looks alive
/// the moment a byte of formatting changes. An error carries no output: the same operation failing
/// twice is no progress however the message is worded.
pub(crate) fn signature<E>(step: &str, outcome: &Result<String, E>) -> u64 {
    let (status, bytes) = match outcome {
        Ok(output) if output.trim().is_empty() => (0u8, 0),
        Ok(output) => (1u8, output.len()),
        Err(_) => (2u8, 0),
    };
    let mut hasher = DefaultHasher::new();
    step.hash(&mut hasher);
    status.hash(&mut hasher);
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Takes the slice of an observation that may enter a prompt.
#[must_use]
pub fn clip(output: &str) -> String {
    let mut lines: Vec<&str> = output.lines().take(OBSERVATION_LINES).collect();
    let over = output.lines().count().saturating_sub(OBSERVATION_LINES);
    if over > 0 {
        lines.push("(more lines, not shown. narrow the search)");
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    /// A clock that moves on every read.
    ///
    /// Local rather than the `FakeClock` adapter, because Ring 0 may not import Ring 2 and a
    /// `#[cfg(test)]` block is still Ring 0. A `sleep` here would be a flake waiting to happen.
    struct Ticking {
        micros: AtomicI64,
        step: i64,
    }

    impl Ticking {
        fn every(step: Duration) -> Self {
            Self {
                micros: AtomicI64::new(0),
                step: i64::try_from(step.as_micros()).expect("small step"),
            }
        }
    }

    impl Clock for Ticking {
        fn now(&self) -> jiff::Timestamp {
            let at = self.micros.fetch_add(self.step, Ordering::Relaxed);
            jiff::Timestamp::from_microsecond(at).expect("in range")
        }

        fn zone(&self) -> jiff::tz::TimeZone {
            jiff::tz::TimeZone::UTC
        }
    }

    /// A stopped clock, for the cases that are not about time.
    struct Stopped;

    impl Clock for Stopped {
        fn now(&self) -> jiff::Timestamp {
            jiff::Timestamp::from_microsecond(0).expect("epoch")
        }

        fn zone(&self) -> jiff::tz::TimeZone {
            jiff::tz::TimeZone::UTC
        }
    }

    /// Answers with the same text every time, so the caller decides how many steps happen.
    struct Fixed {
        limit: usize,
        taken: AtomicUsize,
        answer: Result<String, &'static str>,
    }

    impl Fixed {
        fn answering(limit: usize, text: &str) -> Self {
            Self {
                limit,
                taken: AtomicUsize::new(0),
                answer: Ok(text.to_owned()),
            }
        }
    }

    #[async_trait]
    impl Steps for Fixed {
        type Step = usize;
        type Error = &'static str;

        async fn next(&self, _seen: &[String]) -> Result<Option<usize>, &'static str> {
            let at = self.taken.fetch_add(1, Ordering::Relaxed);
            Ok((at < self.limit).then_some(at))
        }

        fn describe(&self, step: &usize) -> String {
            // Distinct per step, so nothing here is mistaken for a stall.
            format!("STEP {step}")
        }

        async fn run(&self, _step: &usize) -> Result<String, &'static str> {
            self.answer.clone()
        }
    }

    /// S1's new limit. A loop bounded only on steps still spends a minute on one slow fetch.
    #[tokio::test]
    async fn a_slow_attempt_ends_on_the_wall_clock_and_keeps_what_it_found() {
        let steps = Fixed::answering(100, "something");
        // Each read moves a second, and the budget is three, so a couple of steps land first.
        let clock = Ticking::every(Duration::from_secs(1));
        let budget = Budget::of_steps(100).within(Duration::from_secs(3));

        let out = run(&steps, budget, &clock).await.expect("no choice failed");

        assert_eq!(out.ending, Ending::OutOfTime, "{out:?}");
        assert!(
            out.steps < 100,
            "it must stop well short of the step budget"
        );
        assert!(
            !out.found.is_empty(),
            "what it found before the clock ran out is still an answer: {out:?}"
        );
    }

    /// The empty case, which reaches a caller once budgets are shared across subsystems.
    ///
    /// It is reported as out of budget rather than as stopped, and the distinction is the point:
    /// `Stopped` means the caller looked and chose to stop, which would be a claim about a search
    /// that never happened. §10.8's rule, applied to a budget of nothing.
    #[tokio::test]
    async fn a_budget_of_no_steps_runs_nothing_and_says_it_had_none() {
        let steps = Fixed::answering(100, "something");

        let out = run(&steps, Budget::of_steps(0), &Stopped)
            .await
            .expect("no choice failed");

        assert_eq!(out.steps, 0);
        assert!(out.found.is_empty());
        assert_eq!(
            out.ending,
            Ending::OutOfBudget,
            "a budget of nothing is exhausted from the start, not a search that chose to stop: \
             {out:?}"
        );
    }

    /// A caller that cannot choose is not a caller that found nothing, and the two must not be
    /// reported as the same thing. Lane 2 has no case for this: its navigator always answers.
    #[tokio::test]
    async fn a_caller_that_cannot_choose_a_step_fails_rather_than_finding_nothing() {
        struct Mute;

        #[async_trait]
        impl Steps for Mute {
            type Step = ();
            type Error = &'static str;

            async fn next(&self, _seen: &[String]) -> Result<Option<()>, &'static str> {
                Err("the navigator could not be reached")
            }

            fn describe(&self, (): &()) -> String {
                String::new()
            }

            async fn run(&self, (): &()) -> Result<String, &'static str> {
                Ok(String::new())
            }
        }

        let out = run(&Mute, Budget::of_steps(8), &Stopped).await;

        assert!(
            out.is_err(),
            "a choice that could not be made ends the attempt, it is not an empty result"
        );
    }

    /// A step failing is ordinary. A path that is not there is a normal move in narrow, look,
    /// refine, and the loop has to carry on rather than treat it as the end.
    #[tokio::test]
    async fn a_step_that_fails_is_a_dead_end_and_the_attempt_carries_on() {
        struct Flaky {
            taken: AtomicUsize,
        }

        #[async_trait]
        impl Steps for Flaky {
            type Step = usize;
            type Error = &'static str;

            async fn next(&self, _seen: &[String]) -> Result<Option<usize>, &'static str> {
                let at = self.taken.fetch_add(1, Ordering::Relaxed);
                Ok((at < 3).then_some(at))
            }

            fn describe(&self, step: &usize) -> String {
                format!("STEP {step}")
            }

            async fn run(&self, step: &usize) -> Result<String, &'static str> {
                if *step == 0 {
                    return Err("that path is not there");
                }
                Ok(format!("found on step {step}"))
            }
        }

        let out = run(
            &Flaky {
                taken: AtomicUsize::new(0),
            },
            Budget::of_steps(8),
            &Stopped,
        )
        .await
        .expect("a failing step is not a failing attempt");

        assert_eq!(
            out.steps, 3,
            "the dead end cost a step and did not end the run"
        );
        assert_eq!(
            out.found.len(),
            2,
            "only what answered is carried, never the dead end: {out:?}"
        );
    }
}

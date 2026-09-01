//! The loop's handle on memory.
//!
//! One object over the bundle, the index and the session in progress, so the core loop wires to a
//! single thing rather than to four. Everything here is Ring 0: §6.2 lists the gate and the store
//! among the locked internals, so this is not a port and there is no adapter behind it.

use std::sync::Mutex;

use jiff::civil::Date;

use super::bundle::{Bundle, BundleError};
use super::consolidate::{self, Budget, ConsolidateError, Episode, Extractor, Report};
use super::gate::TierScope;
use super::index::{Index, IndexError, Origin, Query, Recalled, Session, Use};
use super::reconcile::Reference;
use super::resolve::Matcher;
use super::working_set::{self, WorkingSetError};

/// Claims a single turn may carry. Precision over recall (§10.1): a wrong memory costs more than
/// a missing one, because a missing memory reads as forgetfulness and a wrong one as not knowing
/// you at all.
pub const RECALL_CAP: usize = 5;

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("could not read or write the bundle: {0}")]
    Bundle(#[from] BundleError),
    #[error("could not use the index: {0}")]
    Index(#[from] IndexError),
    #[error("could not regenerate the working set: {0}")]
    WorkingSet(#[from] WorkingSetError),
    #[error("could not consolidate: {0}")]
    Consolidate(#[from] ConsolidateError),
}

/// Memory, as the loop sees it.
pub struct Memory {
    bundle: Bundle,
    index: Index,
    session: String,
    episode: String,
    scope: TierScope,
    /// Turns recorded so far. The next ordinal, and the window boundary, are read off this.
    turns: Mutex<u32>,
}

impl Memory {
    /// Opens memory for one session.
    ///
    /// # Errors
    /// Fails if the bundle or the index cannot be opened.
    pub async fn open(
        root: &std::path::Path,
        index: Index,
        session: impl Into<String>,
        today: Date,
        scope: TierScope,
    ) -> Result<Self, MemoryError> {
        let bundle = Bundle::open(root).await?;
        {
            let reader = bundle.reader().await;
            index.sync(&reader)?;
        }
        Ok(Self {
            bundle,
            index,
            session: session.into(),
            episode: format!("episodes/{today}.md"),
            scope,
            turns: Mutex::new(0),
        })
    }

    #[must_use]
    pub fn bundle(&self) -> &Bundle {
        &self.bundle
    }

    #[must_use]
    pub const fn index(&self) -> &Index {
        &self.index
    }

    /// The frozen prefix's memory block (§8.1). Read once per session, not per turn.
    ///
    /// # Errors
    /// Fails if the bundle cannot be read.
    pub async fn working_set(&self) -> Result<String, MemoryError> {
        Ok(working_set::read(&self.bundle).await?)
    }

    /// Records a turn, both to the episode file and to the live corpus.
    ///
    /// The episode is appended as the session runs rather than written at close (D-045), because
    /// anything else leaves the current session unsearchable while it is happening.
    ///
    /// # Errors
    /// Fails if the episode cannot be appended to or the index cannot be written.
    pub async fn record(&self, speaker: &str, text: &str) -> Result<(), MemoryError> {
        let ordinal = {
            let mut turns = self.turns.lock().unwrap_or_else(|e| e.into_inner());
            *turns += 1;
            *turns
        };
        {
            let writer = self.bundle.writer().await;
            writer.append(&self.episode, &format!("\n**{speaker}**: {text}\n"))?;
        }
        self.index
            .record_turn(&self.session, ordinal, speaker, text)?;
        Ok(())
    }

    /// Pre-fetch for one message (§10.1).
    ///
    /// Runs before the model call, not as a tool call after it, because a round trip on every turn
    /// where memory matters is most of where the sense of already knowing you goes.
    ///
    /// `window_keeps` is how many recent turns are still in the prompt. Those are never retrieved:
    /// they are already there, and re-sending them is waste that breaks the §8.1 cache.
    ///
    /// # Errors
    /// Fails if the index cannot be read.
    pub fn recall(
        &self,
        message: &str,
        window_keeps: u32,
        today: Date,
    ) -> Result<Vec<Recalled>, MemoryError> {
        let recorded = *self.turns.lock().unwrap_or_else(|e| e.into_inner());
        let query = Query::prefetch(message, self.scope, today, RECALL_CAP).spanning(Session {
            id: &self.session,
            window_starts_at: recorded.saturating_sub(window_keeps),
        });
        Ok(self.index.recall(&query)?)
    }

    /// Records that recalled claims were used, so confidence can move with use (§9.9).
    ///
    /// Only claims. A session turn has no file to write a count against, and counting it would
    /// inflate the very corpus that is about to be consolidated away.
    ///
    /// # Errors
    /// Fails if the index cannot be written.
    pub fn mark_used(&self, recalled: &[Recalled]) -> Result<(), MemoryError> {
        let uses: Vec<Use> = recalled
            .iter()
            .filter(|r| r.origin == Origin::Claim)
            .map(Recalled::reference)
            .collect();
        if uses.is_empty() {
            return Ok(());
        }
        Ok(self.index.record_use(&uses)?)
    }

    /// Closes the session: consolidate this episode, regenerate the working set, forget the raw
    /// turns (§9.8, D-045).
    ///
    /// Runs on the Utility role because the app is already awake, and every session, so the cost
    /// compounds rather than arriving as one large bill.
    ///
    /// # Errors
    /// Fails if consolidation, the working set, or the index fails.
    pub async fn close(
        &self,
        extractor: &dyn Extractor,
        matcher: &dyn Matcher,
        budget: &dyn Budget,
        today: Date,
    ) -> Result<Report, MemoryError> {
        let episodes = [Episode {
            path: self.episode.clone(),
            reference: Reference::Live(today),
        }];
        let report = consolidate::run(
            &episodes,
            &self.bundle,
            &self.index,
            extractor,
            matcher,
            budget,
            today,
        )
        .await?;

        working_set::generate(&self.bundle, &self.index, self.scope, today).await?;
        {
            let writer = self.bundle.writer().await;
            writer.commit("Regenerate the working set")?;
        }
        // Only once the claims exist. Dropping the turns first would lose the session if
        // consolidation failed partway.
        if report.remaining.is_empty() {
            self.index.forget_session(&self.session)?;
        }
        Ok(report)
    }
}

/// Renders recalled lines for the turn zone.
///
/// Plain text rather than a structure, because it is going into a prompt. The origin is not shown:
/// the model has no use for whether a fact came from a claim or from earlier in this conversation,
/// and neither does the user.
#[must_use]
pub fn render(recalled: &[Recalled]) -> String {
    let mut out = String::new();
    for line in recalled {
        out.push_str("- ");
        out.push_str(&line.text);
        out.push('\n');
    }
    out
}

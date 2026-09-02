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
use super::index::{Index, IndexError, Layer, Query, Recalled, Session, Use};
use super::reconcile::Reference;
use super::resolve::Matcher;
use super::timeline;
use super::working_set::{self, WorkingSetError};
use crate::core::temporal;

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
    /// Turns recorded when the last close ran. Closing twice with nothing said between is a
    /// no-op, so putting the window away does not cost a model call each time.
    consolidated_at: Mutex<u32>,
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
            consolidated_at: Mutex::new(0),
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
            .filter(|r| r.layer == Layer::Consolidated)
            .map(Recalled::reference)
            .collect();
        if uses.is_empty() {
            return Ok(());
        }
        Ok(self.index.record_use(&uses)?)
    }

    /// Marks a claim wrong, from the rail's one-click `not true` (§9.9).
    ///
    /// Confidence collapses and the claim is flagged. Nothing is deleted by a tap: §9.10 says
    /// nothing is removed by heuristic, and a misfired click must be recoverable.
    ///
    /// # Errors
    /// Fails if the concept cannot be read or written.
    pub async fn contradict(&self, path: &str, ordinal: u32) -> Result<(), MemoryError> {
        let mut concept = {
            let reader = self.bundle.reader().await;
            reader.load_concept(path)?
        };
        for (at, claim) in concept.claims_mut().enumerate() {
            if u32::try_from(at).is_ok_and(|n| n == ordinal) {
                claim.contradicted();
            }
        }
        {
            let writer = self.bundle.writer().await;
            writer.save_concept(path, &concept)?;
            writer.commit(&format!("Marked wrong: {path}"))?;
        }
        {
            let reader = self.bundle.reader().await;
            self.index.sync(&reader)?;
        }
        Ok(())
    }

    /// The timeline, newest first (§17.3).
    ///
    /// Reads `log.md`, which is the record. Rendering from anywhere else would let the screen and
    /// the file disagree, and being able to check the work is the whole point of the surface.
    ///
    /// # Errors
    /// Fails if the bundle cannot be read.
    pub async fn timeline(&self, limit: usize) -> Result<Vec<String>, MemoryError> {
        let reader = self.bundle.reader().await;
        let text = reader.read(super::bundle::LOG).unwrap_or_default();
        let mut rows: Vec<String> = text
            .lines()
            .filter_map(|line| line.trim().strip_prefix("- "))
            .map(str::to_owned)
            .collect();
        rows.reverse();
        rows.truncate(limit);
        Ok(rows)
    }

    /// The last local day the user said anything, or `None` on a first run.
    ///
    /// Read at session start, before this session records anything, or today's own episode would
    /// answer the question. Episodes are dated files, so the newest one is where the user last
    /// spoke and no second record has to be kept in step.
    ///
    /// # Errors
    /// Fails if the bundle cannot be read.
    pub async fn last_spoke_on(&self) -> Result<Option<Date>, MemoryError> {
        let reader = self.bundle.reader().await;
        Ok(reader
            .ls("episodes")
            .unwrap_or_default()
            .into_iter()
            .filter_map(|name| name.strip_suffix(".md")?.parse::<Date>().ok())
            .max())
    }

    /// Past sessions, newest first, for the sidebar.
    ///
    /// Read off `episodes/`, because that is where a session actually is. A separate list would be
    /// a second record to keep in step.
    ///
    /// # Errors
    /// Fails if the bundle cannot be read.
    pub async fn sessions(&self, limit: usize) -> Result<Vec<String>, MemoryError> {
        let reader = self.bundle.reader().await;
        let mut days: Vec<String> = reader
            .ls("episodes")
            .unwrap_or_default()
            .into_iter()
            .filter_map(|name| name.strip_suffix(".md").map(str::to_owned))
            .collect();
        days.sort_unstable();
        days.reverse();
        days.truncate(limit);
        Ok(days)
    }

    /// Loads the concepts a run touched, for the timeline's correction pairs.
    async fn load_touched(&self, report: &Report) -> Vec<(String, super::concept::RawConcept)> {
        let mut paths: Vec<String> = report.decisions.iter().map(|d| d.concept.clone()).collect();
        paths.extend(report.promoted.iter().cloned());
        paths.extend(report.created.iter().cloned());
        paths.sort_unstable();
        paths.dedup();

        let mut out = Vec::with_capacity(paths.len());
        let reader = self.bundle.reader().await;
        for path in paths {
            if let Ok(concept) = reader.load_concept(&path) {
                out.push((path, concept));
            }
        }
        out
    }

    /// The rows this run added to the timeline, for the session summary (§17.4).
    ///
    /// # Errors
    /// Never fails; the signature matches its siblings.
    pub async fn timeline_rows(&self, report: &Report, today: Date) -> Vec<timeline::Entry> {
        let concepts = self.load_touched(report).await;
        timeline::rows(report, &concepts, today)
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
        // Nothing said since the last close means nothing to consolidate.
        {
            let turns = *self.turns.lock().unwrap_or_else(|e| e.into_inner());
            let done = *self
                .consolidated_at
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if turns == done {
                return Ok(Report::default());
            }
        }

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

        // The timeline is written before the working set, so a crash between the two leaves the
        // user able to see what changed rather than only its effect.
        let concepts = self.load_touched(&report).await;
        let rows = timeline::rows(&report, &concepts, today);
        {
            timeline::append(&self.bundle, &rows, today).await?;
        }

        working_set::generate(&self.bundle, &self.index, self.scope, today).await?;
        {
            let writer = self.bundle.writer().await;
            writer.commit("Regenerate the working set")?;
        }
        // Only once the claims exist. Dropping the turns first would lose the session if
        // consolidation failed partway.
        if report.remaining.is_empty() {
            // The turns stay in the live corpus, because the session may continue: closing the
            // window is a session boundary, not the end of the conversation. They are dropped
            // when the process ends.
            let turns = *self.turns.lock().unwrap_or_else(|e| e.into_inner());
            *self
                .consolidated_at
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = turns;
        }
        Ok(report)
    }
}

/// Renders recalled lines for the turn zone.
///
/// Plain text rather than a structure, because it is going into a prompt. The layer is not shown:
/// the model has no use for whether a fact came from a claim or from earlier in this conversation,
/// and neither does the user.
///
/// A claim with a world time carries both the instant and the distance (§10.9). The instant is
/// what makes it checkable against the file; the distance is what the model would otherwise
/// compute, and §9.14 is the evidence that it computes it wrong.
#[must_use]
pub fn render(recalled: &[Recalled], today: Date) -> String {
    let mut out = String::new();
    for line in recalled {
        out.push_str("- ");
        out.push_str(&line.text);
        if let Some(from) = line.valid_from {
            out.push_str("  ");
            out.push_str(&temporal::since(from, today));
        }
        out.push('\n');
    }
    out
}

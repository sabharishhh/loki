//! The loop's handle on memory.
//!
//! One object over the bundle, the index and the session in progress, so the core loop wires to a
//! single thing rather than to four. Everything here is Ring 0: §6.2 lists the gate and the store
//! among the locked internals, so this is not a port and there is no adapter behind it.

use std::sync::Mutex;

use jiff::civil::Date;

use super::bundle::{self, Bundle, BundleError};
use super::claim::{Claim, Confidence, Origin};
use super::concept::{Attribution, Label, RawConcept, Status};
use super::consolidate::{self, Budget, ConsolidateError, Episode, Extractor, Report};
use super::gate::TierScope;
use super::index::{Index, IndexError, Lane, Layer, Query, Recalled, Session, Use};
use super::knowledge;
use super::reconcile::Reference;
use super::resolve::Matcher;
use super::runtime;
use super::timeline;
use super::working_set::{self, WorkingSetError};
use crate::core::temporal;

/// Marks a line in the buffer as something Loki said back, not something the user said (§9.8).
///
/// Without it a claim injected by pre-fetch is read on the next pass as a fresh statement, so a
/// fact recalled a hundred times becomes a hundred claims phrased a hundred ways. The event that
/// says which claims were injected is already emitted and the buffer already records the turn, so
/// this reads a log we already write.
pub const RECALLED: &str = "**recalled**:";

/// How long a recall row counts towards promotion (§10.6, §26 question 17).
///
/// A claim heavily used last year and untouched since should not still be promoting things. Ninety
/// days is long enough that a fact used monthly still accumulates and short enough that last
/// year's habits stop voting. Open question 17, so it is one named number to change.
pub const RECALL_WINDOW_DAYS: i64 = 90;

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
    #[error("{path} has no claim {ordinal}")]
    NoSuchClaim { path: String, ordinal: u32 },
    #[error("cannot merge: {why}")]
    CannotMerge { why: String },
}

/// The heading a claim sits under, so an edit lands in the same section as what it replaced.
fn section_of(concept: &RawConcept, ordinal: u32) -> Option<String> {
    let mut seen = 0u32;
    for section in &concept.sections {
        let len = u32::try_from(section.claims.len()).unwrap_or(u32::MAX);
        if ordinal < seen.saturating_add(len) {
            return Some(section.heading.clone());
        }
        seen = seen.saturating_add(len);
    }
    None
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
        seed_singletons(&bundle, today).await?;
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

    /// Writes the index's recall counts into the concept files (§9.13, §10.6).
    ///
    /// The rows in `index.sqlite` are disposable working data. These counts are the record, and
    /// they are what §9.8 promotes an inferred claim on, so losing them to a rebuild would lose
    /// the promotion signal with them.
    async fn fold_recalls(&self) -> Result<(), MemoryError> {
        let pending = self.index.drain_pending_uses()?;
        if pending.is_empty() {
            return Ok(());
        }

        let mut by_path: std::collections::BTreeMap<String, Vec<&super::index::PendingUse>> =
            std::collections::BTreeMap::new();
        for entry in &pending {
            by_path.entry(entry.path.clone()).or_default().push(entry);
        }

        for (path, entries) in by_path {
            let mut concept = {
                let reader = self.bundle.reader().await;
                // A file that will not parse is not a reason to lose the whole fold.
                match reader.load_concept(&path) {
                    Ok(concept) => concept,
                    Err(_) => continue,
                }
            };
            for (at, claim) in concept.claims_mut().enumerate() {
                let Ok(ordinal) = u32::try_from(at) else {
                    continue;
                };
                let Some(entry) = entries.iter().find(|e| e.ordinal == ordinal) else {
                    continue;
                };
                for _ in 0..entry.uses {
                    claim.used_without_correction();
                }
                claim.recalls = entry.recalls;
                claim.recall_queries = entry.recall_queries;
                claim.recall_days = entry.recall_days;
            }
            let writer = self.bundle.writer().await;
            writer.save_concept(&path, &concept)?;
        }
        Ok(())
    }

    /// Whether anything is waiting to be consolidated (§18.2).
    ///
    /// True after a session that ended without a close, which is what makes the next launch able
    /// to pick it up rather than orphaning the turns.
    pub async fn has_unconsolidated(&self) -> bool {
        let reader = self.bundle.reader().await;
        reader
            .read(bundle::CURRENT)
            .is_ok_and(|text| !text.trim().is_empty())
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
        let line = format!("\n**{speaker}**: {text}\n");
        {
            let writer = self.bundle.writer().await;
            // Two writes, two jobs. The episode is the permanent dated record §11.3 imports from
            // and lane 2 reaches. The buffer is what has not been consolidated yet, and it is what
            // consolidation reads, so a second close in one session does not re-extract the first.
            writer.append(&self.episode, &line)?;
            writer.append(bundle::CURRENT, &line)?;
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
    pub async fn note_recalled(&self, recalled: &[Recalled]) -> Result<(), MemoryError> {
        let lines: Vec<String> = recalled
            .iter()
            .filter(|r| r.layer == Layer::Consolidated)
            .map(|r| format!("{RECALLED} {}\n", r.text))
            .collect();
        if lines.is_empty() {
            return Ok(());
        }
        let writer = self.bundle.writer().await;
        Ok(writer.append(bundle::CURRENT, &lines.concat())?)
    }

    /// Lane 2: the agent searching memory directly, when lane 1 was not enough (§10.8).
    ///
    /// The caller decides whether to call this, using [`runtime::should_escalate`] on the absolute
    /// score lane 1 already returned. Nothing here decides, because a model deciding whether to
    /// search is a step it will sometimes skip.
    ///
    /// # Errors
    /// Fails if the bundle or index cannot be reached.
    pub async fn search_deeply(
        &self,
        question: &str,
        navigator: &dyn runtime::Navigator,
        today: Date,
    ) -> Result<runtime::Found, runtime::RuntimeError> {
        let rt = runtime::Runtime::new(&self.bundle, &self.index, self.scope);
        runtime::search(question, &rt, navigator, today).await
    }

    /// The memory runtime, for a caller that wants one primitive rather than a whole search.
    #[must_use]
    pub const fn runtime(&self) -> runtime::Runtime<'_> {
        runtime::Runtime::new(&self.bundle, &self.index, self.scope)
    }

    /// Records what retrieval returned, for §10.6's three counted signals.
    ///
    /// Separate from `mark_used`, which is §9.9's confidence meter. A claim can be recalled without
    /// being used well, and the two questions want different answers.
    ///
    /// # Errors
    /// Fails if the index cannot be written.
    pub fn note_recall(
        &self,
        recalled: &[Recalled],
        query: &str,
        today: Date,
        lane: Lane,
    ) -> Result<(), MemoryError> {
        Ok(self.index.record_recall(recalled, query, today, lane)?)
    }

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

    /// What Loki knows, grouped by entity, for §17.3's screen.
    ///
    /// # Errors
    /// Fails if the bundle cannot be read.
    pub async fn knowledge(&self, today: Date) -> Result<knowledge::Knowledge, MemoryError> {
        Ok(knowledge::read(&self.bundle, today).await?)
    }

    /// Confirms which side of a conflict is right (§9.7 rule 4).
    ///
    /// Keeps the claim at `ordinal`, retires every believed rival on the same attribute, and marks
    /// the concept human-verified, which pins it against §9.9's decay and §9.10's archival. That
    /// is what a person picking a side actually means, and it is the only thing in the system that
    /// resolves rule 4: the store deliberately refuses to guess, so without this the concept stays
    /// out of use forever.
    ///
    /// # Errors
    /// Fails if the concept cannot be read or written, or `ordinal` names no claim.
    pub async fn settle(&self, path: &str, keep: u32, today: Date) -> Result<(), MemoryError> {
        let mut concept = {
            let reader = self.bundle.reader().await;
            reader.load_concept(path)?
        };

        let Some(winner) = concept.claims().nth(keep as usize).cloned() else {
            return Err(MemoryError::NoSuchClaim {
                path: path.to_owned(),
                ordinal: keep,
            });
        };

        for (at, claim) in concept.claims_mut().enumerate() {
            let is_winner = u32::try_from(at).is_ok_and(|n| n == keep);
            if is_winner {
                claim.confidence = Confidence::High;
            } else if claim.validity.is_believed() && claim.same_attribute_as(&winner) {
                // The world time of the losing claim closes today, because a person settling a
                // conflict is saying which is true now, not when the other stopped being true.
                claim.invalidate(today, today, &winner.text);
            }
        }

        concept.front.verified.push(Attribution {
            by: "human:user".to_owned(),
            at: today,
        });
        concept.front.status = Status::Stable;

        self.write_back(path, &concept, &format!("Settled by hand: {path}"), today)
            .await
    }

    /// Replaces what a claim says, on the user's word (§17.3's edit).
    ///
    /// A supersession rather than an overwrite, so principle 6 holds for a hand edit exactly as it
    /// does for a model one: the old wording keeps its window and the timeline can still show it.
    ///
    /// # Errors
    /// Fails if the concept cannot be read or written, or `ordinal` names no claim.
    pub async fn amend(
        &self,
        path: &str,
        ordinal: u32,
        text: &str,
        today: Date,
    ) -> Result<(), MemoryError> {
        let mut concept = {
            let reader = self.bundle.reader().await;
            reader.load_concept(path)?
        };
        let Some(old) = concept.claims().nth(ordinal as usize).cloned() else {
            return Err(MemoryError::NoSuchClaim {
                path: path.to_owned(),
                ordinal,
            });
        };

        let mut fresh = Claim::new(text, Origin::Stated, today).about(&old.attribute);
        fresh.validity.valid_from = old.validity.valid_from;
        fresh.privacy = old.privacy;

        for (at, claim) in concept.claims_mut().enumerate() {
            if u32::try_from(at).is_ok_and(|n| n == ordinal) {
                claim.invalidate(today, today, text);
            }
        }
        let heading = section_of(&concept, ordinal).unwrap_or_else(|| old.attribute.clone());
        concept.add(&heading, fresh);

        concept.front.verified.push(Attribution {
            by: "human:user".to_owned(),
            at: today,
        });
        self.write_back(path, &concept, &format!("Edited by hand: {path}"), today)
            .await
    }

    /// Retires a claim on the user's word, with nothing put in its place (§17.3's delete).
    ///
    /// Retired, not removed. Principle 6 and §9.5 both turn on the superseded claim still being
    /// there, and a store that deletes on a tap cannot show what it used to think.
    ///
    /// # Errors
    /// Fails if the concept cannot be read or written, or `ordinal` names no claim.
    pub async fn forget(&self, path: &str, ordinal: u32, today: Date) -> Result<(), MemoryError> {
        let mut concept = {
            let reader = self.bundle.reader().await;
            reader.load_concept(path)?
        };
        if concept.claims().nth(ordinal as usize).is_none() {
            return Err(MemoryError::NoSuchClaim {
                path: path.to_owned(),
                ordinal,
            });
        }
        for (at, claim) in concept.claims_mut().enumerate() {
            if u32::try_from(at).is_ok_and(|n| n == ordinal) {
                claim.validity.valid_to = Some(today);
                claim.validity.unlearned = Some(today);
                claim.replaced_by = None;
            }
        }
        self.write_back(path, &concept, &format!("Forgotten by hand: {path}"), today)
            .await
    }

    /// Folds one card into another (§9.4).
    ///
    /// The repair for a split. Everything else in §9.4 stops a split happening at write time;
    /// nothing repaired one afterwards, so a name used before it was known to be the user's own
    /// left two cards for one person with no way back.
    ///
    /// **Never automatic.** A wrong merge silently hides a true fact while a split leaves two
    /// visible rows, which is §21.2's asymmetry, so this is only ever called because somebody
    /// looked at both cards and said yes.
    ///
    /// `from` becomes a tombstone: deprecated, emptied, and carrying `merged_into`. Links into it
    /// still resolve and git still holds what it had.
    ///
    /// # Errors
    /// Fails if either card cannot be read or written, if they are the same card, or if `from` has
    /// already been merged somewhere.
    pub async fn merge(&self, from: &str, into: &str, today: Date) -> Result<(), MemoryError> {
        if from == into {
            return Err(MemoryError::CannotMerge {
                why: "a card cannot be merged into itself".to_owned(),
            });
        }
        let (mut source, mut target) = {
            let reader = self.bundle.reader().await;
            (reader.load_concept(from)?, reader.load_concept(into)?)
        };
        if let Some(already) = &source.front.merged_into {
            return Err(MemoryError::CannotMerge {
                why: format!("{from} was already merged into {already}"),
            });
        }

        // The name first, so the claims arriving below are filed under a card that is already
        // called the right thing.
        if target.front.label == Label::Described && source.front.label == Label::Named {
            target.front.rename(&source.front.name);
        } else {
            target.front.learn_alias(&source.front.name);
        }
        for alias in &source.front.aliases {
            target.front.learn_alias(alias);
        }
        for relation in &source.front.relations {
            if relation.is_current() {
                target.front.relate(&relation.label, &relation.to, today);
            }
        }

        let arriving: Vec<(String, Claim)> = source
            .sections
            .iter()
            .flat_map(|section| {
                section
                    .claims
                    .iter()
                    .map(|claim| (section.heading.clone(), claim.clone()))
            })
            .collect();
        for (heading, claim) in arriving {
            // Two cards about one person say some of the same things. A restatement is a second
            // occurrence, not a second claim, which is the same rule consolidation applies.
            if target.claims().any(|held| held.restates(&claim)) {
                continue;
            }
            target.add(&heading, claim);
        }

        source.sections.clear();
        source.front.aliases.clear();
        source.front.relations.clear();
        source.front.status = Status::Deprecated;
        source.front.merged_into = Some(into.to_owned());

        {
            let writer = self.bundle.writer().await;
            writer.save_concept(into, &target)?;
            writer.save_concept(from, &source)?;
        }
        self.repoint(from, into).await?;
        {
            let writer = self.bundle.writer().await;
            writer.commit(&format!("Merged {from} into {into}"))?;
        }
        {
            let reader = self.bundle.reader().await;
            self.index.sync(&reader)?;
        }
        Ok(())
    }

    /// Moves every edge that pointed at the merged card onto the one it merged into.
    ///
    /// Without this the owner's `sister` edge would keep pointing at a tombstone, and the graph
    /// lookups that resolve "my sister" would stop finding anybody.
    async fn repoint(&self, from: &str, into: &str) -> Result<(), MemoryError> {
        let paths = {
            let reader = self.bundle.reader().await;
            reader.concepts()?
        };
        for path in paths {
            if path == from {
                continue;
            }
            let mut concept = {
                let reader = self.bundle.reader().await;
                match reader.load_concept(&path) {
                    Ok(concept) => concept,
                    Err(_) => continue,
                }
            };
            let mut touched = false;
            for relation in &mut concept.front.relations {
                if relation.to == from {
                    relation.to = into.to_owned();
                    touched = true;
                }
            }
            if touched {
                let writer = self.bundle.writer().await;
                writer.save_concept(&path, &concept)?;
            }
        }
        Ok(())
    }

    /// Drops one of the other names an entity answers to, on the user's word (§17.3).
    ///
    /// An alias is knowledge, so it needs the same one tap a claim has. Removed rather than
    /// retired: unlike a claim it carries no window, and a name the user says is wrong was never
    /// true rather than true until now.
    ///
    /// # Errors
    /// Fails if the card cannot be read or written.
    pub async fn forget_alias(
        &self,
        path: &str,
        form: &str,
        today: Date,
    ) -> Result<(), MemoryError> {
        let mut concept = {
            let reader = self.bundle.reader().await;
            reader.load_concept(path)?
        };
        concept
            .front
            .aliases
            .retain(|held| !held.eq_ignore_ascii_case(form.trim()));
        self.write_back(
            path,
            &concept,
            &format!("Dropped the name {form} from {path}"),
            today,
        )
        .await
    }

    /// Closes an edge on the user's word, on `today` (§9.4).
    ///
    /// Closed, not deleted, because unlike a name an edge has a window: a manager who changed is a
    /// different thing from a manager who was never yours, and the file keeps both.
    ///
    /// # Errors
    /// Fails if the card cannot be read or written.
    pub async fn forget_relation(
        &self,
        path: &str,
        label: &str,
        to: &str,
        today: Date,
    ) -> Result<(), MemoryError> {
        let mut concept = {
            let reader = self.bundle.reader().await;
            reader.load_concept(path)?
        };
        for edge in &mut concept.front.relations {
            if edge.is_current() && edge.label.eq_ignore_ascii_case(label) && edge.to == to {
                edge.until = Some(today);
            }
        }
        self.write_back(path, &concept, &format!("Closed {label} on {path}"), today)
            .await
    }

    /// Rewrites the working set from the current files (§9.2).
    ///
    /// A hand edit changes what the prefix should say, and the prefix is otherwise only rebuilt by
    /// consolidation. Without this, correcting something on the trust surface leaves the model
    /// still reading the old version until the next session closes.
    ///
    /// # Errors
    /// Fails if the bundle cannot be read or written.
    pub async fn refresh_working_set(&self, today: Date) -> Result<(), MemoryError> {
        working_set::generate(&self.bundle, &self.index, self.scope, today).await?;
        Ok(())
    }

    /// Saves, commits and re-indexes. The three steps every hand edit needs, in one place.
    async fn write_back(
        &self,
        path: &str,
        concept: &RawConcept,
        message: &str,
        today: Date,
    ) -> Result<(), MemoryError> {
        {
            let writer = self.bundle.writer().await;
            writer.save_concept(path, concept)?;
            writer.commit(message)?;
        }
        {
            let reader = self.bundle.reader().await;
            self.index.sync(&reader)?;
        }
        // The prefix is otherwise only rebuilt by consolidation, so a correction made on the trust
        // surface would not reach the model until the session ended. Correcting something and
        // watching it be repeated is the same failure as not being heard at all.
        self.refresh_working_set(today).await
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
        // The buffer is the work. Empty means nothing to consolidate, and it is a better test
        // than a turn counter because it survives a crash: a session that ended without a close
        // leaves its buffer on disk and the next launch picks it up (B-30, §18.2).
        if !self.has_unconsolidated().await {
            return Ok(Report::default());
        }

        // §9.13: the recall counts live in the record, so an index rebuild loses the rows and
        // keeps the signal. Folded *before* the pass, not after, because promotion reads them: a
        // claim that has earned its place should be promoted by this pass rather than the next.
        self.fold_recalls().await?;
        // §10.6: the rows are disposable working data, pruned past the promotion window. The
        // counts folded above are the record, so pruning costs nothing that matters.
        self.index.prune_recalls(today, RECALL_WINDOW_DAYS)?;
        {
            let reader = self.bundle.reader().await;
            self.index.sync(&reader)?;
        }

        // Read the buffer, not the episode. The episode is the permanent record and grows all day,
        // so extracting from it made every close re-read the whole day and the extractor, being a
        // model, worded each fact differently every time. The buffer holds only what has not been
        // consolidated yet.
        let episodes = [Episode {
            path: bundle::CURRENT.to_owned(),
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

        // §9.13: the recall counts live in the record, so an index rebuild loses the rows and
        // keeps the signal. Folded before the working set is generated, because a claim that just
        // earned promotion should reach the prefix on this pass rather than the next.
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
        // Only once the claims exist and are committed. Emptying the buffer before that would
        // lose the session if the pass failed partway, and the buffer is the only copy of what
        // has not been consolidated.
        //
        // A rejected pass keeps its buffer too: the fallback appended rather than superseding, so
        // the next run gets another chance at doing it properly.
        if report.remaining.is_empty() && report.rejected.is_none() {
            consolidate::clear_buffer(&self.bundle).await?;
            let writer = self.bundle.writer().await;
            writer.commit("Clear the session buffer")?;
        }
        // The turns stay in the live corpus, because the session may continue: closing the window
        // is a session boundary, not the end of the conversation.
        Ok(report)
    }
}

/// Writes the owner and assistant cards if they are not already there (§9.4, S-21).
///
/// Before the first turn rather than on first mention. Seeded lazily they would be created by
/// whichever claim happened to arrive first, and the aliases that make "the user" resolve to the
/// owner would not exist for the claim that needed them. §11.3's import depends on this too: every
/// "I" in an exported chat is the same person, and with no card to point at, each export writes
/// another one.
///
/// **`you` belongs to the assistant and never to the owner.** From inside a conversation "you" is
/// Loki, so a store that answered both would file "you are Loki" onto the owner. The plan listed it
/// under the owner; this is the correction, see D-066. `I` is left out for a second reason: it is
/// one letter, and blocking's near-name match on a single character catches everything.
async fn seed_singletons(bundle: &Bundle, today: Date) -> Result<(), MemoryError> {
    use super::concept::{Frontmatter, Label, RawConcept, Role, render};

    for (path, name, role, label, aliases) in [
        (
            bundle::OWNER,
            // Not "you". The card's name is a surface form blocking answers to, and inside a
            // conversation "you" is Loki. A card named that would take "you are Loki" onto the
            // owner, which is the exact collision seeding exists to prevent.
            "the user",
            Role::Owner,
            // No name has been given yet, which is exactly what `described` records.
            Label::Described,
            ["me", "myself", "the owner"].as_slice(),
        ),
        (
            bundle::ASSISTANT,
            "Loki",
            Role::Assistant,
            Label::Named,
            ["you", "the assistant"].as_slice(),
        ),
    ] {
        let exists = {
            let reader = bundle.reader().await;
            reader.read(path).is_ok()
        };
        if exists {
            continue;
        }
        // Seeded with no claims, so it is `draft` and the gate keeps it out of every prompt until
        // it has learned something. No exception needed anywhere.
        let mut front = Frontmatter::new(name, today);
        front.role = role;
        front.label = label;
        front.aliases = aliases.iter().map(|a| (*a).to_owned()).collect();
        let writer = bundle.writer().await;
        writer.write(path, &render(&RawConcept::new(front)))?;
    }
    Ok(())
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

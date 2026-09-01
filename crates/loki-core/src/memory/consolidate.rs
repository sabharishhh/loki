//! Consolidation: episodes in, memory out (§9.8).
//!
//! ```text
//! 1  Extract     episodes -> candidate claims, oldest first
//! 2  Resolve     each claim -> entity, via blocking then matching (§9.4)
//! 3  Reconcile   set validity intervals, invalidate what was superseded (§9.5, §9.7)
//! 4  Promote     draft -> stable for what has earned it
//! 5  Regenerate  rebuild the working set, commit
//! ```
//!
//! **Oldest first is not a detail.** Process out of order and you cannot tell which claim replaced
//! which, so validity intervals become guesses.
//!
//! Built as a driver over a list of episodes rather than a session-close hook, because §11.3 says
//! import is this same pipeline over historical episodes: one code path, not two systems. The
//! reference time, the provenance and the budget are therefore parameters, and the run is
//! resumable so §11.5's paused import can continue where it stopped.

use async_trait::async_trait;
use jiff::civil::Date;

use super::bundle::{Bundle, BundleError, SCRATCH};
use super::claim::{Claim, Source};
use super::concept::{self, Frontmatter, RawConcept, Status};
use super::index::{Index, IndexError};
use super::reconcile::{self, Precedence, Promotion, Reference};
use super::resolve::{self, Kind, Matcher, Resolution, ResolveError};

/// How long a stable, unused concept waits before it is archived (§9.10).
pub const ARCHIVE_AFTER_DAYS: i64 = 180;

#[derive(Debug, thiserror::Error)]
pub enum ConsolidateError {
    #[error("could not read or write the bundle: {0}")]
    Bundle(#[from] BundleError),
    #[error("could not update the index: {0}")]
    Index(#[from] IndexError),
    #[error("could not resolve an entity: {0}")]
    Resolve(#[from] ResolveError),
    #[error("extraction failed: {0}")]
    Extract(String),
}

/// One episode to consolidate, with the reference its relative times resolve against (§9.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Episode {
    /// Path in the bundle, for example `episodes/2026-08-29.md`.
    pub path: String,
    pub reference: Reference,
}

/// A claim an extractor found, before it has an entity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The entity as the text referred to it, for §9.4's blocking.
    pub surface: String,
    pub kind: Kind,
    pub heading: String,
    pub text: String,
    /// Days before the reference, for a relative expression. `None` means it is already absolute.
    pub days_ago: Option<i64>,
    /// Absolute world time, when the text gave one.
    pub valid_from: Option<Date>,
    pub source: Source,
    pub tags: Vec<String>,
}

impl Candidate {
    /// When this claim started being true, resolved against the right reference (§9.6).
    ///
    /// An absolute date in the text wins. Otherwise a relative expression counts back from the
    /// reference, which for an import is the source message's timestamp, not today.
    #[must_use]
    pub fn world_time(&self, reference: Reference) -> Date {
        self.valid_from
            .unwrap_or_else(|| reference.resolve(self.days_ago.unwrap_or(0)))
    }
}

/// Turns an episode into candidate claims. One Utility-role call (§22.2).
///
/// A trait so the pipeline is testable without a model in the loop. A test whose setup is a model
/// call measures two things and fails for the wrong reasons.
#[async_trait]
pub trait Extractor: Send + Sync {
    /// # Errors
    /// Fails if the underlying call fails.
    async fn extract(&self, episode: &str, text: &str) -> Result<Vec<Candidate>, ConsolidateError>;
}

/// Stops the run when the ceiling is reached, so §11.5's import pauses rather than overrunning.
pub trait Budget: Send + Sync {
    /// Whether there is room for another episode.
    fn may_continue(&self) -> bool;
}

/// A budget that never stops. The live session's default: consolidation of one session is small.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unbounded;

impl Budget for Unbounded {
    fn may_continue(&self) -> bool {
        true
    }
}

/// What a run did. Feeds §17.4's session summary and §21.2's scoring.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub episodes: Vec<String>,
    pub extracted: usize,
    pub created: Vec<String>,
    pub promoted: Vec<String>,
    pub archived: Vec<String>,
    /// Every reconcile decision, so over-supersession can be scored rather than assumed.
    pub decisions: Vec<reconcile::Decided>,
    /// Entity ties and rule-4 conflicts. One tap each, and neither is guessed at.
    pub surfaced: Vec<String>,
    /// Episodes left unprocessed because the budget ran out. Pass these back in to resume.
    pub remaining: Vec<String>,
}

impl Report {
    /// Whether anything happened that is worth telling the user about (§17.4).
    ///
    /// A confidence bump is not news. Silence when nothing happened, because a card that says
    /// "learned nothing today" teaches people to ignore the card.
    #[must_use]
    pub fn is_newsworthy(&self) -> bool {
        !self.promoted.is_empty() || !self.decisions.is_empty() || !self.surfaced.is_empty()
    }
}

/// Runs the pipeline over `episodes`, oldest first.
///
/// # Errors
/// Fails if the bundle cannot be read or written, the index cannot be updated, or extraction
/// fails. A budget running out is not an error: the untouched episodes come back in
/// [`Report::remaining`].
pub async fn run(
    episodes: &[Episode],
    bundle: &Bundle,
    index: &Index,
    extractor: &dyn Extractor,
    matcher: &dyn Matcher,
    budget: &dyn Budget,
    today: Date,
) -> Result<Report, ConsolidateError> {
    let mut report = Report::default();

    for (at, episode) in episodes.iter().enumerate() {
        if !budget.may_continue() {
            report.remaining = episodes[at..].iter().map(|e| e.path.clone()).collect();
            break;
        }

        let text = {
            let reader = bundle.reader().await;
            reader.read(&episode.path)?
        };
        let candidates = extractor.extract(&episode.path, &text).await?;
        report.episodes.push(episode.path.clone());
        report.extracted += candidates.len();

        for candidate in candidates {
            let resolution = resolve::resolve(
                &candidate.surface,
                &candidate.tags,
                &candidate.text,
                candidate.kind,
                index,
                matcher,
            )
            .await?;
            absorb(
                &candidate,
                resolution,
                episode,
                bundle,
                index,
                &mut report,
                today,
            )
            .await?;
        }
    }

    archive_stale(bundle, &mut report, today).await?;

    {
        let writer = bundle.writer().await;
        writer.commit(&summary_line(&report))?;
    }
    {
        let reader = bundle.reader().await;
        index.sync(&reader)?;
    }
    Ok(report)
}

/// Writes one candidate into the entity it resolved to.
async fn absorb(
    candidate: &Candidate,
    resolution: Resolution,
    episode: &Episode,
    bundle: &Bundle,
    index: &Index,
    report: &mut Report,
    today: Date,
) -> Result<(), ConsolidateError> {
    let valid_from = candidate.world_time(episode.reference);
    let claim = match candidate.source {
        Source::Stated => Claim::stated(&candidate.text, valid_from),
        Source::Inferred => Claim::inferred(&candidate.text, valid_from, today),
    };

    match resolution {
        Resolution::Ambiguous { between } => {
            // §9.4's known failure, two people with the same name. Create neither.
            report.surfaced.push(format!(
                "{} could be {}",
                candidate.surface,
                between.join(" or ")
            ));
            Ok(())
        }
        Resolution::New { path, aliases } => {
            // Blocking can miss, so a "new" entity whose file already exists is merged into,
            // never written over. Overwriting would silently drop every claim already there,
            // and losing memory is the one failure this whole subsystem exists to prevent.
            let existing = {
                let reader = bundle.reader().await;
                reader.load_concept(&path).ok()
            };
            if let Some(mut concept) = existing {
                merge(&mut concept, &path, candidate, claim, report, today);
                // Scoped, because `refresh` below takes a reader and the write guard excludes it.
                {
                    let writer = bundle.writer().await;
                    writer.save_concept(&path, &concept)?;
                }
            } else {
                let mut front = Frontmatter::new(&candidate.surface, today);
                front.aliases = aliases;
                front.tags.clone_from(&candidate.tags);
                // A first mention stays draft. It promotes on a second occurrence, or on use
                // without correction, which stops one offhand remark becoming a fact about you.
                front.status = Status::Draft;
                let mut concept = RawConcept::new(front);
                concept.add(&candidate.heading, claim);

                {
                    let writer = bundle.writer().await;
                    writer.save_concept(&path, &concept)?;
                }
                report.created.push(path);
            }
            refresh(bundle, index).await?;
            Ok(())
        }
        Resolution::Existing { path } => {
            let mut concept = {
                let reader = bundle.reader().await;
                reader.load_concept(&path)?
            };
            merge(&mut concept, &path, candidate, claim, report, today);
            {
                let writer = bundle.writer().await;
                writer.save_concept(&path, &concept)?;
            }
            refresh(bundle, index).await?;
            Ok(())
        }
    }
}

/// Re-syncs the index mid-run.
///
/// Blocking reads the index, so an entity created earlier in this same run has to be visible to
/// the claims that follow it. Without this, a second claim about the same person resolves as new,
/// and the write lands on top of the first. The sync is incremental, so this costs one stat per
/// concept rather than a rebuild.
async fn refresh(bundle: &Bundle, index: &Index) -> Result<(), ConsolidateError> {
    let reader = bundle.reader().await;
    index.sync(&reader)?;
    Ok(())
}

/// Merges a claim into a concept that already exists, applying §9.7 and then §9.8.
fn merge(
    concept: &mut RawConcept,
    path: &str,
    candidate: &Candidate,
    claim: Claim,
    report: &mut Report,
    today: Date,
) {
    let conflict = concept
        .claims()
        .find(|held| conflicts(held, &claim))
        .map(|held| (held.text.clone(), held.clone()));

    let Some((held_text, held)) = conflict else {
        let occurrences =
            1 + u32::try_from(concept.claims().filter(|c| c.text == claim.text).count())
                .unwrap_or(u32::MAX);
        let decision = reconcile::promotion(&claim, occurrences, false);
        concept.add(&candidate.heading, claim);
        if decision == Promotion::Auto && concept.front.status == Status::Draft {
            concept.front.status = Status::Stable;
            report.promoted.push(path.to_owned());
        }
        return;
    };

    let outcome = reconcile::precedence(&held, &claim, false);
    report.decisions.push(reconcile::Decided {
        concept: path.to_owned(),
        held: held_text.clone(),
        incoming: claim.text.clone(),
        outcome,
    });
    if outcome == Precedence::Surface {
        report
            .surfaced
            .push(format!("{held_text} against {}", claim.text));
    }
    reconcile::apply(
        concept,
        &candidate.heading,
        &held_text,
        claim,
        outcome,
        today,
    );
}

/// Whether two claims are about the same thing, so only one of them can be true.
///
/// Same heading is not enough and same text is too strict. The signal we have without a model is
/// the section they sit under, which extraction assigns, plus them not being literally identical.
fn conflicts(held: &Claim, incoming: &Claim) -> bool {
    held.validity.is_believed() && held.text != incoming.text
}

/// Archives stable concepts that have aged out without being used (§9.10).
async fn archive_stale(
    bundle: &Bundle,
    report: &mut Report,
    today: Date,
) -> Result<(), ConsolidateError> {
    let paths = {
        let reader = bundle.reader().await;
        reader.concepts()?
    };
    for path in paths {
        let mut concept = {
            let reader = bundle.reader().await;
            match reader.load_concept(&path) {
                Ok(concept) => concept,
                // A file that will not parse is not a reason to abandon the run.
                Err(_) => continue,
            }
        };
        if !reconcile::should_archive(&concept, today, ARCHIVE_AFTER_DAYS) {
            continue;
        }
        // Nothing is deleted by heuristic. Deprecated stays linkable and searchable.
        concept.front.status = Status::Deprecated;
        {
            let writer = bundle.writer().await;
            writer.save_concept(&path, &concept)?;
        }
        report.archived.push(path);
    }
    Ok(())
}

/// Deletes the scratch files a run promoted, so the directory listing matches what is live.
///
/// §9.8: merge, do not append.
///
/// # Errors
/// Fails if the bundle cannot be written.
pub async fn clear_promoted(bundle: &Bundle, sources: &[String]) -> Result<(), BundleError> {
    let writer = bundle.writer().await;
    for path in sources {
        if path.starts_with(SCRATCH) {
            writer.remove(path)?;
        }
    }
    Ok(())
}

fn summary_line(report: &Report) -> String {
    format!(
        "Consolidate {} episode(s): {} created, {} promoted, {} superseded, {} archived",
        report.episodes.len(),
        report.created.len(),
        report.promoted.len(),
        report
            .decisions
            .iter()
            .filter(|d| d.outcome == Precedence::Replace)
            .count(),
        report.archived.len()
    )
}

/// Renders a run as plain-language lines for `log.md`, which the timeline and the session summary
/// both read (§17.3, §17.4).
///
/// The sentence §9.5 exists to make writable: what replaced what, and how long we were wrong.
#[must_use]
pub fn log_lines(report: &Report, concepts: &[(String, RawConcept)], today: Date) -> Vec<String> {
    let mut lines = Vec::new();
    for decided in &report.decisions {
        if decided.outcome != Precedence::Replace {
            continue;
        }
        let wrong_for = concepts
            .iter()
            .find(|(path, _)| *path == decided.concept)
            .and_then(|(_, concept)| {
                concept
                    .claims()
                    .find(|c| c.text == decided.held)
                    .and_then(|c| c.validity.wrong_for_days())
            });
        let tail = match wrong_for {
            Some(days) if days > 0 => format!(", and I was wrong about it for {days} days"),
            _ => String::new(),
        };
        lines.push(format!(
            "{today}: {} replaced {}{tail}.",
            decided.incoming, decided.held
        ));
    }
    for surfaced in &report.surfaced {
        lines.push(format!("{today}: needs you, {surfaced}."));
    }
    lines
}

/// Re-exported so callers do not have to reach into two modules to build a run.
pub use concept::Section;

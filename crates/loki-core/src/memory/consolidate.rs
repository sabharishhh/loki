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

use super::bundle::{self, Bundle, BundleError};
use super::claim::{Claim, Origin};
use super::concept::{Frontmatter, RawConcept, Status};
use super::index::{Index, IndexError};
use super::reconcile::{self, Precedence, Promotion, Reference};
use super::resolve::{self, Kind, Matcher, Resolution, ResolveError};

/// How long a stable, unused concept waits before it is archived (§9.10).
pub const ARCHIVE_AFTER_DAYS: i64 = 180;

/// The share of live claims one pass may retire before it is rejected (§9.8 step 5).
///
/// Open question 19: nobody can pick this before there is a store to observe. A half is loose
/// enough that a real correction pass never trips it and tight enough that a pass gone wrong is
/// caught, and it is one named number to change rather than a rule to rewrite.
///
/// The check is cheap for us in a way it is not for others: the pre-image is `HEAD`, so there is
/// no snapshot to take and the recovery path is `git revert`, which §14.3 already designates as
/// the memory undo.
pub const BOUNDED_LOSS: f32 = 0.5;

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
    /// What the claim is about: a short predicate such as `name`, `employer`, `city`.
    ///
    /// Reconciliation keys on this, so an extractor that leaves it empty produces claims that
    /// only ever accumulate and never supersede one another.
    pub attribute: String,
    pub text: String,
    /// Days before the reference, for a relative expression. `None` means the text gave none.
    pub days_ago: Option<i64>,
    /// Absolute world time, when the text gave one.
    pub valid_from: Option<Date>,
    /// Where the claim came from (§9.12). Extraction over a live session writes `stated` or
    /// `inferred`; a pass over fetched content writes `web`, and one over an account's data
    /// writes `connector`.
    pub origin: Origin,
    pub tags: Vec<String>,
}

impl Candidate {
    /// When this claim started being true, resolved against the right reference (§9.6).
    ///
    /// An absolute date in the text wins. Otherwise a relative expression counts back from the
    /// reference, which for an import is the source message's timestamp, not today.
    ///
    /// `None` when the source gave neither, which is most of what a person says. §9.5: world time
    /// is set only when the source dates it, and defaulting it to the write date is what made
    /// every pair the same vintage and fired rule 4 on almost everything.
    #[must_use]
    pub fn world_time(&self, reference: Reference) -> Option<Date> {
        self.valid_from
            .or_else(|| self.days_ago.map(|days| reference.resolve(days)))
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
    /// Set when step 5 refused the pass and the store was rolled back (§9.8, §21.2).
    ///
    /// Reported, not silent. §21.2 wants the rate because a rejection is a near miss that was
    /// caught, and §17.4 gives it a line because a pass that declined to retire most of your
    /// memory is exactly the kind of thing a person should be told about.
    pub rejected: Option<String>,
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
    /// How many claims this pass retired. Step 5's numerator.
    #[must_use]
    pub fn superseded(&self) -> usize {
        self.decisions
            .iter()
            .filter(|d| d.outcome == Precedence::Replace)
            .count()
    }

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
    // Step 5's pre-image. Counted before anything is written, because the check compares against
    // what the store held rather than against what this pass thinks it held.
    let before = live_claims(bundle).await;

    let mut report = pass(
        episodes,
        bundle,
        index,
        extractor,
        matcher,
        budget,
        Settings {
            today,
            keep: Keep::No,
        },
    )
    .await?;

    // 5. Verify. A pass that retires too much is refused, the store goes back to what it was, and
    //    the same episodes run again with nothing allowed to retire. §9.8: reject the rewrite,
    //    fall back to appending, record the rejection.
    let retired = report.superseded();
    if too_much_lost(before, retired) {
        {
            let writer = bundle.writer().await;
            writer.discard_changes()?;
        }
        report = pass(
            episodes,
            bundle,
            index,
            extractor,
            matcher,
            budget,
            Settings {
                today,
                keep: Keep::Everything,
            },
        )
        .await?;
        report.rejected = Some(format!(
            "a pass would have retired {retired} of {before} claims, so nothing was retired"
        ));
    }

    // 6. Regenerate the catalog, then commit. The working set is the caller's, because it needs
    //    the scope the prompt will be built with.
    catalog(bundle).await?;
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

/// What a pass needs to know that is the same for every claim in it.
///
/// One value rather than two parameters threaded through four functions. Both are properties of
/// the run and not of the claim, so they travel together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Settings {
    today: Date,
    keep: Keep,
}

/// Whether a pass may retire anything (§9.8 step 5's fallback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keep {
    /// The ordinary pass. Supersession works.
    No,
    /// The fallback after a rejection. New claims are appended and nothing is invalidated, so a
    /// bad extraction costs a duplicate rather than the store.
    Everything,
}

/// Steps 1 to 4, plus archival. Everything that writes, with nothing that commits.
async fn pass(
    episodes: &[Episode],
    bundle: &Bundle,
    index: &Index,
    extractor: &dyn Extractor,
    matcher: &dyn Matcher,
    budget: &dyn Budget,
    settings: Settings,
) -> Result<Report, ConsolidateError> {
    let mut report = Report::default();

    for (at, episode) in episodes.iter().enumerate() {
        if !budget.may_continue() {
            report.remaining = episodes[at..].iter().map(|e| e.path.clone()).collect();
            break;
        }

        let text = {
            let reader = bundle.reader().await;
            unrecalled(&reader.read(&episode.path)?)
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
                settings,
            )
            .await?;
        }
    }

    archive_stale(bundle, &mut report, settings.today).await?;
    Ok(report)
}

/// Drops the lines that are Loki quoting itself (§9.8).
///
/// **Recalled content is never re-extracted.** A claim pre-fetch injected into a turn is marked in
/// the buffer, and extraction skips those lines. Without this a fact recalled a hundred times
/// becomes a hundred claims phrased a hundred ways, which is the duplication the build kept
/// producing.
fn unrecalled(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with(super::handle::RECALLED))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Claims that are believed right now, across the whole store. Step 5's denominator.
async fn live_claims(bundle: &Bundle) -> usize {
    let reader = bundle.reader().await;
    let Ok(paths) = reader.concepts() else {
        return 0;
    };
    paths
        .iter()
        .filter_map(|path| reader.load_concept(path).ok())
        .map(|concept| {
            concept
                .claims()
                .filter(|claim| claim.validity.is_believed())
                .count()
        })
        .sum()
}

/// Whether a pass retired more than §9.8 allows.
///
/// **Counts what the pass retired, not the net change.** Net claim count is the obvious measure
/// and it cannot work here: every supersession adds a replacement as it retires, so the total
/// never moves and the check would be dead code. What matters is how much of the store one pass
/// decided was no longer true.
///
/// A store that had nothing cannot lose too much, so an empty pre-image always passes.
fn too_much_lost(before: usize, retired: usize) -> bool {
    if before == 0 {
        return false;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "§10.7 sizes the store in thousands of claims, far below f32's exact range"
    )]
    let share = retired as f32 / before as f32;
    share > BOUNDED_LOSS
}

/// Rewrites `index.md` as the concept catalog (§9.3, §10.3).
///
/// Written by consolidation, which already rewrites the files, so it costs nothing extra and can
/// never drift from the content it describes. Lane 1 never reads it; it is an entry point for a
/// lane 2 search and may never answer a query on its own.
async fn catalog(bundle: &Bundle) -> Result<(), ConsolidateError> {
    let mut out = String::from("---\nokf_version: '0.2'\n---\n\n# Loki memory\n\n");
    {
        let reader = bundle.reader().await;
        let mut lines: Vec<String> = reader
            .concepts()?
            .iter()
            .filter_map(|path| reader.load_concept(path).ok().map(|c| (path.clone(), c)))
            .map(|(path, concept)| {
                let summary = concept
                    .claims()
                    .find(|claim| claim.validity.is_believed())
                    .map_or_else(String::new, |claim| format!(" — {}", claim.text));
                format!("- [{}]({path}){summary}", concept.front.name)
            })
            .collect();
        lines.sort_unstable();
        out.push_str(&lines.join("\n"));
        out.push('\n');
    }
    let writer = bundle.writer().await;
    writer.write(bundle::INDEX, &out)?;
    Ok(())
}

/// Writes one candidate into the entity it resolved to.
async fn absorb(
    candidate: &Candidate,
    resolution: Resolution,
    episode: &Episode,
    bundle: &Bundle,
    index: &Index,
    report: &mut Report,
    settings: Settings,
) -> Result<(), ConsolidateError> {
    let mut claim =
        Claim::new(&candidate.text, candidate.origin, settings.today).about(&candidate.attribute);
    if let Some(valid_from) = candidate.world_time(episode.reference) {
        claim = claim.dated(valid_from);
    }

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
                merge(&mut concept, &path, candidate, claim, report, settings);
                // Scoped, because `refresh` below takes a reader and the write guard excludes it.
                {
                    let writer = bundle.writer().await;
                    writer.save_concept(&path, &concept)?;
                }
            } else {
                let mut front = Frontmatter::new(&candidate.surface, settings.today);
                front.aliases = aliases;
                front.tags.clone_from(&candidate.tags);
                // What the user stated is usable at once; what was merely inferred waits for a
                // second occurrence. See `reconcile::promotion` for why the old rule could never
                // let a first mention through at all.
                front.status = if reconcile::promotion(&claim, false) == Promotion::Auto {
                    Status::Stable
                } else {
                    Status::Draft
                };
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
            merge(&mut concept, &path, candidate, claim, report, settings);
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
    settings: Settings,
) {
    // Saying the same thing again is a second occurrence, not a second claim and not a correction.
    // Exact repeats come from a session closing twice or §11.5 resuming a paused import; reworded
    // repeats come from the extractor being a model, which never phrases a fact the same way
    // twice. Both used to land as new claims, and the reworded ones then read as corrections to
    // something that had not changed.
    if held_restatements(concept, &claim) > 0 {
        if let Some(held) = concept
            .claims_mut()
            .find(|held| held.validity.is_believed() && held.restates(&claim))
        {
            held.reinforced_by(&claim);
        }
        promote(concept, path, &claim, report);
        return;
    }

    let conflict = concept
        .claims()
        .find(|held| conflicts(held, &claim))
        .map(|held| (held.text.clone(), held.clone()));

    let Some((held_text, held)) = conflict else {
        promote(concept, path, &claim, report);
        concept.add(&candidate.heading, claim);
        return;
    };

    // After a rejected pass nothing may retire, so a supersession becomes an append. §9.8's
    // fallback: a bad extraction costs a duplicate rather than the store.
    let outcome = match reconcile::precedence(&held, &claim, false) {
        Precedence::Replace if settings.keep == Keep::Everything => Precedence::Surface,
        other => other,
    };
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
        settings.today,
    );
}

/// How many believed claims already say what this one says.
///
/// Only believed ones count: a claim that was retired and is now stated again is a reversion, and
/// has to come back rather than be swallowed as a repeat of itself.
///
/// The count no longer feeds promotion, which reads recall behaviour instead (§9.8). It answers
/// one question now: is this a fact Loki already holds.
fn held_restatements(concept: &RawConcept, claim: &Claim) -> usize {
    concept
        .claims()
        .filter(|held| held.validity.is_believed() && held.restates(claim))
        .count()
}

/// Lifts a draft concept to stable once a claim has earned it (§9.8).
///
/// No conflict check here, on purpose. B-34 was fixed at this seam first, by refusing to promote
/// while a conflict stood, and that was the wrong place: status is per concept, so it took the
/// whole entity out of use over one argument. The rule belongs at the gate, per claim, where
/// `reconcile::is_shadowed` keeps the older side of a conflict out of a prompt whatever the
/// concept's status is.
fn promote(concept: &mut RawConcept, path: &str, claim: &Claim, report: &mut Report) {
    if reconcile::promotion(claim, false) == Promotion::Auto
        && concept.front.status == Status::Draft
    {
        concept.front.status = Status::Stable;
        report.promoted.push(path.to_owned());
    }
}

/// Whether two claims describe the same thing, so only one of them can be true (§9.7).
///
/// Keyed on the attribute, never on the text. Comparing text calls every second fact about a
/// person a contradiction: a name against a degree, which then takes the whole concept out of use
/// under rule 4. That was B-25, and it made memory unusable the moment an entity had two facts.
///
/// Zep decides contradiction the same way, structurally, on source plus relationship plus target.
/// A claim with no attribute never conflicts: it cannot say what it is about, so it has no
/// standing to displace one that can.
fn conflicts(held: &Claim, incoming: &Claim) -> bool {
    held.validity.is_believed() && !held.restates(incoming) && held.same_attribute_as(incoming)
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
        // Restatements already on disk, written before the rule that folds them existed or under
        // two spellings of one attribute key. Two claims that say the same thing are one fact and
        // a write that happened twice, so collapsing them loses no record.
        let folded = fold_restatements(&mut concept);
        if folded > 0 {
            let writer = bundle.writer().await;
            writer.save_concept(&path, &concept)?;
        }

        // A draft holding a claim that has already earned promotion is stale status, not a
        // candidate. This is how a store repairs itself after the rule that wrote the status
        // changed: rule 4 used to drop a whole concept to draft, and a concept could be left
        // holding a stated, unconflicted name that nothing would ever promote, because promotion
        // only runs when a new claim arrives for that entity.
        if concept.front.status == Status::Draft && earned_stable(&concept) {
            concept.front.status = Status::Stable;
            {
                let writer = bundle.writer().await;
                writer.save_concept(&path, &concept)?;
            }
            report.promoted.push(path);
            continue;
        }
        if folded > 0 {
            continue;
        }

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

/// Collapses believed claims that restate a believed sibling. Returns how many went.
///
/// The one place anything is removed rather than retired, and the reason it is allowed: a claim
/// that restates another is not a second fact, it is the same fact written twice. The surviving
/// claim says exactly what the removed one said, so principle 6's "nothing silently overwrites"
/// is not in play, and git holds the prior state either way.
///
/// Needed because the store predates the rules that would have prevented it. `restates` arrived
/// after some claims were written, and `interest` against `interests` kept two spellings of one
/// property from ever being compared.
fn fold_restatements(concept: &mut RawConcept) -> usize {
    let mut kept: Vec<Claim> = Vec::new();
    let mut dropped = 0usize;

    for section in &mut concept.sections {
        section.claims.retain(|claim| {
            if !claim.validity.is_believed() {
                return true;
            }
            if kept
                .iter()
                .any(|seen| seen.validity.is_believed() && seen.restates(claim))
            {
                dropped += 1;
                return false;
            }
            kept.push(claim.clone());
            true
        });
    }
    concept
        .sections
        .retain(|section| !section.claims.is_empty());
    dropped
}

/// Whether anything in a concept has already earned `stable` under §9.8's rules.
///
/// The promotion rule applied to what is on disk rather than to an arriving claim. A shadowed
/// claim does not count: it never reaches a prompt, so it cannot be the reason the concept is in
/// use.
fn earned_stable(concept: &RawConcept) -> bool {
    concept.claims().enumerate().any(|(at, claim)| {
        claim.validity.is_believed()
            && !reconcile::is_shadowed(concept, u32::try_from(at).unwrap_or(u32::MAX))
            && reconcile::promotion(claim, false) == Promotion::Auto
    })
}

/// Empties the session buffer once a pass has been committed (§9.3).
///
/// Only the buffer, and only after a successful pass. The episode is the permanent record and is
/// never cleared; clearing the buffer is what makes a second close in one session cheap and
/// non-duplicating, because it has nothing left to re-extract.
///
/// # Errors
/// Fails if the bundle cannot be written.
pub async fn clear_buffer(bundle: &Bundle) -> Result<(), BundleError> {
    let writer = bundle.writer().await;
    writer.write(bundle::CURRENT, "")?;
    Ok(())
}

fn summary_line(report: &Report) -> String {
    if let Some(why) = &report.rejected {
        return format!("Consolidation refused: {why}");
    }
    format!(
        "Consolidate {} episode(s): {} created, {} promoted, {} superseded, {} archived",
        report.episodes.len(),
        report.created.len(),
        report.promoted.len(),
        report.superseded(),
        report.archived.len()
    )
}

/// Extraction on the Utility role (§22.2).
///
/// One bounded structured call per episode, sharing no prefix with the conversation, so routing it
/// away from the Primary model costs no cache hit.
pub struct ModelExtractor<'a> {
    provider: &'a dyn crate::ports::model::ModelProvider,
    cancel: tokio_util::sync::CancellationToken,
}

impl<'a> ModelExtractor<'a> {
    #[must_use]
    pub fn new(
        provider: &'a dyn crate::ports::model::ModelProvider,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        Self { provider, cancel }
    }
}

const EXTRACT_INSTRUCTIONS: &str = "\
You read a transcript and list the durable facts it states about people, projects and preferences.

One fact per line, exactly this shape and nothing else:

  <entity> | <person|project|preference> | <attribute> | <stated|inferred> | <YYYY-MM-DD or -> | <the fact>

The attribute is the single most important column. It is a short lower-case key naming WHICH
PROPERTY of the entity the fact sets: name, employer, role, city, education, birthday, pronouns,
reply_style. Two facts sharing an entity and an attribute are treated as competing, and the newer
one replaces the older. Two facts with different attributes both stand.

Four rules about the writing itself, which matter more than any of the rest:
- Stay faithful to the source. Reorganize, never invent.
- Preserve every metadata field exactly: attribute, origin, world time, system time.
- Resolve duplicates and contradictions by the rules below, never by free-form summarizing.
- Return parseable output only, with no prose outside the format.

Rules:
- Only durable facts. A one-off question, a passing joke or a task instruction is not a fact.
- Give every fact an attribute, and reuse the same key for the same property every time. `employer`
  today and `works_at` tomorrow means the change of job is never noticed.
- Attribute keys are singular and lower case: `interest`, not `interests` or `Interests`.
- Never use one attribute for two different properties. A name and a degree are `name` and
  `education`, never both `identity`, or one will wrongly overwrite the other.
- One fact per line, each setting exactly one attribute. Split a sentence that states two
  properties into two lines.
- The entity is what the fact is ABOUT, not who mentioned it. A preference about how the assistant
  behaves belongs to that preference, never to the person who expressed it.
- Use the same entity name every time you refer to the same thing, so it resolves to one file.
- `stated` means the user said it about themselves or their world. `inferred` means you worked it
  out. Prefer `inferred` when unsure, since a stated fact is trusted immediately.
- The date is when the fact started being true, not today. Use `-` if the transcript does not say.
- The fact must be a complete statement that still makes sense on its own, with the entity in it.
  It is shown to the user and put into a prompt with no surrounding context, so a bare value is
  useless. Write it as `Sabharish is a computer science graduate`, never as the bare value
  `computer science graduate`, and never as just the name.
- Phrase the same property the same way each time, so a later mention supersedes rather than
  reading as a second fact.

Example, for a transcript where the user says they are Sabharish, a computer science graduate who
has just moved to Bangalore, and that they want short replies:

  Sabharish | person | name | stated | - | The user's name is Sabharish
  Sabharish | person | education | stated | - | Sabharish is a computer science graduate
  Sabharish | person | city | stated | - | Sabharish lives in Bangalore
  reply length | preference | reply_style | stated | - | Sabharish prefers short replies
- If the transcript states nothing durable, output nothing at all.";

#[async_trait]
impl Extractor for ModelExtractor<'_> {
    async fn extract(
        &self,
        _episode: &str,
        text: &str,
    ) -> Result<Vec<Candidate>, ConsolidateError> {
        use crate::core::vocab::ModelRole;
        use crate::ports::model::{Chunk, Message, Request, SystemBlock};
        use futures_util::StreamExt as _;

        let request = Request {
            role: ModelRole::Utility,
            system: vec![SystemBlock::new(EXTRACT_INSTRUCTIONS)],
            messages: vec![Message::user(text.to_owned())],
            max_tokens: 2_048,
        };
        let mut stream = self
            .provider
            .complete(request, self.cancel.clone())
            .await
            .map_err(|e| ConsolidateError::Extract(e.to_string()))?;

        let mut answer = String::new();
        while let Some(chunk) = stream.next().await {
            if let Chunk::Text(piece) =
                chunk.map_err(|e| ConsolidateError::Extract(e.to_string()))?
            {
                answer.push_str(&piece);
            }
        }
        Ok(parse_candidates(&answer))
    }
}

/// Reads the extractor's lines.
///
/// Tolerant, and it drops what it cannot read rather than failing the run. A malformed line costs
/// one fact; an error costs the whole session's memory.
fn parse_candidates(answer: &str) -> Vec<Candidate> {
    answer
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('|').map(str::trim).collect();
            let [surface, kind, attribute, source, when, fact] = parts.as_slice() else {
                return None;
            };
            if surface.is_empty() || fact.is_empty() {
                return None;
            }
            Some(Candidate {
                surface: (*surface).to_owned(),
                kind: match kind.to_lowercase().as_str() {
                    "project" => Kind::Project,
                    "preference" => Kind::Preference,
                    _ => Kind::Person,
                },
                // The section a claim is filed under is its attribute, so a reader opening the
                // file sees the same grouping reconciliation uses.
                heading: if attribute.is_empty() {
                    "Notes".to_owned()
                } else {
                    (*attribute).to_owned()
                },
                attribute: super::claim::normalize_attribute(attribute),
                text: (*fact).to_owned(),
                days_ago: None,
                valid_from: when.parse::<Date>().ok(),
                // Anything the extractor will not vouch for is inferred, so §9.7 rule 3 keeps it
                // below anything the user actually said. This pass reads a conversation, so it
                // can only ever produce the two user-facing origins: `web` and `connector` are
                // written by the passes that read those, never claimed by a model.
                origin: if source.eq_ignore_ascii_case("stated") {
                    Origin::Stated
                } else {
                    Origin::Inferred
                },
                tags: Vec::new(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extractor_lines_are_read() {
        let out = parse_candidates(
            "Meera | person | Role | stated | 2026-07-15 | moved to the infra team\n\
             Loki | project | Status | inferred | - | is a personal assistant\n",
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].surface, "Meera");
        assert_eq!(out[0].origin, Origin::Stated);
        assert_eq!(out[0].valid_from, Some(jiff::civil::date(2026, 7, 15)));
        assert_eq!(out[1].kind, Kind::Project);
        assert_eq!(out[1].valid_from, None);
    }

    /// A malformed line costs one fact. An error would cost the whole session's memory.
    #[test]
    fn unreadable_lines_are_dropped_not_fatal() {
        let out = parse_candidates(
            "not a fact line at all\n\
             | | | | |\n\
             Dan | person | Notes | stated | - | likes tea\n\
             Here is my answer:\n",
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].surface, "Dan");
    }

    #[test]
    fn an_empty_answer_yields_nothing() {
        assert!(parse_candidates("").is_empty());
        assert!(parse_candidates("\n\n").is_empty());
    }

    /// Anything not explicitly stated must land as inferred, so rule 3 keeps it subordinate.
    #[test]
    fn unclear_provenance_defaults_to_inferred() {
        let out = parse_candidates("Dan | person | Notes | guessed | - | likes tea");
        assert_eq!(out[0].origin, Origin::Inferred);
    }
}

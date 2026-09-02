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
use super::claim::{Claim, Origin};
use super::concept::{Frontmatter, RawConcept, Status};
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
    let mut claim =
        Claim::new(&candidate.text, candidate.origin, today).about(&candidate.attribute);
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
                // What the user stated is usable at once; what was merely inferred waits for a
                // second occurrence. See `reconcile::promotion` for why the old rule could never
                // let a first mention through at all.
                front.status = if reconcile::promotion(&claim, 1, false) == Promotion::Auto {
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
    // Saying the same thing again is a second occurrence, not a second claim and not a correction.
    // Exact repeats come from a session closing twice or §11.5 resuming a paused import; reworded
    // repeats come from the extractor being a model, which never phrases a fact the same way
    // twice. Both used to land as new claims, and the reworded ones then read as corrections to
    // something that had not changed.
    let repeats = held_restatements(concept, &claim);
    if repeats > 0 {
        if let Some(held) = concept
            .claims_mut()
            .find(|held| held.validity.is_believed() && held.restates(&claim))
        {
            held.reinforced_by(&claim);
        }
        promote(concept, path, &claim, repeats.saturating_add(1), report);
        return;
    }

    let conflict = concept
        .claims()
        .find(|held| conflicts(held, &claim))
        .map(|held| (held.text.clone(), held.clone()));

    let Some((held_text, held)) = conflict else {
        promote(concept, path, &claim, 1, report);
        concept.add(&candidate.heading, claim);
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

/// How many believed claims already say what this one says.
///
/// Only believed ones count: a claim that was retired and is now stated again is a reversion, and
/// has to come back rather than be swallowed as a repeat of itself.
fn held_restatements(concept: &RawConcept, claim: &Claim) -> u32 {
    let count = concept
        .claims()
        .filter(|held| held.validity.is_believed() && held.restates(claim))
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// Lifts a draft concept to stable once a claim has earned it (§9.8).
///
/// Refuses while the concept holds a conflict nobody has resolved. Rule 4 takes a whole concept
/// out of use, and status is per concept, so without this check the next unrelated stated fact
/// about the same entity promotes it straight back and both conflicting claims reach a prompt.
/// That is the failure §9.5 says the gate prevents, arriving through the promotion path instead.
fn promote(
    concept: &mut RawConcept,
    path: &str,
    claim: &Claim,
    occurrences: u32,
    report: &mut Report,
) {
    if reconcile::promotion(claim, occurrences, false) == Promotion::Auto
        && concept.front.status == Status::Draft
        && !reconcile::has_unresolved_conflict(concept)
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

Rules:
- Only durable facts. A one-off question, a passing joke or a task instruction is not a fact.
- Give every fact an attribute, and reuse the same key for the same property every time. `employer`
  today and `works_at` tomorrow means the change of job is never noticed.
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

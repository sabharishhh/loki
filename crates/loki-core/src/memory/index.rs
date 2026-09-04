//! The index.
//!
//! Derived and disposable (§10.6). Files are the record and this is a projection over them, so
//! losing it costs a rebuild rather than data. Nothing here is ever the source of truth.
//!
//! Two layers, deliberately. [`Corpus`] is FTS5 over an arbitrary set of documents, and [`Index`]
//! is the memory schema and the §10.1 ranking on top of it. §13.3's `search_tools` is the same
//! bounded-candidate-set rule over tool definitions, so it takes the lower layer rather than a
//! second copy of it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Mutex;

use jiff::civil::{Date, date};
use rusqlite::{Connection, OptionalExtension, params};

use super::bundle::{BundleError, Reader};
pub use super::claim::Origin;

use super::claim::Privacy;
use super::concept::Status;
use super::gate::TierScope;

/// Bumped whenever the schema changes. A mismatch wipes and rebuilds rather than migrating,
/// because the files can always produce it again.
const SCHEMA_VERSION: i64 = 10;

/// Signal weights for §10.1. They sum to one, so a score is directly comparable across queries,
/// which is what §12.6's "did memory already know" needs to threshold on.
const W_KEYWORD: f32 = 0.55;
const W_RECENCY: f32 = 0.15;
const W_USAGE: f32 = 0.15;
const W_LINK: f32 = 0.15;

/// How the keyword signal splits between the two things it measures.
///
/// bm25 alone is corpus-relative: the same good match scores differently in a store of six claims
/// and a store of six thousand, which makes it useless as the absolute threshold §12.6 needs.
/// Term coverage is corpus-independent and carries most of the weight for that reason, with bm25
/// ordering claims that cover the question equally well.
const W_COVERAGE: f32 = 0.65;
const W_BM25: f32 = 0.35;

/// Saturation constants. Each maps an unbounded signal into 0 to 1.
const KEYWORD_SCALE: f32 = 2.0;
const RECENCY_HALF_LIFE_DAYS: f32 = 180.0;
const USAGE_SCALE: f32 = 5.0;

/// How far the link walk goes before a concept counts as unrelated.
const MAX_LINK_HOPS: u32 = 2;

/// Jaro-Winkler above which two surface forms are close enough to be worth a model call.
/// Deliberately loose: blocking's job is recall, and the match call rejects what does not fit.
const NEAR_NAME: f64 = 0.88;

const EPOCH: Date = date(1970, 1, 1);

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("could not open the index: {0}")]
    Open(#[source] rusqlite::Error),
    #[error("could not read the index: {0}")]
    Read(#[source] rusqlite::Error),
    #[error("could not write to the index: {0}")]
    Write(#[source] rusqlite::Error),
    #[error("could not read the bundle: {0}")]
    Bundle(#[from] BundleError),
    #[error("the index lock was poisoned")]
    Poisoned,
}

/// What a sync did. Useful for the import progress in §11.3 and for asserting in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    pub indexed: usize,
    pub removed: usize,
    pub unchanged: usize,
}

/// Which concepts a query may see.
///
/// Import (§11.4) writes everything as `draft`, and its review screen has to search exactly what
/// the prompt path must never see. So drafts are always indexed and filtered here, never omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    /// Stable, unexpired, still believed, currently true. What pre-fetch gets.
    #[default]
    PromptEligible,
    /// Everything on disk, drafts and deprecated included. For review and deliberate exploration.
    Everything,
}

/// A recall request.
#[derive(Debug, Clone)]
pub struct Query<'a> {
    pub text: &'a str,
    /// Precision over recall (§10.1). A wrong memory costs more than a missing one.
    pub limit: usize,
    /// Concept paths already in context, for the link-distance signal.
    pub context: &'a [String],
    pub scope: TierScope,
    pub visibility: Visibility,
    pub today: Date,
    /// The session in progress, if its own turns should be searched too (D-043).
    pub session: Option<Session<'a>>,
}

/// Which turns of the live session recall may see.
///
/// Only what has left the window. Turns still in the prompt are already there, so retrieving them
/// is waste and it breaks the §8.1 cache the frozen prefix exists to protect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session<'a> {
    pub id: &'a str,
    /// The first ordinal still inside the live window. Turns below it are eligible.
    pub window_starts_at: u32,
}

impl<'a> Query<'a> {
    /// A pre-fetch query: prompt-eligible only, normal claims, no context.
    #[must_use]
    pub const fn prefetch(text: &'a str, scope: TierScope, today: Date, limit: usize) -> Self {
        Self {
            text,
            limit,
            context: &[],
            scope,
            visibility: Visibility::PromptEligible,
            today,
            session: None,
        }
    }

    /// Also searches the live session's turns that have fallen out of the window.
    #[must_use]
    pub const fn spanning(mut self, session: Session<'a>) -> Self {
        self.session = Some(session);
        self
    }
}

/// How well a claim answered a query, from 0 to 1.
///
/// Absolute rather than normalised across the result set, so a caller can ask whether the best
/// hit is good enough at all. Phase 5 turns on that distinction.
#[derive(Debug, Clone, Copy, Default, PartialEq, PartialOrd)]
pub struct Score(f32);

impl Score {
    /// Clamped to 0 to 1, because the whole point is that a caller can read it against a
    /// threshold. A score outside the range would make every threshold meaningless.
    #[must_use]
    pub fn new(raw: f32) -> Self {
        Self(raw.clamp(0.0, 1.0))
    }

    #[must_use]
    pub const fn value(self) -> f32 {
        self.0
    }
}

/// Which of §9.2's layers a recalled line came from.
///
/// One union, two layers (D-043). The user never sees the difference, but the caller does: only a
/// consolidated claim has a file to record a use against, and only one can be corrected.
///
/// Named `Layer` rather than `Origin` because §9.12 took that word for where a *claim* came from,
/// and two different concepts under one name in one module tree is how the wrong one gets
/// imported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// A consolidated claim, from any session.
    Consolidated,
    /// A raw turn from the session in progress, not yet consolidated.
    Live,
}

/// One retrieved claim, with everything a caller needs to cite it or record its use.
#[derive(Debug, Clone, PartialEq)]
pub struct Recalled {
    pub layer: Layer,
    pub path: String,
    pub name: String,
    pub heading: String,
    /// Position within the concept, counting all sections. Addresses the claim for [`Index::record_use`].
    pub ordinal: u32,
    pub text: String,
    pub status: Status,
    pub privacy: Privacy,
    /// Where the claim came from (§9.12). A live turn is always `stated`: the user typed it.
    pub origin: Origin,
    /// World time, when the source gave one. What §10.9's rendered distance is measured from.
    pub valid_from: Option<Date>,
    pub score: Score,
}

impl Recalled {
    #[must_use]
    pub fn reference(&self) -> Use {
        Use {
            path: self.path.clone(),
            ordinal: self.ordinal,
        }
    }
}

pub use crate::core::vocab::Lane;

/// Names one claim inside one concept.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Use {
    pub path: String,
    pub ordinal: u32,
}

/// Why a candidate was surfaced by blocking. Ordered strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Blocking {
    ExactName,
    Alias,
    NearName,
    SharedTags,
}

impl Blocking {
    const fn rank(self) -> u8 {
        match self {
            Self::ExactName => 0,
            Self::Alias => 1,
            Self::NearName => 2,
            Self::SharedTags => 3,
        }
    }
}

/// An entity a claim might belong to, surfaced without a model call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: String,
    pub name: String,
    pub why: Blocking,
    /// `people`, `projects` or `preferences`, from the directory the file sits in.
    ///
    /// Evidence about identity, never a partition (§9.4). A person and a project sharing a name is
    /// weak evidence they are different things, not proof, and filtering on it would make the
    /// same entity extracted twice under two kinds into two files that can never meet.
    pub kind: String,
    /// A few things already believed about this candidate, for the match call.
    ///
    /// Without these the matcher is asked whether two identical strings are the same person, which
    /// has no answer. Given "on the design team" against "runs infra" it has one.
    pub facts: Vec<String>,
}

/// Believed claims shown to the matcher per candidate.
///
/// Enough to tell two people apart and few enough that five candidates stay a bounded prompt.
const FACTS_PER_CANDIDATE: usize = 3;

/// Adds matching turns from the live session to the results.
///
/// Scored on keyword coverage alone. A turn has no usage count, no links, and its recency is the
/// conversation's own order rather than a date, so the other three signals have nothing to say.
fn recall_turns(
    db: &Connection,
    query: &Query<'_>,
    session: Session<'_>,
    query_terms: &[String],
    out: &mut Vec<Recalled>,
) -> Result<(), IndexError> {
    let candidates = TURNS_FTS
        .search(db, query.text, query.limit.saturating_mul(4).max(32))
        .map_err(IndexError::Read)?;
    if candidates.is_empty() {
        return Ok(());
    }

    let mut stmt = db
        .prepare("SELECT session, ordinal, speaker, text FROM turn WHERE id = ?1")
        .map_err(IndexError::Read)?;

    for (rowid, bm25) in candidates {
        let row = stmt
            .query_row(params![rowid], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u32>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .optional()
            .map_err(IndexError::Read)?;
        let Some((id, ordinal, speaker, text)) = row else {
            continue;
        };
        if id != session.id || ordinal >= session.window_starts_at {
            continue;
        }
        // Same keyword signal as a claim, and only that signal, so the two are comparable where
        // they overlap without a turn borrowing standing it has not earned.
        #[allow(clippy::cast_possible_truncation)]
        let strength = 1.0 - (bm25 as f32 / KEYWORD_SCALE).exp().min(1.0);
        let covered = coverage(query_terms, &text, &speaker);
        let keyword = W_COVERAGE * covered.clamp(0.0, 1.0) + W_BM25 * strength;
        out.push(Recalled {
            layer: Layer::Live,
            origin: Origin::Stated,
            // A turn from this session is something said just now. A distance on it would read as
            // noise against a frame that already says what "now" is.
            valid_from: None,
            path: format!("{id}#{ordinal}"),
            name: speaker,
            heading: String::new(),
            ordinal,
            text,
            status: Status::Stable,
            privacy: Privacy::Normal,
            score: Score((W_KEYWORD * keyword).clamp(0.0, 1.0)),
        });
    }
    Ok(())
}

/// Keeps the strongest reason a candidate was surfaced, since one entity can match several ways.
fn promote(best: &mut HashMap<String, Candidate>, path: String, name: String, why: Blocking) {
    best.entry(path.clone())
        .and_modify(|c| {
            if why.rank() < c.why.rank() {
                c.why = why;
            }
        })
        .or_insert_with(|| Candidate {
            kind: path
                .split_once('/')
                .map_or_else(String::new, |(dir, _)| dir.to_owned()),
            path,
            name,
            why,
            facts: Vec::new(),
        });
}

/// Uses recorded since the last flush, for consolidation to fold back into the files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingUse {
    pub path: String,
    pub ordinal: u32,
    pub uses: u32,
    /// How often retrieval returned this claim, on either lane (§10.6).
    pub recalls: u32,
    /// How many distinct queries returned it. Breadth, not volume.
    pub recall_queries: u32,
    /// How many distinct days it was returned on. Recurrence, not a busy afternoon.
    pub recall_days: u32,
}

/// FTS5 over one named table of documents, ranked by bm25.
///
/// `table` is a compile-time constant and never caller-supplied, which is what makes the
/// interpolation below safe.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Corpus {
    table: &'static str,
}

impl Corpus {
    pub(crate) const fn new(table: &'static str) -> Self {
        Self { table }
    }

    pub(crate) fn create(self, db: &Connection) -> rusqlite::Result<()> {
        db.execute_batch(&format!(
            // Porter stemming, which §10.5 names as the first of the cheap wins that come before
            // any semantic index. It is what makes "replies" find "reply" and "lives" find
            // "lived", and it costs one word in a schema rather than an embedding model.
            //
            // It does not close the semantic gap: "what did I study" still will not find "is a
            // computer science graduate", because those share no word to stem. §10.5 puts that on
            // a local embedding index, after two failed keyword rounds.
            "CREATE VIRTUAL TABLE IF NOT EXISTS {} USING fts5(title, body, tokenize = 'porter unicode61');",
            self.table
        ))
    }

    pub(crate) fn put(
        self,
        db: &Connection,
        rowid: i64,
        title: &str,
        body: &str,
    ) -> rusqlite::Result<()> {
        db.execute(
            &format!("DELETE FROM {} WHERE rowid = ?1", self.table),
            params![rowid],
        )?;
        db.execute(
            &format!(
                "INSERT INTO {}(rowid, title, body) VALUES (?1, ?2, ?3)",
                self.table
            ),
            params![rowid, title, body],
        )?;
        Ok(())
    }

    pub(crate) fn remove(self, db: &Connection, rowid: i64) -> rusqlite::Result<()> {
        db.execute(
            &format!("DELETE FROM {} WHERE rowid = ?1", self.table),
            params![rowid],
        )?;
        Ok(())
    }

    /// Rowids matching any term, with their bm25. More negative is a better match.
    pub(crate) fn search(
        self,
        db: &Connection,
        query: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<(i64, f64)>> {
        let Some(match_expr) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let sql = format!(
            "SELECT rowid, bm25({0}) FROM {0} WHERE {0} MATCH ?1 ORDER BY bm25({0}) LIMIT ?2",
            self.table
        );
        let mut stmt = db.prepare(&sql)?;
        let rows = stmt.query_map(params![match_expr, limit as i64], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
        })?;
        rows.collect()
    }
}

const CLAIMS_FTS: Corpus = Corpus::new("claim_fts");
/// The live session's turns. A second corpus rather than a second index, so recall reads one
/// union and the caller never has to know which side answered (D-043).
const TURNS_FTS: Corpus = Corpus::new("turn_fts");

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS concept (
    id          INTEGER PRIMARY KEY,
    path        TEXT    NOT NULL UNIQUE,
    name        TEXT    NOT NULL,
    status      TEXT    NOT NULL,
    verified    INTEGER NOT NULL DEFAULT 0,
    described   INTEGER NOT NULL DEFAULT 0,
    merged_into TEXT,
    stale_after INTEGER,
    mtime       INTEGER NOT NULL,
    len         INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS turn (
    id       INTEGER PRIMARY KEY,
    session  TEXT    NOT NULL,
    ordinal  INTEGER NOT NULL,
    speaker  TEXT    NOT NULL,
    text     TEXT    NOT NULL,
    UNIQUE(session, ordinal)
);
CREATE TABLE IF NOT EXISTS claim (
    id          INTEGER PRIMARY KEY,
    concept     INTEGER NOT NULL REFERENCES concept(id) ON DELETE CASCADE,
    ordinal     INTEGER NOT NULL,
    heading     TEXT    NOT NULL,
    text        TEXT    NOT NULL,
    privacy     TEXT    NOT NULL,
    origin      TEXT    NOT NULL DEFAULT 'inferred',
    shadowed    INTEGER NOT NULL DEFAULT 0,
    valid_from  INTEGER,
    valid_to    INTEGER,
    learned     INTEGER NOT NULL,
    unlearned   INTEGER,
    usage_count INTEGER NOT NULL DEFAULT 0,
    uses_pending INTEGER NOT NULL DEFAULT 0,
    UNIQUE (concept, ordinal)
);
CREATE TABLE IF NOT EXISTS link (
    src TEXT NOT NULL,
    dst TEXT NOT NULL,
    PRIMARY KEY (src, dst)
);
CREATE TABLE IF NOT EXISTS relation (
    src   TEXT NOT NULL,
    label TEXT NOT NULL,
    dst   TEXT NOT NULL,
    until INTEGER,
    PRIMARY KEY (src, label, dst)
);
CREATE TABLE IF NOT EXISTS alias (
    concept INTEGER NOT NULL REFERENCES concept(id) ON DELETE CASCADE,
    text    TEXT    NOT NULL,
    PRIMARY KEY (concept, text)
);
CREATE TABLE IF NOT EXISTS tag (
    concept INTEGER NOT NULL REFERENCES concept(id) ON DELETE CASCADE,
    text    TEXT    NOT NULL,
    PRIMARY KEY (concept, text)
);
CREATE TABLE IF NOT EXISTS recall_event (
    concept    TEXT    NOT NULL,
    ordinal    INTEGER NOT NULL,
    query_hash TEXT    NOT NULL,
    day        INTEGER NOT NULL,
    lane       TEXT    NOT NULL,
    rank       INTEGER NOT NULL,
    at         INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS recall_by_claim ON recall_event(concept, ordinal);
CREATE INDEX IF NOT EXISTS recall_by_day ON recall_event(day);
CREATE INDEX IF NOT EXISTS claim_by_concept ON claim(concept);
CREATE INDEX IF NOT EXISTS link_by_src ON link(src);
CREATE INDEX IF NOT EXISTS link_by_dst ON link(dst);
CREATE INDEX IF NOT EXISTS relation_by_src ON relation(src, label);
CREATE INDEX IF NOT EXISTS alias_by_text ON alias(text);
CREATE INDEX IF NOT EXISTS tag_by_text ON tag(text);
";

/// The ranked projection of the bundle.
///
/// Open it once and keep it. [`Index::sync`] brings it level with the files and is cheap enough
/// to call on every turn.
#[derive(Debug)]
pub struct Index {
    db: Mutex<Connection>,
}

impl Index {
    /// Opens or creates the index at `path`.
    ///
    /// A schema-version mismatch wipes it, since §10.6 makes rebuilding cheaper than migrating.
    ///
    /// # Errors
    /// Fails if the file cannot be opened or the schema cannot be applied.
    pub fn open(path: &Path) -> Result<Self, IndexError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let db = Connection::open(path).map_err(IndexError::Open)?;
        Self::prepare(&db)?;
        Ok(Self { db: Mutex::new(db) })
    }

    /// An index that never touches disk. For tests and for the import dry run.
    ///
    /// # Errors
    /// Fails if the schema cannot be applied.
    pub fn in_memory() -> Result<Self, IndexError> {
        let db = Connection::open_in_memory().map_err(IndexError::Open)?;
        Self::prepare(&db)?;
        Ok(Self { db: Mutex::new(db) })
    }

    fn prepare(db: &Connection) -> Result<(), IndexError> {
        db.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(IndexError::Open)?;
        let found: Option<i64> = db
            .query_row("SELECT value FROM meta WHERE key = 'schema'", [], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .unwrap_or(None)
            .and_then(|v| v.parse().ok());
        if found.is_some_and(|v| v != SCHEMA_VERSION) {
            db.execute_batch(
                // Every table, including the live-session ones. A bump that leaves a table
                // behind silently keeps its old shape, which is the failure a version exists to
                // prevent, arriving through the fix for it.
                "DROP TABLE IF EXISTS claim;
                 DROP TABLE IF EXISTS concept;
                 DROP TABLE IF EXISTS link;
                 DROP TABLE IF EXISTS relation;
                 DROP TABLE IF EXISTS alias;
                 DROP TABLE IF EXISTS tag;
                 DROP TABLE IF EXISTS turn;
                 DROP TABLE IF EXISTS recall_event;
                 DROP TABLE IF EXISTS claim_fts;
                 DROP TABLE IF EXISTS turn_fts;
                 DROP TABLE IF EXISTS meta;",
            )
            .map_err(IndexError::Write)?;
        }
        db.execute_batch(SCHEMA).map_err(IndexError::Open)?;
        CLAIMS_FTS.create(db).map_err(IndexError::Open)?;
        TURNS_FTS.create(db).map_err(IndexError::Open)?;
        db.execute(
            "INSERT INTO meta(key, value) VALUES ('schema', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SCHEMA_VERSION.to_string()],
        )
        .map_err(IndexError::Write)?;
        Ok(())
    }

    /// Brings the index level with the files, re-reading only what changed.
    ///
    /// Idempotent per concept, so a paused and resumed import (§11.5) and a revert writing files
    /// underneath the index (§14.3) both land cleanly without a full rebuild.
    ///
    /// # Errors
    /// Fails if the bundle cannot be read or the index cannot be written.
    pub fn sync(&self, reader: &Reader<'_>) -> Result<Stats, IndexError> {
        let mut db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        let tx = db.transaction().map_err(IndexError::Write)?;

        // `concepts` already covers drafts. §10.6: candidates are indexed and filtered at query
        // time, never omitted, or the review screen and recall-driven promotion both become
        // impossible.
        let on_disk = reader.concepts()?;

        let mut known: HashMap<String, (i64, i64)> = HashMap::new();
        {
            let mut stmt = tx
                .prepare("SELECT path, mtime, len FROM concept")
                .map_err(IndexError::Read)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })
                .map_err(IndexError::Read)?;
            for row in rows {
                let (path, mtime, len) = row.map_err(IndexError::Read)?;
                known.insert(path, (mtime, len));
            }
        }

        let mut stats = Stats::default();
        let present: HashSet<&String> = on_disk.iter().collect();

        for path in &on_disk {
            let text = match reader.read(path) {
                Ok(text) => text,
                // A file that vanished between listing and reading is simply not indexed.
                Err(_) => continue,
            };
            let stamp = file_stamp(reader.root(), path);
            let len = i64::try_from(text.len()).unwrap_or(i64::MAX);
            if known
                .get(path)
                .is_some_and(|&(m, l)| m == stamp && l == len)
            {
                stats.unchanged += 1;
                continue;
            }
            let Ok(concept) = reader.load_concept(path) else {
                // Unparseable concepts are skipped rather than failing the whole sync. OKF
                // conformance says a consumer tolerates what it does not understand.
                continue;
            };
            put_concept(&tx, path, &concept, &text, stamp, len)?;
            stats.indexed += 1;
        }

        let stale: Vec<String> = known
            .keys()
            .filter(|p| !present.contains(*p))
            .cloned()
            .collect();
        for path in stale {
            remove_concept(&tx, &path)?;
            stats.removed += 1;
        }

        tx.commit().map_err(IndexError::Write)?;
        Ok(stats)
    }

    /// Wipes the index and builds it from the files.
    ///
    /// # Errors
    /// Fails if the bundle cannot be read or the index cannot be written.
    pub fn rebuild(&self, reader: &Reader<'_>) -> Result<Stats, IndexError> {
        {
            let db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
            db.execute_batch(
                "DELETE FROM claim; DELETE FROM alias; DELETE FROM tag;
                 DELETE FROM link; DELETE FROM concept; DELETE FROM claim_fts;",
            )
            .map_err(IndexError::Write)?;
        }
        self.sync(reader)
    }

    /// Ranked claims for a query, best first.
    ///
    /// Ranking is §10.1: keyword match, recency, usage count, and link distance from what is
    /// already in context. **Every candidate is scored before anything is cut**, because the cap
    /// is on what reaches the prompt and not on what the ranking may consider.
    ///
    /// # Errors
    /// Fails if the index cannot be read.
    pub fn recall(&self, query: &Query<'_>) -> Result<Vec<Recalled>, IndexError> {
        let db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        // Over-fetch, because the visibility and privacy filters below reject candidates and a
        // capped result set must still be full when it can be.
        let mut candidates = CLAIMS_FTS
            .search(&db, query.text, query.limit.saturating_mul(8).max(64))
            .map_err(IndexError::Read)?;

        let distances = link_distances(&db, query.context)?;
        let query_terms = terms(query.text);

        // Half the store was unsearchable. An entity's names and the edges pointing at it are the
        // only place a word like "father" or a nickname appears, and lane 1 ranked claim text
        // alone, so "my father" found nothing about a card that knew perfectly well whose father
        // it was. These join the candidate set with no bm25 of their own: a form match is a real
        // signal but a weaker one than the words of the claim itself.
        let forms = matched_forms(&db, &query_terms)?;
        if !forms.is_empty() {
            let seen: HashSet<i64> = candidates.iter().map(|(id, _)| *id).collect();
            for id in claims_of(&db, forms.keys())? {
                if !seen.contains(&id) {
                    candidates.push((id, 0.0));
                }
            }
        }
        let today = to_days(query.today);
        let mut out = Vec::with_capacity(candidates.len());

        let mut stmt = db
            .prepare(
                "SELECT c.path, c.name, c.status, c.stale_after,
                        m.heading, m.ordinal, m.text, m.privacy, m.origin, m.shadowed,
                        m.learned, m.valid_from, m.valid_to, m.unlearned,
                        m.usage_count + m.uses_pending
                 FROM claim m JOIN concept c ON c.id = m.concept
                 WHERE m.id = ?1",
            )
            .map_err(IndexError::Read)?;

        for (rowid, bm25) in candidates {
            let row = stmt
                .query_row(params![rowid], |r| {
                    Ok(Row {
                        path: r.get(0)?,
                        name: r.get(1)?,
                        status: r.get::<_, String>(2)?,
                        stale_after: r.get(3)?,
                        heading: r.get(4)?,
                        ordinal: r.get(5)?,
                        text: r.get(6)?,
                        privacy: r.get::<_, String>(7)?,
                        origin: r.get::<_, String>(8)?,
                        shadowed: r.get::<_, i64>(9)? != 0,
                        learned: r.get(10)?,
                        valid_from: r.get(11)?,
                        valid_to: r.get(12)?,
                        unlearned: r.get(13)?,
                        uses: r.get(14)?,
                    })
                })
                .optional()
                .map_err(IndexError::Read)?;
            let Some(row) = row else { continue };

            let status = parse_status(&row.status);
            let privacy = parse_privacy(&row.privacy);
            let origin = parse_origin(&row.origin);
            if !query.scope.admits(privacy) || !query.scope.admits_origin(origin) {
                continue;
            }
            if query.visibility == Visibility::PromptEligible && !row.is_eligible(status, today) {
                continue;
            }

            let hops = distances.get(&row.path).copied();
            let form = forms.get(&row.path).map_or("", String::as_str);
            let covered = coverage(&query_terms, &row.text, &format!("{} {form}", row.name));
            let score = combine(bm25, covered, today - row.learned, row.uses, hops);
            out.push(Recalled {
                layer: Layer::Consolidated,
                path: row.path,
                name: row.name,
                heading: row.heading,
                ordinal: row.ordinal,
                text: row.text,
                status,
                privacy,
                origin,
                valid_from: row.valid_from.map(from_days),
                score,
            });
        }

        if let Some(session) = query.session {
            recall_turns(&db, query, session, &query_terms, &mut out)?;
        }

        out.sort_by(|a, b| {
            b.score
                .0
                .total_cmp(&a.score.0)
                .then_with(|| a.path.cmp(&b.path))
        });
        out.truncate(query.limit);
        Ok(out)
    }

    /// Records a turn of the session in progress, so it is retrievable before consolidation runs.
    ///
    /// An FTS insert, no model call. §8.1 says memory writes apply to the next session, which
    /// leaves a long session unable to recall its own beginning; this is what closes that
    /// (D-043). Re-recording the same ordinal replaces it rather than duplicating.
    ///
    /// # Errors
    /// Fails if the index cannot be written.
    pub fn record_turn(
        &self,
        session: &str,
        ordinal: u32,
        speaker: &str,
        text: &str,
    ) -> Result<(), IndexError> {
        let mut db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        let tx = db.transaction().map_err(IndexError::Write)?;
        {
            let existing: Option<i64> = tx
                .query_row(
                    "SELECT id FROM turn WHERE session = ?1 AND ordinal = ?2",
                    params![session, ordinal],
                    |r| r.get(0),
                )
                .optional()
                .map_err(IndexError::Read)?;
            if let Some(id) = existing {
                TURNS_FTS.remove(&tx, id).map_err(IndexError::Write)?;
                tx.execute("DELETE FROM turn WHERE id = ?1", params![id])
                    .map_err(IndexError::Write)?;
            }
            tx.execute(
                "INSERT INTO turn(session, ordinal, speaker, text) VALUES (?1, ?2, ?3, ?4)",
                params![session, ordinal, speaker, text],
            )
            .map_err(IndexError::Write)?;
            let id = tx.last_insert_rowid();
            TURNS_FTS
                .put(&tx, id, speaker, text)
                .map_err(IndexError::Write)?;
        }
        tx.commit().map_err(IndexError::Write)
    }

    /// Drops a session's turns once it has been consolidated into claims.
    ///
    /// The claims are the durable record from then on, and §9.2 keeps raw past turns out of the
    /// automatic corpus (D-045). Deliberate `mem_grep` still reaches the episode file.
    ///
    /// # Errors
    /// Fails if the index cannot be written.
    pub fn forget_session(&self, session: &str) -> Result<(), IndexError> {
        let mut db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        let tx = db.transaction().map_err(IndexError::Write)?;
        {
            let mut stmt = tx
                .prepare("SELECT id FROM turn WHERE session = ?1")
                .map_err(IndexError::Read)?;
            let ids: Vec<i64> = stmt
                .query_map(params![session], |r| r.get(0))
                .map_err(IndexError::Read)?
                .collect::<rusqlite::Result<_>>()
                .map_err(IndexError::Read)?;
            drop(stmt);
            for id in ids {
                TURNS_FTS.remove(&tx, id).map_err(IndexError::Write)?;
            }
            tx.execute("DELETE FROM turn WHERE session = ?1", params![session])
                .map_err(IndexError::Write)?;
        }
        tx.commit().map_err(IndexError::Write)
    }

    /// Records that claims were retrieved and used.
    ///
    /// Counts land here first and are folded into the files by consolidation, because writing a
    /// file per retrieval would put a git commit on the hot path. §9.9 and §9.10 read the result.
    ///
    /// # Errors
    /// Fails if the index cannot be written.
    /// Records that retrieval returned these claims (§10.6).
    ///
    /// One row per claim per turn. The three counts §9.8 promotes on are derived from these rows
    /// rather than incremented, because "how many distinct queries" and "how many distinct days"
    /// cannot be answered by a counter.
    ///
    /// # Errors
    /// Fails if the index cannot be written.
    pub fn record_recall(
        &self,
        returned: &[Recalled],
        query: &str,
        day: Date,
        lane: Lane,
    ) -> Result<(), IndexError> {
        let hash = query_hash(query);
        let day = to_days(day);
        let at = now_seconds();
        let mut db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        let tx = db.transaction().map_err(IndexError::Write)?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO recall_event(concept, ordinal, query_hash, day, lane, rank, at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .map_err(IndexError::Write)?;
            for (rank, line) in returned.iter().enumerate() {
                // A live turn has no file to count against, and counting it would inflate the very
                // corpus that is about to be consolidated away.
                if line.layer != Layer::Consolidated {
                    continue;
                }
                stmt.execute(params![
                    line.path,
                    line.ordinal,
                    hash,
                    day,
                    lane.name(),
                    i64::try_from(rank).unwrap_or(i64::MAX),
                    at
                ])
                .map_err(IndexError::Write)?;
            }
        }
        tx.commit().map_err(IndexError::Write)?;
        Ok(())
    }

    /// Drops recall rows older than the promotion window (§10.6, §26 question 17).
    ///
    /// The rows are disposable working data; the counts folded into the files are the record. A
    /// claim heavily used last year and untouched since should not still be promoting things.
    ///
    /// # Errors
    /// Fails if the index cannot be written.
    pub fn prune_recalls(&self, today: Date, keep_days: i64) -> Result<usize, IndexError> {
        let cutoff = to_days(today) - keep_days;
        let db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        db.execute("DELETE FROM recall_event WHERE day < ?1", params![cutoff])
            .map_err(IndexError::Write)
    }

    pub fn record_use(&self, uses: &[Use]) -> Result<(), IndexError> {
        let mut db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        let tx = db.transaction().map_err(IndexError::Write)?;
        {
            let mut stmt = tx
                .prepare(
                    "UPDATE claim SET uses_pending = uses_pending + 1
                     WHERE ordinal = ?2
                       AND concept = (SELECT id FROM concept WHERE path = ?1)",
                )
                .map_err(IndexError::Write)?;
            for use_ in uses {
                stmt.execute(params![use_.path, use_.ordinal])
                    .map_err(IndexError::Write)?;
            }
        }
        tx.commit().map_err(IndexError::Write)?;
        Ok(())
    }

    /// Takes the uses recorded since the last call, for consolidation to write into the files.
    ///
    /// Clearing here means a caller that fails to persist them loses them. That is the same
    /// exposure as the index being deleted, which §10.6 already accepts.
    ///
    /// # Errors
    /// Fails if the index cannot be read or written.
    pub fn drain_pending_uses(&self) -> Result<Vec<PendingUse>, IndexError> {
        let mut db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        let tx = db.transaction().map_err(IndexError::Write)?;
        let pending: Vec<PendingUse> = {
            // The recall aggregates ride along with the uses, because both are folded into the
            // same file on the same pass. §9.13: the counts live in the record, so an index
            // rebuild loses the rows and keeps the signal.
            let mut stmt = tx
                .prepare(
                    "SELECT c.path, m.ordinal, m.uses_pending,
                            (SELECT COUNT(*) FROM recall_event r
                              WHERE r.concept = c.path AND r.ordinal = m.ordinal),
                            (SELECT COUNT(DISTINCT r.query_hash) FROM recall_event r
                              WHERE r.concept = c.path AND r.ordinal = m.ordinal),
                            (SELECT COUNT(DISTINCT r.day) FROM recall_event r
                              WHERE r.concept = c.path AND r.ordinal = m.ordinal)
                     FROM claim m JOIN concept c ON c.id = m.concept
                     WHERE m.uses_pending > 0
                        OR EXISTS (SELECT 1 FROM recall_event r
                                    WHERE r.concept = c.path AND r.ordinal = m.ordinal)
                     ORDER BY c.path, m.ordinal",
                )
                .map_err(IndexError::Read)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(PendingUse {
                        path: r.get(0)?,
                        ordinal: r.get(1)?,
                        uses: r.get(2)?,
                        recalls: r.get(3)?,
                        recall_queries: r.get(4)?,
                        recall_days: r.get(5)?,
                    })
                })
                .map_err(IndexError::Read)?;
            rows.collect::<rusqlite::Result<_>>()
                .map_err(IndexError::Read)?
        };
        tx.execute(
            "UPDATE claim SET usage_count = usage_count + uses_pending, uses_pending = 0
             WHERE uses_pending > 0",
            [],
        )
        .map_err(IndexError::Write)?;
        tx.commit().map_err(IndexError::Write)?;
        Ok(pending)
    }

    /// How many claims are indexed. For tests and the Activity screen.
    ///
    /// Concept paths ordered by how much they are actually used, most first.
    ///
    /// What the working set is built from: the cap has to fall on what you rely on least, not on
    /// whatever the filesystem happened to list last.
    ///
    /// # Errors
    /// Fails if the index cannot be read.
    pub fn most_used(&self, limit: usize) -> Result<Vec<String>, IndexError> {
        let db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        let mut stmt = db
            .prepare(
                "SELECT c.path, COALESCE(SUM(m.usage_count + m.uses_pending), 0) AS uses
                 FROM concept c LEFT JOIN claim m ON m.concept = c.id
                 WHERE c.status = 'stable'
                 GROUP BY c.id
                 ORDER BY uses DESC, c.mtime DESC, c.path
                 LIMIT ?1",
            )
            .map_err(IndexError::Read)?;
        let rows = stmt
            .query_map(params![limit as i64], |r| r.get::<_, String>(0))
            .map_err(IndexError::Read)?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(IndexError::Read)
    }

    /// The blocking step of §9.4: cheap filtering to at most `limit` candidates, no model call.
    ///
    /// Four signals, strongest first: exact name, alias, near name by normalized distance, and
    /// shared tags. An empty result means the entity is new, which is the case that costs nothing.
    ///
    /// # Errors
    /// Fails if the index cannot be read.
    /// The single concept that answers to this exact surface form, by name or alias.
    ///
    /// Exact only, and only when there is one. This resolves the *whose* half of a descriptor like
    /// "the user's sister", which then decides where a claim is written, so a near match would put
    /// a fact on the wrong person's edge. Blocking is where fuzziness belongs.
    ///
    /// # Errors
    /// Fails if the index cannot be read.
    pub fn path_answering_to(&self, surface: &str) -> Result<Option<String>, IndexError> {
        let db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        let needle = surface.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(None);
        }
        let mut stmt = db
            .prepare(
                "SELECT DISTINCT c.path FROM alias a JOIN concept c ON c.id = a.concept
                 WHERE a.text = ?1 LIMIT 2",
            )
            .map_err(IndexError::Read)?;
        let mut paths: Vec<String> = stmt
            .query_map(params![needle], |r| r.get::<_, String>(0))
            .map_err(IndexError::Read)?
            .collect::<rusqlite::Result<_>>()
            .map_err(IndexError::Read)?;
        // Two entities answering to one form is §9.4's known ambiguity, not something to pick
        // between. The caller falls back to blocking, which surfaces it properly.
        Ok((paths.len() == 1).then(|| paths.remove(0)))
    }

    /// Whether this concept's name is a placeholder rather than a name (§9.4, S-21).
    ///
    /// What decides whether a named entity may absorb a card through an edge. Absorbing a
    /// placeholder is a merge nobody loses anything by; absorbing a named card would make a second
    /// brother into the first one.
    ///
    /// # Errors
    /// Fails if the index cannot be read.
    pub fn describes(&self, path: &str) -> Result<bool, IndexError> {
        let db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        Ok(db
            .query_row(
                "SELECT described FROM concept WHERE path = ?1",
                params![path],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(IndexError::Read)?
            == Some(1))
    }

    /// The current target of `src`'s `label` edge, when there is exactly one (§9.4, S-21).
    ///
    /// Closed edges are excluded and two live targets return nothing: "who is my brother" has no
    /// singular answer when there are two, and guessing between them is worse than not answering.
    ///
    /// # Errors
    /// Fails if the index cannot be read.
    pub fn related(&self, src: &str, label: &str) -> Result<Option<String>, IndexError> {
        let db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        let mut stmt = db
            .prepare(
                "SELECT dst FROM relation WHERE src = ?1 AND label = ?2 AND until IS NULL LIMIT 2",
            )
            .map_err(IndexError::Read)?;
        let mut found: Vec<String> = stmt
            .query_map(params![src, label.trim().to_lowercase()], |r| {
                r.get::<_, String>(0)
            })
            .map_err(IndexError::Read)?
            .collect::<rusqlite::Result<_>>()
            .map_err(IndexError::Read)?;
        Ok((found.len() == 1).then(|| found.remove(0)))
    }

    /// The card a tombstone merged into, if this path is one (§9.4).
    ///
    /// One hop. [`super::resolve`] walks the chain, because it is the caller that knows how far a
    /// pointer may be followed before the store is simply malformed.
    ///
    /// # Errors
    /// Fails if the index cannot be read.
    pub fn merged_into(&self, path: &str) -> Result<Option<String>, IndexError> {
        let db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        db.query_row(
            "SELECT merged_into FROM concept WHERE path = ?1",
            params![path],
            |r| r.get::<_, Option<String>>(0),
        )
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(IndexError::Read(other)),
        })
    }

    pub fn candidates(
        &self,
        surface: &str,
        tags: &[String],
        limit: usize,
    ) -> Result<Vec<Candidate>, IndexError> {
        let db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        let needle = surface.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let wanted: Vec<String> = tags.iter().map(|t| t.to_lowercase()).collect();

        let mut best: HashMap<String, Candidate> = HashMap::new();
        // **A tombstone is never a candidate.** Its names belong to whatever it merged into, and a
        // claim written onto it is invisible in both directions: the §10.4 gate keeps a deprecated
        // concept out of every prompt, and §17.3's duplicate list skips it, so nothing can surface
        // the fact and nothing can repair it. Four of Sabharish's facts landed there because the
        // tombstone's name was the literal surface form and outranked the card it had merged into.
        // B-54.
        let mut stmt = db
            .prepare(
                "SELECT c.path, c.name, a.text FROM alias a JOIN concept c ON c.id = a.concept
                 WHERE c.merged_into IS NULL",
            )
            .map_err(IndexError::Read)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(IndexError::Read)?;

        for row in rows {
            let (path, name, form) = row.map_err(IndexError::Read)?;
            let signal = if form == needle {
                if form == name.to_lowercase() {
                    Blocking::ExactName
                } else {
                    Blocking::Alias
                }
            } else if strsim::jaro_winkler(&form, &needle) >= NEAR_NAME || shortened(&form, &needle)
            {
                Blocking::NearName
            } else {
                continue;
            };
            promote(&mut best, path, name, signal);
        }

        if !wanted.is_empty() {
            let mut stmt = db
                .prepare("SELECT c.path, c.name FROM tag t JOIN concept c ON c.id = t.concept WHERE t.text = ?1")
                .map_err(IndexError::Read)?;
            for tag in &wanted {
                let rows = stmt
                    .query_map(params![tag], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                    })
                    .map_err(IndexError::Read)?;
                for row in rows {
                    let (path, name) = row.map_err(IndexError::Read)?;
                    promote(&mut best, path, name, Blocking::SharedTags);
                }
            }
        }

        let mut out: Vec<Candidate> = best.into_values().collect();
        out.sort_by(|a, b| {
            a.why
                .rank()
                .cmp(&b.why.rank())
                .then_with(|| a.path.cmp(&b.path))
        });
        out.truncate(limit);

        // Only for what survived the cap, so the cost is a handful of rows rather than a join
        // across the store.
        let mut facts = db
            .prepare(
                "SELECT cl.text FROM claim cl JOIN concept c ON c.id = cl.concept
                 WHERE c.path = ?1 AND cl.unlearned IS NULL AND cl.shadowed = 0
                 ORDER BY cl.ordinal LIMIT ?2",
            )
            .map_err(IndexError::Read)?;
        for candidate in &mut out {
            candidate.facts = facts
                .query_map(params![candidate.path, FACTS_PER_CANDIDATE], |r| {
                    r.get::<_, String>(0)
                })
                .map_err(IndexError::Read)?
                .collect::<rusqlite::Result<_>>()
                .map_err(IndexError::Read)?;
        }
        Ok(out)
    }

    /// How many claims are indexed. For tests and the Activity screen.
    ///
    /// # Errors
    /// Fails if the index cannot be read.
    pub fn claim_count(&self) -> Result<usize, IndexError> {
        let db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        db.query_row("SELECT COUNT(*) FROM claim", [], |r| r.get::<_, i64>(0))
            .map(|n| usize::try_from(n).unwrap_or(0))
            .map_err(IndexError::Read)
    }
}

struct Row {
    path: String,
    name: String,
    status: String,
    stale_after: Option<i64>,
    heading: String,
    ordinal: u32,
    text: String,
    privacy: String,
    origin: String,
    shadowed: bool,
    learned: i64,
    valid_from: Option<i64>,
    valid_to: Option<i64>,
    unlearned: Option<i64>,
    uses: i64,
}

impl Row {
    /// The same predicate the gate applies, in SQL terms. Kept in step with
    /// [`super::gate::Active`] and [`super::claim::Validity`].
    fn is_eligible(&self, status: Status, today: i64) -> bool {
        status == Status::Stable
            && !self.shadowed
            && self.stale_after.is_none_or(|end| today < end)
            && self.unlearned.is_none()
            && self.valid_from.is_none_or(|from| today >= from)
            && self.valid_to.is_none_or(|to| today < to)
    }
}

fn put_concept(
    tx: &rusqlite::Transaction<'_>,
    path: &str,
    concept: &super::concept::RawConcept,
    text: &str,
    mtime: i64,
    len: i64,
) -> Result<(), IndexError> {
    remove_concept(tx, path)?;

    let verified = i64::from(concept.front.is_human_verified());
    let described = i64::from(concept.front.label == super::concept::Label::Described);
    tx.execute(
        "INSERT INTO concept(path, name, status, verified, described, merged_into, stale_after, mtime, len)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            path,
            concept.front.name,
            status_str(concept.front.status),
            verified,
            described,
            concept.front.merged_into,
            concept.front.stale_after.map(to_days),
            mtime,
            len
        ],
    )
    .map_err(IndexError::Write)?;
    let concept_id = tx.last_insert_rowid();

    let mut ordinal: u32 = 0;
    for section in &concept.sections {
        for claim in &section.claims {
            tx.execute(
                "INSERT INTO claim(concept, ordinal, heading, text, privacy, origin, shadowed,
                                   valid_from, valid_to, learned, unlearned, usage_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    concept_id,
                    ordinal,
                    section.heading,
                    claim.text,
                    privacy_str(claim.privacy),
                    origin_str(claim.origin),
                    // Computed here because this is where the whole concept is in hand. §9.7's
                    // rule 4 is a property of a claim relative to its siblings, and recall sees
                    // one row at a time.
                    i64::from(super::reconcile::is_shadowed(concept, ordinal)),
                    claim.validity.valid_from.map(to_days),
                    claim.validity.valid_to.map(to_days),
                    to_days(claim.validity.learned),
                    claim.validity.unlearned.map(to_days),
                    claim.usage_count
                ],
            )
            .map_err(IndexError::Write)?;
            let claim_id = tx.last_insert_rowid();
            CLAIMS_FTS
                .put(tx, claim_id, &concept.front.name, &claim.text)
                .map_err(IndexError::Write)?;
            ordinal += 1;
        }
    }

    // Both the name and its aliases are surface forms, so blocking looks up one table.
    for form in std::iter::once(&concept.front.name).chain(concept.front.aliases.iter()) {
        tx.execute(
            "INSERT OR IGNORE INTO alias(concept, text) VALUES (?1, ?2)",
            params![concept_id, form.to_lowercase()],
        )
        .map_err(IndexError::Write)?;
    }
    for tag in &concept.front.tags {
        tx.execute(
            "INSERT OR IGNORE INTO tag(concept, text) VALUES (?1, ?2)",
            params![concept_id, tag.to_lowercase()],
        )
        .map_err(IndexError::Write)?;
    }

    for target in links_in(text, path) {
        tx.execute(
            "INSERT OR IGNORE INTO link(src, dst) VALUES (?1, ?2)",
            params![path, target],
        )
        .map_err(IndexError::Write)?;
    }

    for relation in &concept.front.relations {
        tx.execute(
            "INSERT OR REPLACE INTO relation(src, label, dst, until) VALUES (?1, ?2, ?3, ?4)",
            params![
                path,
                relation.label.to_lowercase(),
                relation.to,
                relation.until.map(to_days)
            ],
        )
        .map_err(IndexError::Write)?;
        // Also an ordinary link, so §10.1's link-distance signal treats a relation the same as a
        // mention. Two people connected by an edge are related whether or not either file happens
        // to name the other in prose.
        tx.execute(
            "INSERT OR IGNORE INTO link(src, dst) VALUES (?1, ?2)",
            params![path, relation.to],
        )
        .map_err(IndexError::Write)?;
    }
    Ok(())
}

fn remove_concept(tx: &rusqlite::Transaction<'_>, path: &str) -> Result<(), IndexError> {
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM concept WHERE path = ?1",
            params![path],
            |r| r.get(0),
        )
        .optional()
        .map_err(IndexError::Read)?;
    let Some(id) = existing else { return Ok(()) };

    let claim_ids: Vec<i64> = {
        let mut stmt = tx
            .prepare("SELECT id FROM claim WHERE concept = ?1")
            .map_err(IndexError::Read)?;
        let rows = stmt
            .query_map(params![id], |r| r.get::<_, i64>(0))
            .map_err(IndexError::Read)?;
        rows.collect::<rusqlite::Result<_>>()
            .map_err(IndexError::Read)?
    };
    for claim_id in claim_ids {
        CLAIMS_FTS.remove(tx, claim_id).map_err(IndexError::Write)?;
    }
    tx.execute("DELETE FROM claim WHERE concept = ?1", params![id])
        .map_err(IndexError::Write)?;
    tx.execute("DELETE FROM alias WHERE concept = ?1", params![id])
        .map_err(IndexError::Write)?;
    tx.execute("DELETE FROM tag WHERE concept = ?1", params![id])
        .map_err(IndexError::Write)?;
    tx.execute("DELETE FROM link WHERE src = ?1", params![path])
        .map_err(IndexError::Write)?;
    tx.execute("DELETE FROM relation WHERE src = ?1", params![path])
        .map_err(IndexError::Write)?;
    tx.execute("DELETE FROM concept WHERE id = ?1", params![id])
        .map_err(IndexError::Write)?;
    Ok(())
}

/// Hops from anything already in context, walking links in both directions.
///
/// Undirected because a mention of Meera in the Loki project file relates the two whichever way
/// the link happens to point.
fn link_distances(db: &Connection, context: &[String]) -> Result<HashMap<String, u32>, IndexError> {
    let mut distances: HashMap<String, u32> = HashMap::new();
    if context.is_empty() {
        return Ok(distances);
    }
    let mut queue: VecDeque<(String, u32)> = VecDeque::new();
    for path in context {
        distances.insert(path.clone(), 0);
        queue.push_back((path.clone(), 0));
    }

    let mut stmt = db
        .prepare("SELECT dst FROM link WHERE src = ?1 UNION SELECT src FROM link WHERE dst = ?1")
        .map_err(IndexError::Read)?;

    while let Some((path, hops)) = queue.pop_front() {
        if hops == MAX_LINK_HOPS {
            continue;
        }
        let rows = stmt
            .query_map(params![path], |r| r.get::<_, String>(0))
            .map_err(IndexError::Read)?;
        for row in rows {
            let neighbour = row.map_err(IndexError::Read)?;
            if !distances.contains_key(&neighbour) {
                distances.insert(neighbour.clone(), hops + 1);
                queue.push_back((neighbour, hops + 1));
            }
        }
    }
    Ok(distances)
}

/// The §10.1 signals, each saturated into 0 to 1 and weighted.
fn combine(bm25: f64, coverage: f32, age_days: i64, uses: i64, hops: Option<u32>) -> Score {
    // bm25 is negative and unbounded, so exponentiate rather than clamp: a very strong match
    // approaches 1 without a cliff where two good matches become indistinguishable.
    #[allow(clippy::cast_possible_truncation)]
    let strength = 1.0 - (bm25 as f32 / KEYWORD_SCALE).exp().min(1.0);
    let keyword = W_COVERAGE * coverage.clamp(0.0, 1.0) + W_BM25 * strength;
    #[allow(clippy::cast_precision_loss)]
    let age = age_days.max(0) as f32;
    let recency = (-age / RECENCY_HALF_LIFE_DAYS).exp();
    #[allow(clippy::cast_precision_loss)]
    let usage = 1.0 - (-(uses.max(0) as f32) / USAGE_SCALE).exp();
    let link = match hops {
        Some(0) => 1.0,
        Some(1) => 0.6,
        Some(2) => 0.3,
        _ => 0.0,
    };
    let total = W_KEYWORD * keyword + W_RECENCY * recency + W_USAGE * usage + W_LINK * link;
    Score(total.clamp(0.0, 1.0))
}

/// Function words, which say nothing about what a question is about.
///
/// Kept short on purpose. A longer list starts discarding real terms, and coverage is only a
/// signal, not a filter.
const STOPWORDS: &[&str] = &[
    "a", "about", "am", "an", "and", "are", "as", "at", "be", "been", "but", "by", "did", "do",
    "does", "for", "from", "had", "has", "have", "he", "her", "him", "his", "how", "i", "if", "in",
    "is", "it", "its", "me", "my", "of", "on", "or", "our", "she", "so", "than", "that", "the",
    "their", "them", "then", "there", "they", "this", "to", "was", "we", "were", "what", "when",
    "where", "which", "who", "why", "will", "with", "you", "your",
];

/// The searchable terms in a piece of text: lowercased, function words dropped.
fn terms(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() > 1)
        .map(str::to_lowercase)
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .collect()
}

/// What fraction of the question's terms this claim actually contains.
///
/// The concept name counts, because "which team is Meera on" is answered by a claim in Meera's
/// file whether or not the claim text repeats her name.
fn coverage(query_terms: &[String], text: &str, forms: &str) -> f32 {
    if query_terms.is_empty() {
        return 0.0;
    }
    let haystack = format!("{} {}", text.to_lowercase(), forms.to_lowercase());
    let matched = query_terms
        .iter()
        .filter(|t| haystack.contains(t.as_str()))
        .count();
    #[allow(clippy::cast_precision_loss)]
    let fraction = matched as f32 / query_terms.len() as f32;
    fraction
}

/// Whether one surface form is the other with words dropped.
///
/// "Meera" against "Meera Raghunathan". Jaro-Winkler scores that pair around 0.85, under the
/// threshold, so a first name and a full name never met and every shortened mention wrote another
/// card. That is the split this whole area exists to prevent, arriving through the one comparison
/// that could not see it.
///
/// Whole words only, and the shorter form needs a word of three characters or more, so an initial
/// or a stray particle does not match everything. Blocking is recall: two people who really do
/// share a first name both reach the matcher, which is exactly where that question belongs.
///
/// A descriptor is excluded on both sides, because the words wrapped around a name in "the other
/// Meera" are the evidence that she is somebody else.
fn shortened(one: &str, other: &str) -> bool {
    fn words(text: &str) -> Vec<&str> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect()
    }
    // A description is never a shortened name. "The other Meera" contains "Meera" and the words
    // around it are the whole point: they are what says she is somebody else.
    if super::resolve::looks_described(one) || super::resolve::looks_described(other) {
        return false;
    }
    let (a, b) = (words(one), words(other));
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short.len() < long.len()
        && short.iter().any(|w| w.chars().count() >= 3)
        && short.iter().all(|w| long.contains(w))
}

/// Believed claims to pull in from a concept a query named rather than described.
///
/// A handful, because this is a second door into the same ranking and not a way around the cap.
const CLAIMS_PER_FORM: usize = 4;

/// Concepts a query term names outright, and the form it named them by.
///
/// Two sources, one rule: a concept answers to its aliases, and to the label of any live edge
/// pointing at it. "My father" names the card the owner's `father` edge points at, whatever that
/// card happens to be called, which is the whole reason relations are in the index.
///
/// Exact term equality, deliberately. Blocking is where fuzziness belongs; here a loose match
/// would put another person's facts in front of the model.
fn matched_forms(
    db: &Connection,
    query_terms: &[String],
) -> Result<HashMap<String, String>, IndexError> {
    let mut found: HashMap<String, String> = HashMap::new();
    if query_terms.is_empty() {
        return Ok(found);
    }

    let mut by_alias = db
        .prepare("SELECT c.path FROM alias a JOIN concept c ON c.id = a.concept WHERE a.text = ?1")
        .map_err(IndexError::Read)?;
    let mut by_edge = db
        .prepare("SELECT dst FROM relation WHERE label = ?1 AND until IS NULL")
        .map_err(IndexError::Read)?;

    for term in query_terms {
        for stmt in [&mut by_alias, &mut by_edge] {
            let rows = stmt
                .query_map(params![term], |r| r.get::<_, String>(0))
                .map_err(IndexError::Read)?;
            for path in rows {
                found
                    .entry(path.map_err(IndexError::Read)?)
                    .or_insert_with(|| term.clone());
            }
        }
    }
    Ok(found)
}

/// Claim rowids for the given concepts, live ones only.
fn claims_of<'a>(
    db: &Connection,
    paths: impl Iterator<Item = &'a String>,
) -> Result<Vec<i64>, IndexError> {
    let mut stmt = db
        .prepare(
            "SELECT m.id FROM claim m JOIN concept c ON c.id = m.concept
             WHERE c.path = ?1 AND m.unlearned IS NULL AND m.shadowed = 0
             ORDER BY m.ordinal LIMIT ?2",
        )
        .map_err(IndexError::Read)?;
    let mut out = Vec::new();
    for path in paths {
        let rows = stmt
            .query_map(params![path, CLAIMS_PER_FORM], |r| r.get::<_, i64>(0))
            .map_err(IndexError::Read)?;
        for id in rows {
            out.push(id.map_err(IndexError::Read)?);
        }
    }
    Ok(out)
}

/// Turns user text into an FTS5 MATCH expression.
///
/// Every term is quoted, so a stray `"` or `*` in a question cannot become query syntax. Terms
/// are ORed because recall wants candidates for bm25 to rank, not an all-terms filter.
fn fts_query(text: &str) -> Option<String> {
    let quoted: Vec<String> = terms(text)
        .iter()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    if quoted.is_empty() {
        return None;
    }
    Some(quoted.join(" OR "))
}

/// Cross-links out of a concept, as `[text](path.md)` and `[[name]]`.
///
/// Relative targets are resolved against the concept's own directory. A link to a file that does
/// not exist is kept: OKF says a broken cross-link is knowledge not yet written.
fn links_in(text: &str, from: &str) -> Vec<String> {
    let dir = Path::new(from).parent();
    let mut found = Vec::new();

    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '['
            && bytes.get(i + 1) == Some(&'[')
            && let Some(end) = find_from(&bytes, i + 2, "]]")
        {
            let name: String = bytes[i + 2..end].iter().collect();
            if !name.trim().is_empty() {
                found.push(name.trim().to_lowercase());
            }
            i = end + 2;
            continue;
        }
        if bytes[i] == ']'
            && bytes.get(i + 1) == Some(&'(')
            && let Some(end) = find_from(&bytes, i + 2, ")")
        {
            let target: String = bytes[i + 2..end].iter().collect();
            if target.ends_with(".md") && !target.contains("://") {
                found.push(normalise(&target, dir));
            }
            i = end + 1;
            continue;
        }
        i += 1;
    }
    found.sort_unstable();
    found.dedup();
    found
}

fn find_from(chars: &[char], start: usize, needle: &str) -> Option<usize> {
    let pattern: Vec<char> = needle.chars().collect();
    (start..chars.len().saturating_sub(pattern.len() - 1))
        .find(|&i| chars[i..i + pattern.len()] == pattern[..])
}

/// Resolves a relative link to a bundle-relative path, collapsing `.` and `..`.
fn normalise(target: &str, from_dir: Option<&Path>) -> String {
    let joined = match from_dir {
        Some(dir) if !target.starts_with('/') => dir.join(target),
        _ => Path::new(target.trim_start_matches('/')).to_path_buf(),
    };
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for part in joined.components() {
        match part {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::CurDir => {}
            std::path::Component::Normal(p) => parts.push(p),
            _ => {}
        }
    }
    parts
        .iter()
        .filter_map(|p| p.to_str())
        .collect::<Vec<_>>()
        .join("/")
}

fn file_stamp(root: &Path, path: &str) -> i64 {
    std::fs::metadata(root.join(path))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

/// A stable, short digest of a query, so §10.6 can count distinct questions without storing them.
///
/// Not a cryptographic hash and not meant to be: it groups repeats of one question, and the raw
/// text is not kept because a query log is a record of what someone asked.
///
/// Public so the `MemoryRecalled` event and the recall log carry the same digest. Two hashes for
/// one query would make the event stream and the log impossible to line up.
pub fn query_hash(query: &str) -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    query.trim().to_lowercase().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn to_days(day: Date) -> i64 {
    day.duration_since(EPOCH).as_secs() / 86_400
}

/// The inverse of [`to_days`]. Out-of-range days fall back to the epoch rather than failing a
/// recall: a wrong distance on one line costs less than losing the line.
fn from_days(days: i64) -> Date {
    EPOCH
        .checked_add(jiff::Span::new().days(days))
        .unwrap_or(EPOCH)
}

const fn status_str(status: Status) -> &'static str {
    match status {
        Status::Draft => "draft",
        Status::Stable => "stable",
        Status::Deprecated => "deprecated",
    }
}

fn parse_status(text: &str) -> Status {
    match text {
        "stable" => Status::Stable,
        "deprecated" => Status::Deprecated,
        _ => Status::Draft,
    }
}

const fn privacy_str(privacy: Privacy) -> &'static str {
    match privacy {
        Privacy::Normal => "normal",
        Privacy::Private => "private",
    }
}

fn parse_privacy(text: &str) -> Privacy {
    if text == "private" {
        Privacy::Private
    } else {
        Privacy::Normal
    }
}

const fn origin_str(origin: Origin) -> &'static str {
    match origin {
        Origin::Inferred => "inferred",
        Origin::Stated => "stated",
        Origin::Web => "web",
        Origin::Connector => "connector",
    }
}

/// Anything unrecognised reads as inferred, matching the file parser and §9.12's safe direction.
fn parse_origin(text: &str) -> Origin {
    match text {
        "stated" => Origin::Stated,
        "web" => Origin::Web,
        "connector" => Origin::Connector,
        _ => Origin::Inferred,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn fts_query_quotes_every_term_and_ors_them() {
        assert_eq!(
            fts_query("infra team"),
            Some("\"infra\" OR \"team\"".into())
        );
    }

    #[test]
    fn function_words_are_dropped_from_the_query() {
        assert_eq!(terms("which team is Meera on"), ["team", "meera"]);
    }

    #[test]
    fn coverage_counts_the_concept_name_as_well_as_the_claim() {
        let asked = terms("which team is Meera on");
        let full = coverage(&asked, "Works on the infra team", "Meera");
        let partial = coverage(&asked, "Works on the infra team", "Old Notes");
        assert!((full - 1.0).abs() < f32::EPSILON, "{full}");
        assert!((partial - 0.5).abs() < f32::EPSILON, "{partial}");
    }

    /// A question phrased entirely in function words produces no query at all, which is why no
    /// amount of indexing answers "who am I". Worth pinning: it looks like a retrieval bug and is
    /// not one, and the fix is §10.5's semantic fallback rather than a longer alias list.
    #[test]
    fn a_question_of_only_function_words_has_no_terms() {
        assert!(terms("who am I").is_empty());
        assert_eq!(fts_query("who am I"), None);
    }

    #[test]
    fn fts_query_neutralises_syntax_in_user_text() {
        let built = fts_query("find \"quotes\" OR star* NOT here").expect("terms");
        assert!(!built.contains('*'), "{built}");
        assert!(built.contains("\"quotes\""), "{built}");
        // A bare NOT would be an operator. Quoted, it is a term. OR is a stopword and is gone
        // entirely, which is safe for the same reason.
        assert!(built.contains("\"not\""), "{built}");
        assert!(!built.split("\" OR \"").any(|t| t == "or"), "{built}");
    }

    #[test]
    fn fts_query_is_none_when_nothing_is_searchable() {
        assert_eq!(fts_query("  ?  a "), None);
    }

    #[test]
    fn markdown_links_resolve_against_the_concept_directory() {
        let text = "Works with [Meera](../people/meera.md) on [Loki](./loki.md).";
        assert_eq!(
            links_in(text, "projects/notes.md"),
            vec![
                "people/meera.md".to_string(),
                "projects/loki.md".to_string()
            ]
        );
    }

    #[test]
    fn wiki_links_and_external_urls_are_told_apart() {
        let text = "See [[Meera]] and [docs](https://example.com/a.md).";
        assert_eq!(
            links_in(text, "projects/loki.md"),
            vec!["meera".to_string()]
        );
    }

    #[test]
    fn a_stronger_keyword_match_scores_higher() {
        let weak = combine(-1.0, 1.0, 0, 0, None);
        let strong = combine(-12.0, 1.0, 0, 0, None);
        assert!(strong > weak, "{strong:?} vs {weak:?}");
    }

    #[test]
    fn score_stays_within_zero_and_one() {
        let best = combine(-500.0, 1.0, 0, 10_000, Some(0));
        let worst = combine(-0.001, 0.0, 100_000, 0, None);
        assert!((0.0..=1.0).contains(&best.value()), "{best:?}");
        assert!((0.0..=1.0).contains(&worst.value()), "{worst:?}");
        assert!(best > worst);
    }

    #[test]
    fn recency_usage_and_links_each_move_the_score() {
        let base = combine(-5.0, 0.5, 400, 0, None);
        assert!(combine(-5.0, 0.5, 0, 0, None) > base, "recency");
        assert!(combine(-5.0, 0.5, 400, 20, None) > base, "usage");
        assert!(combine(-5.0, 0.5, 400, 0, Some(1)) > base, "link");
        assert!(combine(-5.0, 1.0, 400, 0, None) > base, "coverage");
    }

    #[test]
    fn epoch_days_round_trip_against_known_values() {
        assert_eq!(to_days(date(1970, 1, 1)), 0);
        assert_eq!(to_days(date(1970, 1, 2)), 1);
        assert_eq!(to_days(date(2026, 9, 1)), 20_697);
    }
}

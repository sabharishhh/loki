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
const SCHEMA_VERSION: i64 = 4;

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
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Score(f32);

impl Score {
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
}

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
        .or_insert(Candidate { path, name, why });
}

/// Uses recorded since the last flush, for consolidation to fold back into the files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingUse {
    pub path: String,
    pub ordinal: u32,
    pub uses: u32,
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
            "CREATE VIRTUAL TABLE IF NOT EXISTS {} USING fts5(title, body);",
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
CREATE INDEX IF NOT EXISTS claim_by_concept ON claim(concept);
CREATE INDEX IF NOT EXISTS link_by_src ON link(src);
CREATE INDEX IF NOT EXISTS link_by_dst ON link(dst);
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
                "DROP TABLE IF EXISTS claim;
                 DROP TABLE IF EXISTS concept;
                 DROP TABLE IF EXISTS link;
                 DROP TABLE IF EXISTS alias;
                 DROP TABLE IF EXISTS tag;
                 DROP TABLE IF EXISTS claim_fts;
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

        let mut on_disk = reader.concepts()?;
        on_disk.extend(reader.scratch_concepts()?);

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
    /// already in context.
    ///
    /// # Errors
    /// Fails if the index cannot be read.
    pub fn recall(&self, query: &Query<'_>) -> Result<Vec<Recalled>, IndexError> {
        let db = self.db.lock().map_err(|_| IndexError::Poisoned)?;
        // Over-fetch, because the visibility and privacy filters below reject candidates and a
        // capped result set must still be full when it can be.
        let candidates = CLAIMS_FTS
            .search(&db, query.text, query.limit.saturating_mul(8).max(64))
            .map_err(IndexError::Read)?;

        let distances = link_distances(&db, query.context)?;
        let query_terms = terms(query.text);
        let today = to_days(query.today);
        let mut out = Vec::with_capacity(query.limit);

        let mut stmt = db
            .prepare(
                "SELECT c.path, c.name, c.status, c.stale_after,
                        m.heading, m.ordinal, m.text, m.privacy, m.origin,
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
                        learned: r.get(9)?,
                        valid_from: r.get(10)?,
                        valid_to: r.get(11)?,
                        unlearned: r.get(12)?,
                        uses: r.get(13)?,
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
            let covered = coverage(&query_terms, &row.text, &row.name);
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
                score,
            });
            if out.len() == query.limit {
                break;
            }
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
            let mut stmt = tx
                .prepare(
                    "SELECT c.path, m.ordinal, m.uses_pending
                     FROM claim m JOIN concept c ON c.id = m.concept
                     WHERE m.uses_pending > 0
                     ORDER BY c.path, m.ordinal",
                )
                .map_err(IndexError::Read)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok(PendingUse {
                        path: r.get(0)?,
                        ordinal: r.get(1)?,
                        uses: r.get(2)?,
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
        let mut stmt = db
            .prepare(
                "SELECT c.path, c.name, a.text FROM alias a JOIN concept c ON c.id = a.concept",
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
            } else if strsim::jaro_winkler(&form, &needle) >= NEAR_NAME {
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
    tx.execute(
        "INSERT INTO concept(path, name, status, verified, stale_after, mtime, len)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            path,
            concept.front.name,
            status_str(concept.front.status),
            verified,
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
                "INSERT INTO claim(concept, ordinal, heading, text, privacy, origin,
                                   valid_from, valid_to, learned, unlearned, usage_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    concept_id,
                    ordinal,
                    section.heading,
                    claim.text,
                    privacy_str(claim.privacy),
                    origin_str(claim.origin),
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
fn coverage(query_terms: &[String], text: &str, name: &str) -> f32 {
    if query_terms.is_empty() {
        return 0.0;
    }
    let haystack = format!("{} {}", text.to_lowercase(), name.to_lowercase());
    let matched = query_terms
        .iter()
        .filter(|t| haystack.contains(t.as_str()))
        .count();
    #[allow(clippy::cast_precision_loss)]
    let fraction = matched as f32 / query_terms.len() as f32;
    fraction
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

fn to_days(day: Date) -> i64 {
    day.duration_since(EPOCH).as_secs() / 86_400
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

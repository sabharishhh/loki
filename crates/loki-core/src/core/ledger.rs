//! The spend record.
//!
//! Every model call, search and page fetch is appended here. It is an [`EventSink`], so it costs
//! the loop nothing to feed: it reads the same stream the renderers do.
//!
//! Amounts are stored in micro-cents. One call costs a fraction of a cent, and rounding each to
//! whole cents would record nothing on a cheap model.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};

use super::event::Event;
use super::ids::TaskId;
use super::sink::EventSink;
use super::vocab::{CostModel, ModelRole};

/// What a spend entry was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Model,
    Search,
    Fetch,
}

impl Kind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Search => "search",
            Self::Fetch => "fetch",
        }
    }
}

/// One line of spend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub at: i64,
    pub task: Option<TaskId>,
    pub kind: Kind,
    pub provider: String,
    pub role: Option<ModelRole>,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub micro_cents: u64,
}

/// Spend on one calendar day, local time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayTotal {
    pub day: String,
    pub micro_cents: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("could not open the ledger: {0}")]
    Open(#[source] rusqlite::Error),
    #[error("could not write to the ledger: {0}")]
    Write(#[source] rusqlite::Error),
    #[error("could not read the ledger: {0}")]
    Read(#[source] rusqlite::Error),
    #[error("no application support directory")]
    NoHome,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS spend (
    id          INTEGER PRIMARY KEY,
    at          INTEGER NOT NULL,
    task        INTEGER,
    kind        TEXT    NOT NULL,
    provider    TEXT    NOT NULL,
    role        TEXT,
    tokens_in   INTEGER NOT NULL DEFAULT 0,
    tokens_out  INTEGER NOT NULL DEFAULT 0,
    micro_cents INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS spend_at ON spend(at);
CREATE INDEX IF NOT EXISTS spend_task ON spend(task);
";

/// Append-only spend history.
pub struct Ledger {
    conn: Mutex<Connection>,
    /// The task events are currently attributed to.
    ///
    /// `ModelCall` carries no task id, so the task is tracked from the surrounding
    /// `TaskStarted` and `TaskFinished`. Correct because the loop runs one task at a time.
    current: Mutex<Option<TaskId>>,
}

impl Ledger {
    /// The default location, `~/Library/Application Support/Loki/ledger.sqlite`.
    ///
    /// # Errors
    /// Fails if there is no application support directory.
    pub fn default_path() -> Result<PathBuf, LedgerError> {
        let base = dirs::data_dir().ok_or(LedgerError::NoHome)?;
        Ok(base.join("Loki").join("ledger.sqlite"))
    }

    /// Opens or creates the ledger, making the parent directory if needed.
    ///
    /// # Errors
    /// Fails if the file cannot be created or the schema cannot be applied.
    pub fn open(path: &Path) -> Result<Self, LedgerError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                LedgerError::Open(rusqlite::Error::ToSqlConversionFailure(e.into()))
            })?;
        }
        let conn = Connection::open(path).map_err(LedgerError::Open)?;
        Self::prepare(conn)
    }

    /// An in-memory ledger, for tests.
    ///
    /// # Errors
    /// Fails if the schema cannot be applied.
    pub fn in_memory() -> Result<Self, LedgerError> {
        Self::prepare(Connection::open_in_memory().map_err(LedgerError::Open)?)
    }

    fn prepare(conn: Connection) -> Result<Self, LedgerError> {
        conn.execute_batch(SCHEMA).map_err(LedgerError::Open)?;
        Ok(Self {
            conn: Mutex::new(conn),
            current: Mutex::new(None),
        })
    }

    /// Appends one entry.
    ///
    /// # Errors
    /// Fails if the insert fails.
    ///
    /// # Panics
    /// If another thread panicked while holding the connection.
    pub fn record(&self, entry: &Entry) -> Result<(), LedgerError> {
        let conn = self.conn.lock().expect("ledger poisoned");
        conn.execute(
            "INSERT INTO spend (at, task, kind, provider, role, tokens_in, tokens_out, micro_cents)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                entry.at,
                entry.task.map(TaskId::get),
                entry.kind.as_str(),
                entry.provider,
                entry.role.map(role_name),
                entry.tokens_in,
                entry.tokens_out,
                entry.micro_cents,
            ],
        )
        .map_err(LedgerError::Write)?;
        Ok(())
    }

    /// Total spend since a unix timestamp, in micro-cents.
    ///
    /// # Errors
    /// Fails if the query fails.
    ///
    /// # Panics
    /// If another thread panicked while holding the connection.
    pub fn spent_since(&self, at: i64) -> Result<u64, LedgerError> {
        let conn = self.conn.lock().expect("ledger poisoned");
        conn.query_row(
            "SELECT COALESCE(SUM(micro_cents), 0) FROM spend WHERE at >= ?1",
            params![at],
            |row| row.get(0),
        )
        .map_err(LedgerError::Read)
    }

    /// Spend so far this calendar month, local time. What the monthly ceiling is measured against.
    ///
    /// # Errors
    /// Fails if the query fails.
    ///
    /// # Panics
    /// If another thread panicked while holding the connection.
    pub fn spent_this_month(&self) -> Result<u64, LedgerError> {
        let conn = self.conn.lock().expect("ledger poisoned");
        conn.query_row(
            "SELECT COALESCE(SUM(micro_cents), 0) FROM spend
             WHERE date(at, 'unixepoch', 'localtime')
                   >= date('now', 'localtime', 'start of month')",
            [],
            |row| row.get(0),
        )
        .map_err(LedgerError::Read)
    }

    /// Daily totals, most recent first.
    ///
    /// # Errors
    /// Fails if the query fails.
    ///
    /// # Panics
    /// If another thread panicked while holding the connection.
    pub fn by_day(&self, limit: u32) -> Result<Vec<DayTotal>, LedgerError> {
        let conn = self.conn.lock().expect("ledger poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT date(at, 'unixepoch', 'localtime') AS day, SUM(micro_cents)
                 FROM spend GROUP BY day ORDER BY day DESC LIMIT ?1",
            )
            .map_err(LedgerError::Read)?;
        let rows = stmt
            .query_map(params![limit], |row| {
                Ok(DayTotal {
                    day: row.get(0)?,
                    micro_cents: row.get(1)?,
                })
            })
            .map_err(LedgerError::Read)?;
        rows.collect::<Result<_, _>>().map_err(LedgerError::Read)
    }

    /// Total spend for one task. What the Activity row shows.
    ///
    /// # Errors
    /// Fails if the query fails.
    ///
    /// # Panics
    /// If another thread panicked while holding the connection.
    pub fn by_task(&self, task: TaskId) -> Result<u64, LedgerError> {
        let conn = self.conn.lock().expect("ledger poisoned");
        conn.query_row(
            "SELECT COALESCE(SUM(micro_cents), 0) FROM spend WHERE task = ?1",
            params![task.get()],
            |row| row.get(0),
        )
        .map_err(LedgerError::Read)
    }

    fn task(&self) -> Option<TaskId> {
        *self.current.lock().expect("ledger poisoned")
    }

    fn set_task(&self, task: Option<TaskId>) {
        *self.current.lock().expect("ledger poisoned") = task;
    }
}

impl EventSink for Ledger {
    fn emit(&self, event: &Event) {
        let entry = match event {
            Event::TaskStarted { id, .. } => {
                self.set_task(Some(*id));
                return;
            }
            Event::TaskFinished { .. } => {
                self.set_task(None);
                return;
            }
            Event::ModelCall {
                provider,
                role,
                tokens_in,
                tokens_out,
                cost,
                ..
            } => Entry {
                at: now(),
                task: self.task(),
                kind: Kind::Model,
                provider: provider.clone(),
                role: Some(*role),
                tokens_in: *tokens_in,
                tokens_out: *tokens_out,
                micro_cents: cost.charge_micros(*tokens_in, *tokens_out),
            },
            Event::Searched { provider, cost, .. } => Entry {
                at: now(),
                task: self.task(),
                kind: Kind::Search,
                provider: provider.clone(),
                role: None,
                tokens_in: 0,
                tokens_out: 0,
                micro_cents: flat(*cost),
            },
            Event::Fetched { cost, .. } => Entry {
                at: now(),
                task: self.task(),
                kind: Kind::Fetch,
                provider: "ladder".to_owned(),
                role: None,
                tokens_in: 0,
                tokens_out: 0,
                micro_cents: flat(*cost),
            },
            _ => return,
        };

        // A sink must not fail the loop. A lost line is better than a lost turn.
        let _ = self.record(&entry);
    }
}

/// A search or fetch has no tokens, so a per-token rate cannot price it.
///
/// The free ladder is the v1 default, so this is zero in practice. A paid adapter will need its
/// own per-call price on the event.
const fn flat(cost: CostModel) -> u64 {
    match cost {
        CostModel::Free => 0,
        CostModel::PerToken { .. } => 0,
    }
}

const fn role_name(role: ModelRole) -> &'static str {
    match role {
        ModelRole::Primary => "primary",
        ModelRole::Utility => "utility",
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::vocab::{Cents, Locality, TaskStatus};

    fn terra() -> CostModel {
        CostModel::PerToken {
            input_per_mtok: Cents::new(400),
            output_per_mtok: Cents::new(500),
        }
    }

    fn model_call() -> Event {
        Event::ModelCall {
            provider: "openai".into(),
            role: ModelRole::Primary,
            locality: Locality::Cloud,
            tokens_in: 12_000,
            tokens_out: 800,
            cost: terra(),
        }
    }

    #[test]
    fn a_model_call_is_recorded_at_full_precision() {
        let ledger = Ledger::in_memory().unwrap();
        ledger.emit(&model_call());
        // 12k in at 400 plus 800 out at 500 is 5.2 cents.
        assert_eq!(ledger.spent_since(0).unwrap(), 5_200_000);
    }

    #[test]
    fn sub_cent_calls_accumulate_instead_of_vanishing() {
        let luna = CostModel::PerToken {
            input_per_mtok: Cents::new(40),
            output_per_mtok: Cents::new(50),
        };
        let ledger = Ledger::in_memory().unwrap();
        for _ in 0..1000 {
            ledger.emit(&Event::ModelCall {
                provider: "openai".into(),
                role: ModelRole::Utility,
                locality: Locality::Cloud,
                tokens_in: 12_000,
                tokens_out: 800,
                cost: luna,
            });
        }
        // 0.52 cents each. Whole cents would have recorded zero a thousand times over.
        assert_eq!(ledger.spent_since(0).unwrap(), 520_000_000);
    }

    #[test]
    fn spend_is_attributed_to_the_surrounding_task() {
        let ledger = Ledger::in_memory().unwrap();
        ledger.emit(&Event::TaskStarted {
            id: TaskId::new(7),
            summary: String::new(),
        });
        ledger.emit(&model_call());
        ledger.emit(&Event::TaskFinished {
            id: TaskId::new(7),
            status: TaskStatus::Completed,
        });

        assert_eq!(ledger.by_task(TaskId::new(7)).unwrap(), 5_200_000);
        assert_eq!(ledger.by_task(TaskId::new(8)).unwrap(), 0);
    }

    #[test]
    fn spend_outside_a_task_is_still_recorded() {
        let ledger = Ledger::in_memory().unwrap();
        ledger.emit(&model_call());
        assert_eq!(ledger.spent_since(0).unwrap(), 5_200_000);
        assert_eq!(ledger.by_task(TaskId::new(0)).unwrap(), 0);
    }

    #[test]
    fn non_billable_events_are_ignored() {
        let ledger = Ledger::in_memory().unwrap();
        ledger.emit(&Event::ScopeClosed {
            id: crate::core::ids::ScopeId::new(0),
            ms: 10,
        });
        assert_eq!(ledger.spent_since(0).unwrap(), 0);
    }

    #[test]
    fn this_month_and_by_day_see_a_fresh_entry() {
        let ledger = Ledger::in_memory().unwrap();
        ledger.emit(&model_call());
        assert_eq!(ledger.spent_this_month().unwrap(), 5_200_000);

        let days = ledger.by_day(30).unwrap();
        assert_eq!(days.len(), 1);
        assert_eq!(days[0].micro_cents, 5_200_000);
    }

    #[test]
    fn history_survives_reopening_the_file() {
        let dir = std::env::temp_dir().join(format!("loki-ledger-{}", std::process::id()));
        let path = dir.join("ledger.sqlite");
        let _ = std::fs::remove_dir_all(&dir);

        {
            let ledger = Ledger::open(&path).unwrap();
            ledger.emit(&model_call());
        }
        let reopened = Ledger::open(&path).unwrap();
        assert_eq!(reopened.spent_since(0).unwrap(), 5_200_000);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

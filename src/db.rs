use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::error::{Result, RewindError};
use crate::model::{
    Effect, EffectType, JournalStatus, Manifest, OperationKind, OperationRecord, OperationStatus,
    Reversibility, StateKind, StateRecord, TrackingConfidence, WorkspaceCondition,
};

#[derive(Clone, Debug)]
pub struct Catalog {
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct WorkspaceRow {
    pub id: Uuid,
    pub root: String,
    pub condition: WorkspaceCondition,
    pub baseline_state: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OperationDraft {
    pub kind: OperationKind,
    pub status: OperationStatus,
    pub pre_state_id: Option<String>,
    pub post_state_id: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub exit_code: Option<i32>,
    pub confidence: TrackingConfidence,
    pub reversibility: Reversibility,
    pub error: Option<String>,
    pub effects: Vec<Effect>,
}

/// One durable passive boundary. `id` is the immutable identity that the
/// shell integration captures from the pre-hook and hands back to the
/// post-hook; it is the only correlation key that may ever be used to pair a
/// boundary with the background bookkeeping that completes it (Phase 1.4).
#[derive(Clone, Debug)]
pub struct BoundaryRow {
    pub id: String,
    pub session_id: String,
    pub command: String,
    pub cwd: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub exit_code: Option<i32>,
    /// `true` once exactly one post-hook has accounted for this boundary.
    /// A consumed boundary is never consumed again.
    pub consumed: bool,
}

/// One recorded unknown interval: a span of history whose causal and state
/// evidence is not trustworthy. Phase 2 planning treats an interval that
/// intersects a rollback closure as a hard boundary and must be able to name
/// it; the boolean `has_open_unknown` cannot.
#[derive(Clone, Debug)]
pub struct UnknownIntervalRow {
    pub id: i64,
    pub start_state_id: Option<String>,
    pub end_state_id: Option<String>,
    pub reason: String,
    pub is_open: bool,
    pub created_at: i64,
    pub closed_at: Option<i64>,
}

const BOUNDARY_COLUMNS: &str =
    "id, session_id, command, cwd, started_at, ended_at, exit_code, consumed";

fn read_boundary(row: &rusqlite::Row<'_>) -> rusqlite::Result<BoundaryRow> {
    Ok(BoundaryRow {
        id: row.get(0)?,
        session_id: row.get(1)?,
        command: row.get(2)?,
        cwd: row.get(3)?,
        started_at: row.get(4)?,
        ended_at: row.get(5)?,
        exit_code: row.get(6)?,
        consumed: row.get::<_, i64>(7)? != 0,
    })
}

impl Catalog {
    pub fn initialize(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let catalog = Self { path };
        let connection = catalog.connection()?;
        // The `passive_boundary_consume_once` trigger makes the boundary
        // accounting invariant database-enforced rather than caller-enforced:
        // a boundary that a post-hook has accounted for (`consumed = 1`) can
        // never be rewritten or resurrected. It is created with IF NOT EXISTS
        // so existing catalogs pick it up on their next open.
        connection.execute_batch(
            "
            PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = FULL;
            CREATE TABLE IF NOT EXISTS workspaces (
                id TEXT PRIMARY KEY,
                root TEXT NOT NULL,
                condition TEXT NOT NULL,
                baseline_state_id TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS states (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL REFERENCES workspaces(id),
                kind TEXT NOT NULL,
                parent_id TEXT,
                label TEXT,
                manifest_json TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS operations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace_id TEXT NOT NULL REFERENCES workspaces(id),
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                pre_state_id TEXT,
                post_state_id TEXT,
                command TEXT,
                cwd TEXT,
                exit_code INTEGER,
                confidence TEXT NOT NULL,
                reversibility TEXT NOT NULL,
                error TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS effects (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                operation_id INTEGER NOT NULL REFERENCES operations(id),
                path TEXT NOT NULL,
                from_path TEXT,
                effect_type TEXT NOT NULL,
                pre_json TEXT NOT NULL,
                post_json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS unknown_intervals (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace_id TEXT NOT NULL REFERENCES workspaces(id),
                start_state_id TEXT,
                end_state_id TEXT,
                reason TEXT NOT NULL,
                is_open INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                closed_at INTEGER
            );
            CREATE TABLE IF NOT EXISTS passive_boundaries (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL REFERENCES workspaces(id),
                session_id TEXT NOT NULL,
                command TEXT NOT NULL,
                cwd TEXT NOT NULL,
                started_at INTEGER NOT NULL,
                ended_at INTEGER,
                exit_code INTEGER,
                consumed INTEGER NOT NULL DEFAULT 0 CHECK (consumed IN (0, 1))
            );
            CREATE TRIGGER IF NOT EXISTS passive_boundary_consume_once
            BEFORE UPDATE ON passive_boundaries
            WHEN OLD.consumed = 1
            BEGIN
                SELECT RAISE(ABORT, 'passive boundary already accounted for');
            END;
            CREATE TABLE IF NOT EXISTS bypass_markers (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace_id TEXT NOT NULL REFERENCES workspaces(id),
                boundary_id TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                consumed INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS transactions (
                id TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL REFERENCES workspaces(id),
                operation_id INTEGER,
                status TEXT NOT NULL,
                journal_path TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS snapshots (
                name TEXT PRIMARY KEY,
                workspace_id TEXT NOT NULL REFERENCES workspaces(id),
                state_id TEXT NOT NULL REFERENCES states(id),
                created_at INTEGER NOT NULL
            );
            ",
        )?;
        Ok(catalog)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn connection(&self) -> Result<Connection> {
        let connection = Connection::open(&self.path)?;
        connection.execute_batch("PRAGMA foreign_keys = ON;")?;
        Ok(connection)
    }

    pub fn integrity_check(&self) -> Result<String> {
        let connection = self.connection()?;
        Ok(connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?)
    }

    pub fn create_workspace(&self, id: Uuid, root: &str) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO workspaces(id, root, condition, baseline_state_id, created_at)
             VALUES(?1, ?2, 'DEGRADED', NULL, ?3)",
            params![id.to_string(), root, now()],
        )?;
        Ok(())
    }

    pub fn workspace(&self, id: Uuid) -> Result<WorkspaceRow> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT id, root, condition, baseline_state_id FROM workspaces WHERE id=?1",
                params![id.to_string()],
                |row| {
                    let id: String = row.get(0)?;
                    let condition: String = row.get(2)?;
                    Ok((id, row.get::<_, String>(1)?, condition, row.get(3)?))
                },
            )
            .optional()?
            .ok_or_else(|| RewindError::NotFound(format!("workspace {id}")))?;
        Ok(WorkspaceRow {
            id: Uuid::parse_str(&row.0)
                .map_err(|error| RewindError::Database(error.to_string()))?,
            root: row.1,
            condition: WorkspaceCondition::from_str(&row.2)?,
            baseline_state: row.3,
        })
    }

    pub fn set_workspace(
        &self,
        id: Uuid,
        condition: &WorkspaceCondition,
        baseline_state: Option<&str>,
    ) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE workspaces SET condition=?2, baseline_state_id=?3 WHERE id=?1",
            params![id.to_string(), condition.as_str(), baseline_state],
        )?;
        Ok(())
    }

    pub fn insert_state(
        &self,
        workspace_id: Uuid,
        kind: StateKind,
        manifest: &Manifest,
        parent_id: Option<&str>,
        label: Option<&str>,
    ) -> Result<String> {
        let id = manifest.state_id()?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT OR IGNORE INTO states(id, workspace_id, kind, parent_id, label, manifest_json, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                workspace_id.to_string(),
                kind.as_str(),
                parent_id,
                label,
                serde_json::to_string(manifest)?,
                now()
            ],
        )?;
        Ok(id)
    }

    pub fn state(&self, id: &str) -> Result<StateRecord> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT id, kind, parent_id, label, manifest_json FROM states WHERE id=?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| RewindError::NotFound(format!("state {id}")))?;
        Ok(StateRecord {
            id: row.0,
            kind: StateKind::from_str(&row.1)?,
            manifest: serde_json::from_str(&row.4)?,
            parent_id: row.2,
            label: row.3,
        })
    }

    pub fn insert_operation(&self, workspace_id: Uuid, draft: &OperationDraft) -> Result<i64> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO operations(
                workspace_id, kind, status, pre_state_id, post_state_id, command,
                cwd, exit_code, confidence, reversibility, error, created_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                workspace_id.to_string(),
                draft.kind.as_str(),
                draft.status.as_str(),
                draft.pre_state_id,
                draft.post_state_id,
                draft.command,
                draft.cwd,
                draft.exit_code,
                draft.confidence.as_str(),
                draft.reversibility.as_str(),
                draft.error,
                now()
            ],
        )?;
        let operation_id = transaction.last_insert_rowid();
        for effect in &draft.effects {
            transaction.execute(
                "INSERT INTO effects(operation_id, path, from_path, effect_type, pre_json, post_json)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    operation_id,
                    effect.path,
                    effect.from_path,
                    effect.effect_type.as_str(),
                    serde_json::to_string(&effect.pre)?,
                    serde_json::to_string(&effect.post)?
                ],
            )?;
        }
        transaction.commit()?;
        Ok(operation_id)
    }

    pub fn operation(&self, id: i64, workspace_id: Uuid) -> Result<OperationRecord> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT id, workspace_id, kind, status, pre_state_id, post_state_id, command, cwd,
                        exit_code, confidence, reversibility, error, created_at
                 FROM operations WHERE id=?1 AND workspace_id=?2",
                params![id, workspace_id.to_string()],
                read_operation,
            )
            .optional()?
            .ok_or_else(|| RewindError::NotFound(format!("operation {id}")))?;
        let effects = read_effects(&connection, id)?;
        Ok(OperationRecord { effects, ..row })
    }

    pub fn list_operations(&self, workspace_id: Uuid) -> Result<Vec<OperationRecord>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, workspace_id, kind, status, pre_state_id, post_state_id, command, cwd,
                    exit_code, confidence, reversibility, error, created_at
             FROM operations WHERE workspace_id=?1 ORDER BY id",
        )?;
        let rows = statement.query_map(params![workspace_id.to_string()], read_operation)?;
        let mut operations = Vec::new();
        for row in rows {
            let operation = row?;
            let effects = read_effects(&connection, operation.id)?;
            operations.push(OperationRecord {
                effects,
                ..operation
            });
        }
        Ok(operations)
    }

    pub fn latest_undoable(&self, workspace_id: Uuid) -> Result<OperationRecord> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT id, workspace_id, kind, status, pre_state_id, post_state_id, command, cwd,
                        exit_code, confidence, reversibility, error, created_at
                 FROM operations
                 WHERE workspace_id=?1 AND status='COMPLETED' AND reversibility='FULLY_REVERSIBLE'
                 ORDER BY id DESC LIMIT 1",
                params![workspace_id.to_string()],
                read_operation,
            )
            .optional()?
            .ok_or_else(|| RewindError::NotFound("no undoable operation".to_owned()))?;
        let effects = read_effects(&connection, row.id)?;
        Ok(OperationRecord { effects, ..row })
    }

    pub fn latest_redoable(&self, workspace_id: Uuid) -> Result<OperationRecord> {
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT id, workspace_id, kind, status, pre_state_id, post_state_id, command, cwd,
                        exit_code, confidence, reversibility, error, created_at
                 FROM operations
                 WHERE workspace_id=?1 AND status='UNDONE' AND reversibility='FULLY_REVERSIBLE'
                 ORDER BY id DESC LIMIT 1",
                params![workspace_id.to_string()],
                read_operation,
            )
            .optional()?
            .ok_or_else(|| RewindError::NotFound("no redoable operation".to_owned()))?;
        let effects = read_effects(&connection, row.id)?;
        Ok(OperationRecord { effects, ..row })
    }

    pub fn set_operation_status(&self, id: i64, status: OperationStatus) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE operations SET status=?2 WHERE id=?1",
            params![id, status.as_str()],
        )?;
        Ok(())
    }

    pub fn open_unknown(
        &self,
        workspace_id: Uuid,
        start_state: Option<&str>,
        reason: &str,
    ) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO unknown_intervals(workspace_id, start_state_id, end_state_id, reason, is_open, created_at)
             VALUES(?1, ?2, NULL, ?3, 1, ?4)",
            params![workspace_id.to_string(), start_state, reason, now()],
        )?;
        Ok(())
    }

    pub fn close_unknown(&self, workspace_id: Uuid, end_state: &str) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE unknown_intervals SET end_state_id=?2, is_open=0, closed_at=?3
             WHERE workspace_id=?1 AND is_open=1",
            params![workspace_id.to_string(), end_state, now()],
        )?;
        Ok(())
    }

    pub fn has_open_unknown(&self, workspace_id: Uuid) -> Result<bool> {
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM unknown_intervals WHERE workspace_id=?1 AND is_open=1",
            params![workspace_id.to_string()],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }
    /// Every unknown interval recorded for this workspace, ordered by
    /// `(created_at, id)` so the sequence is deterministic.
    ///
    /// Read-only: this is part of the Phase 2 planning surface and must never
    /// be called from a path that mutates the workspace.
    pub fn unknown_intervals(&self, workspace_id: Uuid) -> Result<Vec<UnknownIntervalRow>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, start_state_id, end_state_id, reason, is_open, created_at, closed_at
             FROM unknown_intervals WHERE workspace_id=?1 ORDER BY created_at, id",
        )?;
        let mapped = statement.query_map(params![workspace_id.to_string()], |row| {
            Ok(UnknownIntervalRow {
                id: row.get(0)?,
                start_state_id: row.get(1)?,
                end_state_id: row.get(2)?,
                reason: row.get(3)?,
                is_open: row.get::<_, i64>(4)? != 0,
                created_at: row.get(5)?,
                closed_at: row.get(6)?,
            })
        })?;
        let mut intervals = Vec::new();
        for interval in mapped {
            intervals.push(interval?);
        }
        Ok(intervals)
    }

    pub fn add_bypass_marker(&self, workspace_id: Uuid, boundary_id: &str) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO bypass_markers(workspace_id, boundary_id, created_at)
             VALUES(?1, ?2, ?3)",
            params![workspace_id.to_string(), boundary_id, now()],
        )?;
        Ok(())
    }

    pub fn has_pending_bypass(&self, workspace_id: Uuid) -> Result<bool> {
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM bypass_markers WHERE workspace_id=?1 AND consumed=0",
            params![workspace_id.to_string()],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn consume_bypasses(&self, workspace_id: Uuid) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE bypass_markers SET consumed=1 WHERE workspace_id=?1 AND consumed=0",
            params![workspace_id.to_string()],
        )?;
        Ok(())
    }

    /// Creates a durable passive boundary and returns its immutable id.
    ///
    /// The id is the ONLY correlation key between a boundary and the
    /// background post-hook that completes it: the pre-hook prints it, the
    /// shell integration captures it for exactly the command whose post-hook
    /// will follow, and the post-hook hands it back. Nothing may ever
    /// discover a boundary by recency, timestamp, command text, session
    /// ordering, or "most recent unconsumed" lookup (Phase 1.4).
    pub fn add_boundary(
        &self,
        workspace_id: Uuid,
        session_id: &str,
        command: &str,
        cwd: &str,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO passive_boundaries(id, workspace_id, session_id, command, cwd, started_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                workspace_id.to_string(),
                session_id,
                command,
                cwd,
                now()
            ],
        )?;
        Ok(id)
    }

    /// Fetches exactly one boundary by identity, scoped to the workspace.
    ///
    /// Returns `None` when the id is unknown or belongs to another
    /// workspace; the caller must then fail open. There is deliberately no
    /// session-scoped or recency-scoped lookup: with asynchronous post-hooks
    /// several boundaries can be in flight at once, so any "which boundary
    /// is newest?" lookup can consume another command's boundary.
    pub fn boundary(&self, workspace_id: Uuid, id: &str) -> Result<Option<BoundaryRow>> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                &format!(
                    "SELECT {BOUNDARY_COLUMNS} FROM passive_boundaries
                     WHERE id=?1 AND workspace_id=?2"
                ),
                params![id, workspace_id.to_string()],
                read_boundary,
            )
            .optional()?)
    }

    /// Lists a workspace's boundaries in creation order for diagnostics.
    pub fn boundaries(&self, workspace_id: Uuid) -> Result<Vec<BoundaryRow>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(&format!(
            "SELECT {BOUNDARY_COLUMNS} FROM passive_boundaries
             WHERE workspace_id=?1 ORDER BY started_at, id"
        ))?;
        let rows = statement.query_map(params![workspace_id.to_string()], read_boundary)?;
        let mut boundaries = Vec::new();
        for row in rows {
            boundaries.push(row?);
        }
        Ok(boundaries)
    }

    /// Atomically accounts for exactly one boundary, recording the finished
    /// command's exit status. Returns `false` when the boundary does not
    /// exist in this workspace or was already accounted for; callers must
    /// treat `false` conservatively (fail open, never fall back to another
    /// boundary). The `consumed = 0` predicate plus SQLite's write
    /// serialization make duplicate accounting impossible even when several
    /// background hook processes race for the same id.
    pub fn finish_boundary(&self, workspace_id: Uuid, id: &str, exit_code: i32) -> Result<bool> {
        let connection = self.connection()?;
        let updated = connection.execute(
            "UPDATE passive_boundaries SET ended_at=?3, exit_code=?4, consumed=1
             WHERE id=?1 AND workspace_id=?2 AND consumed=0",
            params![id, workspace_id.to_string(), now(), exit_code],
        )?;
        Ok(updated == 1)
    }

    pub fn add_transaction(
        &self,
        id: Uuid,
        workspace_id: Uuid,
        operation_id: Option<i64>,
        status: &JournalStatus,
        journal_path: &str,
    ) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO transactions(id, workspace_id, operation_id, status, journal_path, created_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id.to_string(),
                workspace_id.to_string(),
                operation_id,
                status.as_str(),
                journal_path,
                now()
            ],
        )?;
        Ok(())
    }

    pub fn update_transaction(&self, id: Uuid, status: &JournalStatus) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE transactions SET status=?2 WHERE id=?1",
            params![id.to_string(), status.as_str()],
        )?;
        Ok(())
    }

    pub fn unfinished_transactions(
        &self,
        workspace_id: Uuid,
    ) -> Result<Vec<(Uuid, String, String)>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, status, journal_path FROM transactions
             WHERE workspace_id=?1 AND status <> 'COMMITTED' AND status <> 'ABANDONED'",
        )?;
        let rows = statement.query_map(params![workspace_id.to_string()], |row| {
            let id: String = row.get(0)?;
            Ok((id, row.get(1)?, row.get(2)?))
        })?;
        let mut result = Vec::new();
        for row in rows {
            let row = row?;
            let id = Uuid::parse_str(&row.0)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            result.push((id, row.1, row.2));
        }
        Ok(result)
    }

    pub fn snapshot(&self, workspace_id: Uuid, name: &str, state_id: &str) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO snapshots(name, workspace_id, state_id, created_at)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET state_id=excluded.state_id, created_at=excluded.created_at",
            params![name, workspace_id.to_string(), state_id, now()],
        )?;
        Ok(())
    }

    pub fn snapshot_state(&self, workspace_id: Uuid, name: &str) -> Result<String> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT state_id FROM snapshots WHERE workspace_id=?1 AND name=?2",
                params![workspace_id.to_string(), name],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| RewindError::NotFound(format!("snapshot {name}")))
    }
}

fn read_operation(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationRecord> {
    let workspace_id: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let status: String = row.get(3)?;
    let confidence: String = row.get(9)?;
    let reversibility: String = row.get(10)?;
    Ok(OperationRecord {
        id: row.get(0)?,
        workspace_id: Uuid::parse_str(&workspace_id)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?,
        kind: parse_kind(&kind)?,
        status: parse_status(&status)?,
        pre_state_id: row.get(4)?,
        post_state_id: row.get(5)?,
        command: row.get(6)?,
        cwd: row.get(7)?,
        exit_code: row.get(8)?,
        confidence: parse_confidence(&confidence)?,
        reversibility: parse_reversibility(&reversibility)?,
        error: row.get(11)?,
        created_at: row.get(12)?,
        effects: Vec::new(),
    })
}

fn read_effects(connection: &Connection, operation_id: i64) -> Result<Vec<Effect>> {
    let mut statement = connection.prepare(
        "SELECT path, from_path, effect_type, pre_json, post_json
         FROM effects WHERE operation_id=?1 ORDER BY id",
    )?;
    let rows = statement.query_map(params![operation_id], |row| {
        let effect_type: String = row.get(2)?;
        let pre: String = row.get(3)?;
        let post: String = row.get(4)?;
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            effect_type,
            pre,
            post,
        ))
    })?;
    let mut effects = Vec::new();
    for row in rows {
        let row = row?;
        effects.push(Effect {
            path: row.0,
            from_path: row.1,
            effect_type: parse_effect_type(&row.2)?,
            pre: serde_json::from_str(&row.3)?,
            post: serde_json::from_str(&row.4)?,
        });
    }
    Ok(effects)
}

fn parse_kind(value: &str) -> rusqlite::Result<OperationKind> {
    match value {
        "STRONG" => Ok(OperationKind::Strong),
        "PASSIVE_OBSERVATION" => Ok(OperationKind::PassiveObservation),
        "RECONCILIATION" => Ok(OperationKind::Reconciliation),
        "RESTORE" => Ok(OperationKind::Restore),
        "CAPTURE_FAILED" => Ok(OperationKind::CaptureFailed),
        "BOUNDARY_ONLY" => Ok(OperationKind::BoundaryOnly),
        other => Err(other_error("operation kind", other)),
    }
}

fn parse_status(value: &str) -> rusqlite::Result<OperationStatus> {
    match value {
        "COMPLETED" => Ok(OperationStatus::Completed),
        "UNDONE" => Ok(OperationStatus::Undone),
        "FAILED" => Ok(OperationStatus::Failed),
        "UNTRUSTED" => Ok(OperationStatus::Untrusted),
        other => Err(other_error("operation status", other)),
    }
}

fn parse_confidence(value: &str) -> rusqlite::Result<TrackingConfidence> {
    match value {
        "TRACKED" => Ok(TrackingConfidence::Tracked),
        "LOW_CONFIDENCE_OBSERVATION" => Ok(TrackingConfidence::LowConfidenceObservation),
        "DEGRADED" => Ok(TrackingConfidence::Degraded),
        "UNSUPPORTED" => Ok(TrackingConfidence::Unsupported),
        other => Err(other_error("confidence", other)),
    }
}

fn parse_reversibility(value: &str) -> rusqlite::Result<Reversibility> {
    match value {
        "FULLY_REVERSIBLE" => Ok(Reversibility::FullyReversible),
        "PARTIALLY_REVERSIBLE" => Ok(Reversibility::PartiallyReversible),
        "UNAVAILABLE" => Ok(Reversibility::Unavailable),
        "UNSUPPORTED" => Ok(Reversibility::Unsupported),
        other => Err(other_error("reversibility", other)),
    }
}

fn parse_effect_type(value: &str) -> rusqlite::Result<EffectType> {
    match value {
        "CREATE" => Ok(EffectType::Create),
        "MODIFY" => Ok(EffectType::Modify),
        "DELETE" => Ok(EffectType::Delete),
        "RENAME" => Ok(EffectType::Rename),
        "TYPE_CHANGE" => Ok(EffectType::TypeChange),
        other => Err(other_error("effect type", other)),
    }
}

fn other_error(kind: &str, value: &str) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("unknown {kind}: {value}"),
    )))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros() as i64)
        .unwrap_or(0)
}

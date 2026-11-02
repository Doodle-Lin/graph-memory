//! Read-only logical snapshot reader for `graph-memory` workspace databases.
//!
//! [`read_snapshot`] opens an existing database strictly read-only, checks that
//! the schema is exactly the one this build knows how to serialize, and
//! materialises every application row into a [`Snapshot`]. This module never
//! writes: it does not create the file, does not create or migrate tables, and
//! does not change `journal_mode`.
//!
//! Consistency
//! -----------
//! A single deferred transaction is opened before anything is inspected, so
//! `user_version`, `sqlite_master`, `table_xinfo`, `quick_check`, the byte
//! accounting and all four `SELECT`s observe one SQLite snapshot. The
//! transaction is committed only after every row array has been collected.
//! [`read_snapshot_with_hook`] additionally runs a hook while that snapshot is
//! still open, right after the project rows are read, so tests can interleave a
//! concurrent writer deterministically. Production code passes `|| {}`.
//!
//! Errors are deliberately generic: no SQLite message, filesystem path or
//! stored content is ever exposed through the returned `String`.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use crate::backup_format::{EventRow, MemoryRow, ProjectRow, RevisionRow, Snapshot, MAX_BYTES};

const ERR_OPEN: &str = "workspace database could not be opened read-only";
const ERR_SCHEMA: &str = "workspace database schema is not supported";
const ERR_READ: &str = "workspace database could not be read";
const ERR_INTEGRITY: &str = "workspace database failed its integrity check";
const ERR_TOO_LARGE: &str = "workspace data is too large to snapshot";

const TABLE_PROJECTS: &str = "work_projects";
const TABLE_MEMORIES: &str = "work_memories";
const TABLE_REVISIONS: &str = "work_revisions";
const TABLE_EVENTS: &str = "work_events";
/// Optional telemetry table. Its presence is tolerated, but it is excluded from
/// the snapshot and never introspected beyond its identity as a table.
const TABLE_ACTIVITY: &str = "agent_activity";

/// Tables whose rows end up in the snapshot, and which must all be present.
const SNAPSHOT_TABLES: [&str; 4] = [
    TABLE_PROJECTS,
    TABLE_MEMORIES,
    TABLE_REVISIONS,
    TABLE_EVENTS,
];

/// One column of a table whose exact declaration is part of the contract.
#[derive(Clone, Copy)]
struct ColumnSpec {
    name: &'static str,
    ty: &'static str,
}

static PROJECT_COLUMNS: [ColumnSpec; 4] = [
    ColumnSpec {
        name: "id",
        ty: "TEXT",
    },
    ColumnSpec {
        name: "name",
        ty: "TEXT",
    },
    ColumnSpec {
        name: "directory",
        ty: "TEXT",
    },
    ColumnSpec {
        name: "created_at",
        ty: "TEXT",
    },
];

static MEMORY_COLUMNS: [ColumnSpec; 4] = [
    ColumnSpec {
        name: "id",
        ty: "TEXT",
    },
    ColumnSpec {
        name: "memory_scope",
        ty: "TEXT",
    },
    ColumnSpec {
        name: "project_id",
        ty: "TEXT",
    },
    ColumnSpec {
        name: "body",
        ty: "TEXT",
    },
];

static REVISION_COLUMNS: [ColumnSpec; 3] = [
    ColumnSpec {
        name: "memory_id",
        ty: "TEXT",
    },
    ColumnSpec {
        name: "version",
        ty: "INTEGER",
    },
    ColumnSpec {
        name: "body",
        ty: "TEXT",
    },
];

static EVENT_COLUMNS: [ColumnSpec; 3] = [
    ColumnSpec {
        name: "event_id",
        ty: "TEXT",
    },
    ColumnSpec {
        name: "request",
        ty: "TEXT",
    },
    ColumnSpec {
        name: "response",
        ty: "TEXT",
    },
];

fn expected_columns(table: &str) -> Option<&'static [ColumnSpec]> {
    match table {
        TABLE_PROJECTS => Some(&PROJECT_COLUMNS),
        TABLE_MEMORIES => Some(&MEMORY_COLUMNS),
        TABLE_REVISIONS => Some(&REVISION_COLUMNS),
        TABLE_EVENTS => Some(&EVENT_COLUMNS),
        _ => None,
    }
}

const SELECT_PROJECTS: &str =
    "SELECT id, name, directory, created_at FROM work_projects ORDER BY id";

const SELECT_MEMORIES: &str =
    "SELECT id, memory_scope, project_id, body FROM work_memories ORDER BY id";

const SELECT_REVISIONS: &str =
    "SELECT memory_id, version, body FROM work_revisions ORDER BY memory_id, version";

const SELECT_EVENTS: &str = "SELECT event_id, request, response FROM work_events ORDER BY event_id";

// `CAST(x AS BLOB)` yields the exact stored byte sequence, so `length(...)` is
// the UTF-8 byte count. Nullable columns are wrapped in `COALESCE` because both
// `length(NULL)` and `SUM` over an empty table are `NULL`.
const SIZE_PROJECTS: &str = "SELECT COALESCE(SUM(length(CAST(id AS BLOB))), 0) \
     + COALESCE(SUM(length(CAST(name AS BLOB))), 0) \
     + COALESCE(SUM(length(CAST(directory AS BLOB))), 0) \
     + COALESCE(SUM(length(CAST(created_at AS BLOB))), 0) \
     FROM work_projects";

const SIZE_MEMORIES: &str = "SELECT COALESCE(SUM(length(CAST(id AS BLOB))), 0) \
     + COALESCE(SUM(length(CAST(memory_scope AS BLOB))), 0) \
     + COALESCE(SUM(length(CAST(project_id AS BLOB))), 0) \
     + COALESCE(SUM(length(CAST(body AS BLOB))), 0) \
     FROM work_memories";

const SIZE_REVISIONS: &str = "SELECT COALESCE(SUM(length(CAST(memory_id AS BLOB))), 0) \
     + COALESCE(SUM(length(CAST(version AS BLOB))), 0) \
     + COALESCE(SUM(length(CAST(body AS BLOB))), 0) \
     FROM work_revisions";

const SIZE_EVENTS: &str = "SELECT COALESCE(SUM(length(CAST(event_id AS BLOB))), 0) \
     + COALESCE(SUM(length(CAST(request AS BLOB))), 0) \
     + COALESCE(SUM(length(CAST(response AS BLOB))), 0) \
     FROM work_events";

const SIZE_QUERIES: [&str; 4] = [SIZE_PROJECTS, SIZE_MEMORIES, SIZE_REVISIONS, SIZE_EVENTS];

/// Reads a consistent logical snapshot of the workspace at `path`.
///
/// The caller owns serialization of the returned [`Snapshot`].
pub(super) fn read_snapshot(path: &Path) -> Result<Snapshot, String> {
    read_snapshot_with_hook(path, || {})
}

/// [`read_snapshot`] with a test hook that runs while the read transaction is
/// still open, immediately after the project rows have been collected.
///
/// The hook is not invoked if the snapshot fails earlier (open, schema
/// validation, integrity, byte budget). Production callers pass `|| {}`.
pub(super) fn read_snapshot_with_hook(
    path: &Path,
    after_projects: impl FnOnce(),
) -> Result<Snapshot, String> {
    // Read-only and no mutex: the file must already exist, and nothing below may
    // create, migrate or reconfigure it. `MemoryStore::open` is intentionally not
    // used because it creates the schema and switches to WAL.
    let mut db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| ERR_OPEN.to_string())?;
    db.busy_timeout(Duration::from_secs(3))
        .map_err(|_| ERR_OPEN.to_string())?;

    // Deferred: the read snapshot is only pinned by the first statement below,
    // but it then covers every inspection and every SELECT.
    let txn = db
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(|_| ERR_OPEN.to_string())?;

    validate_user_version(&txn)?;
    inspect_tables(&txn)?;
    validate_columns(&txn)?;
    check_integrity(&txn)?;
    enforce_size_budget(&txn)?;

    let projects = read_projects(&txn)?;
    after_projects();
    let memories = read_memories(&txn)?;
    let revisions = read_revisions(&txn)?;
    let events = read_events(&txn)?;

    txn.commit().map_err(|_| ERR_READ.to_string())?;

    Ok(Snapshot {
        projects,
        memories,
        revisions,
        events,
    })
}

/// Any `user_version` other than 0 means the file was written by a different
/// build and cannot be interpreted by this one.
fn validate_user_version(db: &Connection) -> Result<(), String> {
    let version: i64 = db
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|_| ERR_SCHEMA.to_string())?;
    if version != 0 {
        return Err(ERR_SCHEMA.to_string());
    }
    Ok(())
}

/// Accepts exactly the known tables plus the optional telemetry table.
///
/// Views are rejected; indexes and triggers are not data-bearing and are
/// ignored, as are SQLite's own `sqlite_*` bookkeeping objects.
fn inspect_tables(db: &Connection) -> Result<(), String> {
    let mut stmt = db
        .prepare("SELECT type, name FROM sqlite_master WHERE type IN ('table', 'view')")
        .map_err(|_| ERR_SCHEMA.to_string())?;

    let entries = {
        let mapped = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|_| ERR_SCHEMA.to_string())?;
        mapped
            .collect::<rusqlite::Result<Vec<(String, String)>>>()
            .map_err(|_| ERR_SCHEMA.to_string())?
    };

    let mut present: BTreeSet<String> = BTreeSet::new();
    for (kind, name) in entries {
        if name.starts_with("sqlite_") {
            continue;
        }
        if kind == "table" && (SNAPSHOT_TABLES.contains(&name.as_str()) || name == TABLE_ACTIVITY) {
            present.insert(name);
        } else {
            // Unknown application table, or a view: not part of the format.
            return Err(ERR_SCHEMA.to_string());
        }
    }

    for required in SNAPSHOT_TABLES {
        if !present.contains(required) {
            return Err(ERR_SCHEMA.to_string());
        }
    }
    Ok(())
}

/// Verifies `table_xinfo` for every snapshotted table: same column count, same
/// order, same names, same declared types, and no hidden columns.
///
/// `agent_activity` is excluded telemetry and is deliberately not introspected.
fn validate_columns(db: &Connection) -> Result<(), String> {
    for table in SNAPSHOT_TABLES {
        let expected = expected_columns(table).ok_or_else(|| ERR_SCHEMA.to_string())?;
        validate_table_columns(db, table, expected)?;
    }
    Ok(())
}

fn validate_table_columns(
    db: &Connection,
    table: &str,
    expected: &[ColumnSpec],
) -> Result<(), String> {
    // `table` comes from the compile-time constants above, never from input, so
    // interpolating it into the pragma cannot be influenced by the file.
    let sql = format!("PRAGMA table_xinfo({table})");
    let mut stmt = db.prepare(&sql).map_err(|_| ERR_SCHEMA.to_string())?;

    let actual = {
        let mapped = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(1)?, // name
                    row.get::<_, String>(2)?, // declared type
                    row.get::<_, i64>(6)?,    // hidden
                ))
            })
            .map_err(|_| ERR_SCHEMA.to_string())?;
        mapped
            .collect::<rusqlite::Result<Vec<(String, String, i64)>>>()
            .map_err(|_| ERR_SCHEMA.to_string())?
    };

    if actual.len() != expected.len() {
        return Err(ERR_SCHEMA.to_string());
    }
    for ((name, ty, hidden), spec) in actual.iter().zip(expected.iter()) {
        if *hidden != 0 || name != spec.name || !ty.trim().eq_ignore_ascii_case(spec.ty) {
            return Err(ERR_SCHEMA.to_string());
        }
    }
    Ok(())
}

/// `quick_check(1)` must report `ok` even though the file was opened read-only.
fn check_integrity(db: &Connection) -> Result<(), String> {
    let result: String = db
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|_| ERR_INTEGRITY.to_string())?;
    if result != "ok" {
        return Err(ERR_INTEGRITY.to_string());
    }
    Ok(())
}

/// Totals the UTF-8 bytes of every TEXT column before any row is materialised,
/// so an oversized workspace is rejected without allocating its rows first.
fn enforce_size_budget(db: &Connection) -> Result<(), String> {
    let mut total: u64 = 0;
    for sql in SIZE_QUERIES {
        let sum: i64 = db
            .query_row(sql, [], |row| row.get(0))
            .map_err(|_| ERR_READ.to_string())?;
        if sum < 0 {
            return Err(ERR_TOO_LARGE.to_string());
        }
        total = total
            .checked_add(sum as u64)
            .ok_or_else(|| ERR_TOO_LARGE.to_string())?;
    }
    if total > MAX_BYTES as u64 {
        return Err(ERR_TOO_LARGE.to_string());
    }
    Ok(())
}

fn read_projects(db: &Connection) -> Result<Vec<ProjectRow>, String> {
    let mut stmt = db
        .prepare(SELECT_PROJECTS)
        .map_err(|_| ERR_READ.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ProjectRow {
                id: row.get(0)?,
                name: row.get(1)?,
                directory: row.get(2)?,
                created_at: row.get(3)?,
            })
        })
        .map_err(|_| ERR_READ.to_string())?;
    rows.collect::<rusqlite::Result<Vec<ProjectRow>>>()
        .map_err(|_| ERR_READ.to_string())
}

fn read_memories(db: &Connection) -> Result<Vec<MemoryRow>, String> {
    let mut stmt = db
        .prepare(SELECT_MEMORIES)
        .map_err(|_| ERR_READ.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(MemoryRow {
                id: row.get(0)?,
                memory_scope: row.get(1)?,
                project_id: row.get(2)?,
                body: row.get(3)?,
            })
        })
        .map_err(|_| ERR_READ.to_string())?;
    rows.collect::<rusqlite::Result<Vec<MemoryRow>>>()
        .map_err(|_| ERR_READ.to_string())
}

fn read_revisions(db: &Connection) -> Result<Vec<RevisionRow>, String> {
    let mut stmt = db
        .prepare(SELECT_REVISIONS)
        .map_err(|_| ERR_READ.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|_| ERR_READ.to_string())?;
    let raw = rows
        .collect::<rusqlite::Result<Vec<(String, i64, String)>>>()
        .map_err(|_| ERR_READ.to_string())?;

    let mut revisions = Vec::with_capacity(raw.len());
    for (memory_id, version, body) in raw {
        if version < 0 {
            return Err(ERR_READ.to_string());
        }
        revisions.push(RevisionRow {
            memory_id,
            version: version as u64,
            body,
        });
    }
    Ok(revisions)
}

fn read_events(db: &Connection) -> Result<Vec<EventRow>, String> {
    let mut stmt = db
        .prepare(SELECT_EVENTS)
        .map_err(|_| ERR_READ.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(EventRow {
                event_id: row.get(0)?,
                request: row.get(1)?,
                response: row.get(2)?,
            })
        })
        .map_err(|_| ERR_READ.to_string())?;
    rows.collect::<rusqlite::Result<Vec<EventRow>>>()
        .map_err(|_| ERR_READ.to_string())
}

//! Explicit, local backup operations. No default workspace or restore side effects.
use crate::backup_format::{self, Counts, Snapshot, MAX_BYTES};
use rusqlite::{Connection, TransactionBehavior};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

mod snapshot;

/// Database file name every operation targets inside a data directory.
const DB_NAME: &str = "work-memory.db";

fn counts(snapshot: &Snapshot) -> Counts {
    Counts {
        projects: snapshot.projects.len(),
        memories: snapshot.memories.len(),
        revisions: snapshot.revisions.len(),
        events: snapshot.events.len(),
    }
}

/// Read the complete logical workspace in one transaction, then publish a new
/// backup. Existing files, including symlinks, are never replaced.
pub fn export_workspace(data_dir: &Path, output: &Path) -> Result<Counts, String> {
    match fs::symlink_metadata(output) {
        Ok(_) => return Err("output already exists; choose a new backup filename".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("cannot inspect output path".into()),
    }
    let snapshot = snapshot::read_snapshot(&data_dir.join(DB_NAME))?;
    let summary = counts(&snapshot);
    let bytes = backup_format::encode(snapshot, &chrono::Utc::now().to_rfc3339())?;
    publish_with_writer(output, |file| file.write_all(&bytes), || {})?;
    Ok(summary)
}

/// Validate a backup without opening a database or changing any files.
pub fn verify_file(path: &Path) -> Result<Counts, String> {
    Ok(counts(&read_backup(path)?))
}

/// Restore a validated backup into an empty target data directory.
///
/// The order is fixed and observable: the backup is fully `decode`d and sized
/// first, so an invalid input never creates, replaces or truncates anything. The
/// target must already exist and be **completely empty**: any entry at all —
/// `work-memory.db`, an unrelated file, a stray directory — is refused, so a
/// workspace is never overwritten and unrelated user data is never silently
/// joined by restore output. The database is then materialised through a
/// create-new connection inside a same-filesystem UUID staging directory, synced
/// with `sync_all`, and only then published with a no-clobber hard link. Any
/// failure removes this operation's staging file and leaves the target directory
/// exactly as it was.
///
/// `dry_run` performs the validation, the target check and the budget check,
/// then reports the counts without writing any file or directory.
pub fn restore_workspace(input: &Path, data_dir: &Path, dry_run: bool) -> Result<Counts, String> {
    // 1. Validate the backup before touching the filesystem state of the target.
    // The full snapshot is kept in memory: `encode` rejects a payload above the
    // same budget, so no larger snapshot can have produced a valid backup file.
    let snapshot = read_backup(input)?;
    let summary = counts(&snapshot);

    // 2. The target directory must exist, be a directory, and hold no entry at
    // all. This is the same rule the shipped desktop UI applies
    // (`Directory.EnumerateFileSystemEntries(target).Take(1)`), so the CLI and the
    // UI cannot disagree about the same action. `read_dir` errors (permissions,
    // disappearing directory) fail closed: they must never be mistaken for
    // "empty" and silently restored into.
    if !data_dir.is_dir() {
        return Err("restore target must be an existing directory".into());
    }
    let mut entries =
        fs::read_dir(data_dir).map_err(|_| "cannot inspect restore target".to_string())?;
    match entries.next() {
        // Completely empty: restore may proceed.
        None => {}
        // Any entry at all — `work-memory.db`, an unrelated file, a stray
        // directory, or an entry that cannot even be read — makes the target
        // non-empty: refuse and leave it exactly as it is.
        Some(_) => return Err("restore requires an empty target directory".into()),
    }
    drop(entries);
    let destination = data_dir.join(DB_NAME);

    // 3. Nothing below this line writes when `--dry-run` was requested.
    if dry_run {
        return Ok(summary);
    }

    // 4. Stage beside the target so the final link is same-filesystem. The UUID
    // directory and its single database file are this operation's only
    // filesystem additions, and both are removed on every failure path.
    let staging = data_dir.join(format!(".graph-memory-restore-{}", uuid::Uuid::new_v4()));
    let mut staging_guard = StagingDir::new(staging.clone());
    if fs::create_dir(&staging).is_err() {
        return Err("cannot create restore staging directory".into());
    }
    let mut db_guard = Temporary(staging.join(DB_NAME));
    let written = write_restored_database(&db_guard.0, &snapshot);
    // Flush file data and metadata before the name becomes visible. The handle
    // needs write access: on Windows `sync_all` on a read-only handle is refused.
    let staged = written.and_then(|_| {
        OpenOptions::new()
            .write(true)
            .open(&db_guard.0)
            .and_then(|file| file.sync_all())
            .map_err(|_| "cannot synchronize restored database".to_string())
    });
    // Closing the SQLite connection inside `write_restored_database` must happen
    // before the destination name is published.
    staged?;

    // 5. Same-filesystem, no-clobber publication. Never copy or truncate the
    // destination, which a competing writer may have created meanwhile.
    let staged_path = db_guard.0.clone();
    fs::hard_link(&staged_path, &destination).map_err(|_| {
        "cannot publish restored database: target already exists or filesystem cannot create a hard link"
    })?;

    // 6. The database is published; drop the staging file and directory
    // best-effort so a successful restore is never reported as a failure over
    // cleanup alone.
    let _ = fs::remove_file(&staged_path);
    db_guard.disarm();
    let _ = fs::remove_dir(&staging);
    staging_guard.disarm();
    Ok(summary)
}

/// Read and fully validate a backup file without changing any file or directory.
fn read_backup(path: &Path) -> Result<Snapshot, String> {
    let file = File::open(path).map_err(|_| "cannot open backup file")?;
    let metadata = file.metadata().map_err(|_| "cannot inspect backup file")?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
        return Err("backup must be a regular file no larger than 64 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read backup file")?;
    backup_format::decode(&bytes)
}

/// Materialise `snapshot` as a fresh SQLite workspace at `path` in one
/// transaction, over the exact schema `MemoryStore::open` creates.
///
/// `path` must not exist: the connection is opened with `SQLITE_OPEN_CREATE`
/// but no `SQLITE_OPEN_READ_WRITE` on an existing path only because callers pass
/// a create-new staging file they just removed. WAL is switched on before the
/// transaction opens, so the committed data is durable in the database file and
/// no `-wal` sidecar is left behind.
fn write_restored_database(path: &Path, snapshot: &Snapshot) -> Result<(), String> {
    let mut db = Connection::open(path).map_err(|_| "cannot create restored database")?;
    db.busy_timeout(std::time::Duration::from_secs(3))
        .map_err(|_| "cannot configure restored database")?;
    db.execute_batch(
        "PRAGMA journal_mode=WAL;
        CREATE TABLE work_projects(id TEXT PRIMARY KEY,name TEXT NOT NULL,directory TEXT NOT NULL DEFAULT '',created_at TEXT NOT NULL);
        CREATE UNIQUE INDEX project_directory ON work_projects(directory) WHERE directory<>'';
        CREATE TABLE work_memories(id TEXT PRIMARY KEY,memory_scope TEXT NOT NULL,project_id TEXT,body TEXT NOT NULL);
        CREATE INDEX memory_scope_project ON work_memories(memory_scope,project_id);
        CREATE TABLE work_revisions(memory_id TEXT NOT NULL,version INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(memory_id,version));
        CREATE TABLE agent_activity(tool TEXT PRIMARY KEY,calls INTEGER NOT NULL,successes INTEGER NOT NULL,last_at TEXT NOT NULL,last_ok INTEGER NOT NULL);
        CREATE TABLE work_events(event_id TEXT PRIMARY KEY,request TEXT NOT NULL,response TEXT NOT NULL);",
    )
    .map_err(|_| "cannot create restored database schema")?;

    let transaction = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| "cannot start restore transaction")?;
    {
        let mut projects = transaction
            .prepare("INSERT INTO work_projects(id,name,directory,created_at) VALUES(?1,?2,?3,?4)")
            .map_err(|_| "cannot prepare restore statements")?;
        for row in &snapshot.projects {
            projects
                .execute((&row.id, &row.name, &row.directory, &row.created_at))
                .map_err(|_| "cannot write restored project rows")?;
        }
        let mut memories = transaction
            .prepare("INSERT INTO work_memories(id,memory_scope,project_id,body) VALUES(?1,?2,?3,?4)")
            .map_err(|_| "cannot prepare restore statements")?;
        for row in &snapshot.memories {
            memories
                .execute((
                    &row.id,
                    &row.memory_scope,
                    row.project_id.as_deref(),
                    &row.body,
                ))
                .map_err(|_| "cannot write restored memory rows")?;
        }
        let mut revisions = transaction
            .prepare("INSERT INTO work_revisions(memory_id,version,body) VALUES(?1,?2,?3)")
            .map_err(|_| "cannot prepare restore statements")?;
        for row in &snapshot.revisions {
            let version =
                i64::try_from(row.version).map_err(|_| "revision version out of range")?;
            revisions
                .execute((&row.memory_id, version, &row.body))
                .map_err(|_| "cannot write restored revision rows")?;
        }
        let mut events = transaction
            .prepare("INSERT INTO work_events(event_id,request,response) VALUES(?1,?2,?3)")
            .map_err(|_| "cannot prepare restore statements")?;
        for row in &snapshot.events {
            events
                .execute((&row.event_id, &row.request, &row.response))
                .map_err(|_| "cannot write restored event rows")?;
        }
    }
    transaction
        .commit()
        .map_err(|_| "cannot commit restored database")?;
    Ok(())
}

/// The single create-new staging database owned by one restore operation.
struct Temporary(PathBuf);
impl Temporary {
    fn disarm(self) {
        std::mem::forget(self);
    }
}
impl Drop for Temporary {
    fn drop(&mut self) {
        // Only this operation's create-new UUID temporary file, never a tree.
        let _ = fs::remove_file(&self.0);
    }
}

/// The UUID staging directory owned by one restore operation.
///
/// Untracked stale directories (a previous process killed between `create_dir`
/// and publication) are deliberately never removed: this operation only cleans
/// up what it created in this call.
struct StagingDir(Option<PathBuf>);
impl StagingDir {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }
    fn disarm(&mut self) {
        self.0 = None;
    }
}
impl Drop for StagingDir {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_dir(&path);
        }
    }
}

fn publish_with_writer(
    output: &Path,
    write: impl FnOnce(&mut File) -> std::io::Result<()>,
    before_publish: impl FnOnce(),
) -> Result<(), String> {
    let name = output.file_name().ok_or("output must name a file")?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent
        .canonicalize()
        .map_err(|_| "output parent directory must already exist")?;
    if !parent.is_dir() {
        return Err("output parent is not a directory".into());
    }
    let destination = parent.join(name);
    let temporary = parent.join(format!(".graph-memory-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| "cannot create backup staging file")?;
    let _cleanup = Temporary(temporary.clone());
    let staged = write(&mut file)
        .map_err(|_| "cannot write backup staging file")
        .and_then(|_| {
            file.sync_all()
                .map_err(|_| "cannot synchronize backup staging file")
        });
    drop(file);
    staged?;
    before_publish();
    // Same-filesystem link creation is no-clobber, unlike rename on Unix.
    // Unsupported filesystems fail closed; never copy/truncate the destination.
    fs::hard_link(&temporary, &destination).map_err(|_| {
        "cannot publish backup: destination exists or filesystem cannot create a hard link"
    })?;
    Ok(())
}

#[cfg(test)]
mod tests;

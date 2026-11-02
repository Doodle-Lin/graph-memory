//! Explicit, local backup operations. No default workspace or restore side effects.
use crate::backup_format::{self, Counts, Snapshot, MAX_BYTES};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

mod snapshot;

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
    let snapshot = snapshot::read_snapshot(&data_dir.join("work-memory.db"))?;
    let summary = counts(&snapshot);
    let bytes = backup_format::encode(snapshot, &chrono::Utc::now().to_rfc3339())?;
    publish_with_writer(output, |file| file.write_all(&bytes), || {})?;
    Ok(summary)
}

/// Validate a backup without opening a database or changing any files.
pub fn verify_file(path: &Path) -> Result<Counts, String> {
    let file = File::open(path).map_err(|_| "cannot open backup file")?;
    let metadata = file.metadata().map_err(|_| "cannot inspect backup file")?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES as u64 {
        return Err("backup must be a regular file no larger than 64 MiB".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read backup file")?;
    Ok(counts(&backup_format::decode(&bytes)?))
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        // Only this operation's create_new UUID temporary file, never a tree.
        let _ = fs::remove_file(&self.0);
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

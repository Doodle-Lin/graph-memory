use super::*;
use crate::memory_store::MemoryStore;
use rusqlite::Connection;
use serde_json::{json, Value};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("gm-backup-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn db(&self) -> PathBuf {
        self.0.join("work-memory.db")
    }
    fn output(&self) -> PathBuf {
        self.0.join("中文 backup.gmbackup")
    }
    fn no_staging(&self) -> bool {
        fs::read_dir(&self.0)
            .unwrap()
            .all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp"))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn create(store: &mut MemoryStore, event: &str) -> Value {
    store.route("POST", "/api/v1/memories", json!({"memory_scope":"personal", "event_id":event,
        "content":format!("合成备份测试内容 {event}"), "source":"fixture", "conditions":"test only"})).unwrap()
}

#[test]
fn snapshot_excludes_commits_between_table_reads_and_includes_wal() {
    let fixture = Fixture::new();
    let mut writer = MemoryStore::open(&fixture.db()).unwrap();
    let first = create(&mut writer, "before");
    assert!(
        fs::metadata(fixture.0.join("work-memory.db-wal"))
            .unwrap()
            .len()
            > 0
    );
    let snapshot = snapshot::read_snapshot_with_hook(&fixture.db(), || {
        let project = writer
            .route(
                "POST",
                "/api/v1/projects",
                json!({"name":"committed during read"}),
            )
            .unwrap();
        writer
            .route(
                "POST",
                "/api/v1/memories",
                json!({"memory_scope":"project","project_id":project["id"],
            "event_id":"during","content":"本记录必须与新项目一起排除"}),
            )
            .unwrap();
        writer
            .route(
                "POST",
                "/api/v1/memories/update",
                json!({"id":first["memory"]["id"],"expected_version":1,
            "content":"本次快照不应看见的第二版"}),
            )
            .unwrap();
    })
    .unwrap();
    assert_eq!(snapshot.projects.len(), 0);
    assert_eq!(snapshot.memories.len(), 1);
    assert_eq!(snapshot.revisions.len(), 1);
    assert_eq!(snapshot.events.len(), 1);
    let body: Value = serde_json::from_str(&snapshot.memories[0].body).unwrap();
    assert_eq!(body["version"], 1);
    backup_format::decode(&backup_format::encode(snapshot, "2026-11-02T00:00:00Z").unwrap())
        .unwrap();
    let later = snapshot::read_snapshot(&fixture.db()).unwrap();
    assert_eq!(
        (
            later.projects.len(),
            later.memories.len(),
            later.revisions.len(),
            later.events.len()
        ),
        (1, 2, 3, 2)
    );
}

#[test]
fn snapshot_ignores_uncommitted_transaction_and_excludes_telemetry() {
    let fixture = Fixture::new();
    let mut writer = MemoryStore::open(&fixture.db()).unwrap();
    create(&mut writer, "kept");
    writer
        .route(
            "POST",
            "/api/v1/agent/call",
            json!({"tool":"memory_projects","arguments":{}}),
        )
        .unwrap();
    let mut raw = Connection::open(fixture.db()).unwrap();
    let transaction = raw.transaction().unwrap();
    transaction.execute("DELETE FROM work_events", []).unwrap();
    let summary = export_workspace(&fixture.0, &fixture.output()).unwrap();
    assert_eq!(summary.events, 1);
    transaction.rollback().unwrap();
    let bytes = fs::read(fixture.output()).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("agent_activity"));
    assert_eq!(verify_file(&fixture.output()).unwrap(), summary);
}

#[test]
fn no_overwrite_missing_source_parent_and_failure_cleanup() {
    let fixture = Fixture::new();
    assert!(export_workspace(&fixture.0, &fixture.output()).is_err());
    assert!(!fixture.db().exists());
    assert!(!fixture.output().exists());
    let mut writer = MemoryStore::open(&fixture.db()).unwrap();
    create(&mut writer, "seed");
    export_workspace(&fixture.0, &fixture.output()).unwrap();
    let before = fs::read(fixture.output()).unwrap();
    create(&mut writer, "later");
    assert!(export_workspace(&fixture.0, &fixture.output()).is_err());
    assert_eq!(fs::read(fixture.output()).unwrap(), before);
    assert!(export_workspace(&fixture.0, &fixture.db()).is_err());
    assert!(export_workspace(&fixture.0, &fixture.0.join("absent/backup.gmbackup")).is_err());
    assert!(!fixture.0.join("absent").exists());
    let failed = fixture.0.join("failed.gmbackup");
    let result = publish_with_writer(
        &failed,
        |file| {
            file.write_all(b"partial contents")?;
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "injected write failure",
            ))
        },
        || {},
    );
    assert!(result.is_err());
    assert!(!failed.exists());
    assert!(fixture.no_staging());
    assert_eq!(
        writer
            .route("POST", "/api/v1/search", json!({"scope":"all"}))
            .unwrap()["total"],
        2
    );
}

#[test]
fn destination_created_during_publish_is_preserved() {
    let fixture = Fixture::new();
    let result = publish_with_writer(
        &fixture.output(),
        |file| file.write_all(b"candidate"),
        || {
            fs::write(fixture.output(), b"other writer wins").unwrap();
        },
    );
    assert!(result.is_err());
    assert_eq!(fs::read(fixture.output()).unwrap(), b"other writer wins");
    assert!(fixture.no_staging());
}

#[test]
fn unknown_schema_and_inconsistent_data_do_not_publish_or_repair() {
    for sql in [
        "PRAGMA user_version=99",
        "CREATE TABLE future_data(secret TEXT)",
        "ALTER TABLE work_projects ADD COLUMN new_data TEXT",
        "CREATE VIEW future_view AS SELECT id FROM work_projects",
        "DELETE FROM work_events",
    ] {
        let fixture = Fixture::new();
        let mut writer = MemoryStore::open(&fixture.db()).unwrap();
        create(&mut writer, "seed");
        drop(writer);
        let raw = Connection::open(fixture.db()).unwrap();
        raw.execute_batch(sql).unwrap();
        drop(raw);
        let before = fs::read(fixture.db()).unwrap();
        assert!(
            export_workspace(&fixture.0, &fixture.output()).is_err(),
            "accepted {sql}"
        );
        assert!(!fixture.output().exists());
        assert!(fixture.no_staging());
        assert_eq!(fs::read(fixture.db()).unwrap(), before);
    }
}

#[test]
fn verify_rejects_damage_and_does_not_change_input() {
    let fixture = Fixture::new();
    let mut writer = MemoryStore::open(&fixture.db()).unwrap();
    create(&mut writer, "seed");
    export_workspace(&fixture.0, &fixture.output()).unwrap();
    let original = fs::read(fixture.output()).unwrap();
    let mut unknown: Value = serde_json::from_slice(&original).unwrap();
    unknown["manifest"]["format_version"] = json!(99);
    let mut corrupt: Value = serde_json::from_slice(&original).unwrap();
    corrupt["manifest"]["payload_sha256"] = json!("0".repeat(64));
    for bytes in [
        original[..original.len() / 2].to_vec(),
        serde_json::to_vec(&unknown).unwrap(),
        serde_json::to_vec(&corrupt).unwrap(),
    ] {
        let bad = fixture.0.join("bad.gmbackup");
        fs::write(&bad, &bytes).unwrap();
        assert!(verify_file(&bad).is_err());
        assert_eq!(fs::read(bad).unwrap(), bytes);
    }
    assert_eq!(fs::read(fixture.output()).unwrap(), original);
    assert!(verify_file(&fixture.0).is_err());
}

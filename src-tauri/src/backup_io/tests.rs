use super::*;
use crate::backup_format::{EventRow, MemoryRow, RevisionRow};
use crate::memory_store::MemoryStore;
use rusqlite::Connection;
use serde_json::{json, Value};

/// One `Memory` body with the two P1 retraction fields, built as a JSON literal
/// so it does not depend on the `memory_store` API another owner is extending.
fn retracted_body(
    id: &str,
    version: u64,
    content: &str,
    retracted_at: Option<&str>,
    reason: Option<&str>,
) -> String {
    json!({
        "id": id,
        "version": version,
        "memory_scope": "personal",
        "project_id": null,
        "title": "撤回样本",
        "content": content,
        "kind": "note",
        "source": "fixture",
        "conditions": "test only",
        "evidence_type": "user_or_agent_statement",
        "origin_memory_id": null,
        "created_at": "2026-11-04T00:00:00+00:00",
        "updated_at": format!("2026-11-04T00:00:0{version}+00:00"),
        "retracted_at": retracted_at,
        "retract_reason": reason,
    })
    .to_string()
}

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
        // No `.graph-memory-*.tmp` export staging file and no
        // `.graph-memory-restore-*` restore staging directory may survive.
        fs::read_dir(&self.0).unwrap().all(|e| {
            let name = e.unwrap().file_name().to_string_lossy().into_owned();
            !name.starts_with(".graph-memory")
        })
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

/// A fresh empty directory beside the fixture workspace, uniquely named so
/// repeated calls in one test do not collide.
fn empty_target(fixture: &Fixture) -> PathBuf {
    let target = fixture
        .0
        .join(format!("restored-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&target).unwrap();
    target
}

fn entries(path: &Path) -> Vec<String> {
    let mut names = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn seed_workspace(fixture: &Fixture) -> Counts {
    let mut writer = MemoryStore::open(&fixture.db()).unwrap();
    let project = writer
        .route(
            "POST",
            "/api/v1/projects",
            json!({"name":"恢复测试项目","directory":"C:\\workspace\\restore"}),
        )
        .unwrap();
    create(&mut writer, "first");
    let second = create(&mut writer, "second");
    writer
        .route(
            "POST",
            "/api/v1/memories/update",
            json!({"id":second["memory"]["id"],"expected_version":1,"content":"第二版内容"}),
        )
        .unwrap();
    writer
        .route(
            "POST",
            "/api/v1/memories",
            json!({"memory_scope":"project","project_id":project["id"],"event_id":"project-memory",
                "content":"项目范围内容"}),
        )
        .unwrap();
    export_workspace(&fixture.0, &fixture.output()).unwrap()
}

#[test]
fn restore_roundtrip_preserves_counts_and_opens_as_store() {
    let fixture = Fixture::new();
    let exported = seed_workspace(&fixture);
    assert_eq!((exported.projects, exported.memories), (1, 3));
    let target = empty_target(&fixture);

    assert_eq!(
        restore_workspace(&fixture.output(), &target, false).unwrap(),
        exported
    );

    // Exactly one published database, no staging leftovers.
    assert_eq!(entries(&target), vec!["work-memory.db".to_string()]);
    assert!(fixture.no_staging());

    // The logical snapshot of the restored file equals the exported one.
    let restored = target.join("work-memory.db");
    let snapshot = snapshot::read_snapshot(&restored).unwrap();
    assert_eq!(counts(&snapshot), exported);
    assert_eq!(snapshot.projects.len(), 1);
    assert_eq!(snapshot.memories.len(), 3);
    assert_eq!(snapshot.revisions.len(), 4);
    assert_eq!(snapshot.events.len(), 3);

    // And a real `MemoryStore` can open and query it.
    let mut reader = MemoryStore::open(&restored).unwrap();
    let listed = reader
        .route("POST", "/api/v1/search", json!({"scope":"all"}))
        .unwrap();
    assert_eq!(listed["total"], exported.memories);
}

#[test]
fn restore_refuses_non_empty_target_and_never_touches_it() {
    let fixture = Fixture::new();
    let exported = seed_workspace(&fixture);
    let target = empty_target(&fixture);
    let occupied = target.join("work-memory.db");
    fs::write(&occupied, b"existing workspace").unwrap();

    let error = restore_workspace(&fixture.output(), &target, false).unwrap_err();
    assert_eq!(error, "restore requires an empty target directory");
    assert_eq!(fs::read(&occupied).unwrap(), b"existing workspace");
    assert_eq!(entries(&target), vec!["work-memory.db".to_string()]);
    assert_eq!(
        verify_file(&fixture.output()).unwrap(),
        exported,
        "rejected restore must not modify the backup"
    );

    // An absent target directory is an error and is not created.
    let absent = fixture.0.join("absent-target");
    assert!(restore_workspace(&fixture.output(), &absent, false).is_err());
    assert!(!absent.exists());

    // A plain file instead of a directory is rejected as well.
    let file_target = fixture.0.join("not-a-dir");
    fs::write(&file_target, b"x").unwrap();
    assert!(restore_workspace(&fixture.output(), &file_target, false).is_err());
    assert_eq!(fs::read(&file_target).unwrap(), b"x");
}

#[test]
fn restore_dry_run_reports_counts_without_writing_files() {
    let fixture = Fixture::new();
    let exported = seed_workspace(&fixture);
    let target = empty_target(&fixture);

    assert_eq!(
        restore_workspace(&fixture.output(), &target, true).unwrap(),
        exported
    );
    assert!(entries(&target).is_empty());
    // Dry-run must not even stage anything beside the target.
    assert!(fixture.no_staging());
    assert!(!target.join("work-memory.db").exists());
}

#[test]
fn restore_rejects_damaged_backup_and_leaves_target_empty() {
    let fixture = Fixture::new();
    seed_workspace(&fixture);
    let original = fs::read(fixture.output()).unwrap();
    let mut corrupt: Value = serde_json::from_slice(&original).unwrap();
    corrupt["manifest"]["payload_sha256"] = json!("0".repeat(64));

    for bytes in [
        original[..original.len() / 2].to_vec(),
        serde_json::to_vec(&corrupt).unwrap(),
        b"not a backup at all".to_vec(),
    ] {
        let target = empty_target(&fixture);
        let bad = fixture.0.join(format!("bad-{}.gmbackup", uuid::Uuid::new_v4()));
        fs::write(&bad, &bytes).unwrap();
        assert!(restore_workspace(&bad, &target, false).is_err());
        assert!(entries(&target).is_empty(), "damaged backup wrote files");
        assert_eq!(fs::read(&bad).unwrap(), bytes);
    }
    assert!(fixture.no_staging());
}

/// A workspace whose memory has four versions: statement, retracted with a
/// reason, restored (fields null again) and then retracted again with a
/// different reason, so no two versions agree on the mutable retraction fields.
fn retracted_snapshot() -> Snapshot {
    let id = "retracted-memory";
    let request = |version: u64, content: &str| {
        format!(
            "{{\"event_id\":\"retract-event-{version}\",\"memory_scope\":\"personal\",\"project_id\":null,\"content\":\"{content}\"}}"
        )
    };
    let response = |body: &str| format!("{{\"memory\":{body}}}");
    let bodies = [
        retracted_body(id, 1, "初版结论", None, None),
        retracted_body(
            id,
            2,
            "初版结论",
            Some("2026-11-04T01:00:00+00:00"),
            Some("结论被证伪"),
        ),
        retracted_body(id, 3, "初版结论", None, None),
        retracted_body(
            id,
            4,
            "初版结论",
            Some("2026-11-04T02:00:00+00:00"),
            Some("再次被证伪"),
        ),
    ];
    Snapshot {
        projects: vec![],
        memories: vec![MemoryRow {
            id: id.into(),
            memory_scope: "personal".into(),
            project_id: None,
            body: bodies[3].clone(),
        }],
        revisions: bodies
            .iter()
            .enumerate()
            .map(|(index, body)| RevisionRow {
                memory_id: id.into(),
                version: index as u64 + 1,
                body: body.clone(),
            })
            .collect(),
        events: vec![EventRow {
            event_id: "retract-event-1".into(),
            request: request(1, "初版结论"),
            response: response(&bodies[0]),
        }],
    }
}

#[test]
fn legacy_export_loses_nothing_when_retraction_fields_appear() {
    let fixture = Fixture::new();
    let exported = seed_workspace(&fixture);
    let bytes = fs::read(fixture.output()).unwrap();
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(envelope["manifest"]["format_version"], 1);

    // A legacy envelope still verifies and restores while its bodies have none
    // of the new fields: the additions are optional, not required.
    assert_eq!(verify_file(&fixture.output()).unwrap(), exported);
    let target = empty_target(&fixture);
    assert_eq!(
        restore_workspace(&fixture.output(), &target, false).unwrap(),
        exported
    );
    assert_eq!(
        counts(&snapshot::read_snapshot(&target.join("work-memory.db")).unwrap()),
        exported
    );

    // The same legacy payload with the fields added to every body, and the
    // checksum recomputed, decodes unchanged: nothing is rejected or dropped.
    // `retracted_at`/`retract_reason` are optional, so the current row's
    // retraction state must keep the version that carries it, and every body
    // must survive verbatim.
    let mut extended = snapshot::read_snapshot(&fixture.db()).unwrap();
    let stamp = |body: &mut Value| {
        let retired = body["content"] == json!("第二版内容");
        body["retracted_at"] = if retired {
            json!("2026-11-04T03:00:00+00:00")
        } else {
            Value::Null
        };
        body["retract_reason"] = if retired {
            json!("旧包补写撤回字段")
        } else {
            Value::Null
        };
    };
    for row in extended.memories.iter_mut() {
        let mut body: Value = serde_json::from_str(&row.body).unwrap();
        stamp(&mut body);
        row.body = body.to_string();
    }
    for row in extended.revisions.iter_mut() {
        let mut body: Value = serde_json::from_str(&row.body).unwrap();
        stamp(&mut body);
        row.body = body.to_string();
    }
    let decoded = backup_format::decode(
        &backup_format::encode(extended.clone(), "2026-11-04T00:00:00+00:00").unwrap(),
    )
    .unwrap();
    assert_eq!(counts(&decoded), exported);
    let current: Value = serde_json::from_str(
        &decoded
            .memories
            .iter()
            .find(|row| row.body.contains("第二版内容"))
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(current["retracted_at"], "2026-11-04T03:00:00+00:00");
    assert_eq!(current["retract_reason"], "旧包补写撤回字段");
    let id = current["id"].as_str().unwrap().to_owned();
    let revision: Value = serde_json::from_str(
        &decoded
            .revisions
            .iter()
            .find(|row| row.memory_id == id && row.version == 1)
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(revision["retracted_at"], Value::Null);
    assert_eq!(revision["retract_reason"], Value::Null);
    assert!(revision["content"].as_str().unwrap().contains("second"));
}

#[test]
fn retracted_workspace_round_trips_through_cli_export_verify_restore() {
    let fixture = Fixture::new();
    let snapshot = retracted_snapshot();
    let bytes = backup_format::encode(snapshot.clone(), "2026-11-04T00:00:00+00:00").unwrap();
    fs::write(fixture.output(), &bytes).unwrap();

    // verify accepts the retraction fields and reports their rows.
    assert_eq!(
        verify_file(&fixture.output()).unwrap(),
        Counts {
            projects: 0,
            memories: 1,
            revisions: 4,
            events: 1,
        }
    );

    let target = empty_target(&fixture);
    assert_eq!(
        restore_workspace(&fixture.output(), &target, false).unwrap(),
        verify_file(&fixture.output()).unwrap()
    );

    // The restored database keeps every version and the retraction state.
    let restored = snapshot::read_snapshot(&target.join("work-memory.db")).unwrap();
    assert_eq!(counts(&restored), counts(&snapshot));
    assert_eq!(restored.revisions.len(), 4);
    let body_at = |version: u64| -> Value {
        serde_json::from_str(
            &restored
                .revisions
                .iter()
                .find(|row| row.version == version)
                .unwrap()
                .body,
        )
        .unwrap()
    };
    assert_eq!(body_at(1)["retracted_at"], Value::Null);
    assert_eq!(body_at(2)["retracted_at"], "2026-11-04T01:00:00+00:00");
    assert_eq!(body_at(2)["retract_reason"], "结论被证伪");
    assert_eq!(body_at(3)["retracted_at"], Value::Null);
    assert_eq!(body_at(4)["retract_reason"], "再次被证伪");
    let current: Value = serde_json::from_str(&restored.memories[0].body).unwrap();
    assert_eq!(current["version"], 4);
    assert_eq!(current["retracted_at"], "2026-11-04T02:00:00+00:00");
    assert_eq!(current["retract_reason"], "再次被证伪");

    // Re-exporting the restored directory reproduces the original payload.
    let reexported = fixture.0.join("retracted-reexport.gmbackup");
    assert_eq!(
        export_workspace(&target, &reexported).unwrap(),
        counts(&snapshot)
    );
    let original: Value = serde_json::from_slice(&bytes).unwrap();
    let again: Value = serde_json::from_slice(&fs::read(&reexported).unwrap()).unwrap();
    assert_eq!(
        original["manifest"]["payload_sha256"],
        again["manifest"]["payload_sha256"]
    );
}

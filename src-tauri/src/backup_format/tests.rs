use super::*;
use crate::memory_store::MemoryStore;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::path::PathBuf;

const DATE: &str = "2026-11-02T12:00:00+08:00";

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Read a synthetic database produced by the actual current API. This fixture
// is NOT the production snapshot implementation (L2-02); no concurrent writer.
fn sample(content: &str) -> Snapshot {
    let fixture = Fixture(std::env::temp_dir().join(format!("gm-format-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&fixture.0).unwrap();
    let path = fixture.0.join("work-memory.db");
    let mut store = MemoryStore::open(&path).unwrap();
    let p = store
        .route(
            "POST",
            "/api/v1/projects",
            json!({"name":"中文项目🚀", "directory":"C:/合成项目"}),
        )
        .unwrap();
    store
        .route(
            "POST",
            "/api/v1/projects",
            json!({"name":"Another project"}),
        )
        .unwrap();
    let source = store.route("POST", "/api/v1/memories", json!({"memory_scope":"project", "project_id":p["id"],
        "event_id":"project-event", "content":"原案例：仅供格式测试。", "source":"fixture://case", "conditions":"test only"})).unwrap();
    let personal = store.route("POST", "/api/v1/memories", json!({"memory_scope":"personal", "event_id":" personal-event ",
        "origin_memory_id":source["memory"]["id"], "content":content, "title":"经验🚀", "kind":"", "source":"fixture://source",
        "conditions":"离线测试", "retained_unknown":{"request_metadata":"必须保留"}})).unwrap();
    store
        .route(
            "POST",
            "/api/v1/memories/update",
            json!({"id":personal["memory"]["id"],"expected_version":1,
        "content":"修订后的合成内容", "kind":"自定义类型", "source":"fixture://revised"}),
        )
        .unwrap();
    store
        .route(
            "POST",
            "/api/v1/memories/undo",
            json!({"id":personal["memory"]["id"],"expected_version":2}),
        )
        .unwrap();
    drop(store);
    let db = Connection::open(&path).unwrap();
    let projects = db
        .prepare("SELECT id,name,directory,created_at FROM work_projects ORDER BY id DESC")
        .unwrap()
        .query_map([], |r| {
            Ok(ProjectRow {
                id: r.get(0)?,
                name: r.get(1)?,
                directory: r.get(2)?,
                created_at: r.get(3)?,
            })
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let memories = db
        .prepare("SELECT id,memory_scope,project_id,body FROM work_memories ORDER BY id DESC")
        .unwrap()
        .query_map([], |r| {
            Ok(MemoryRow {
                id: r.get(0)?,
                memory_scope: r.get(1)?,
                project_id: r.get(2)?,
                body: r.get(3)?,
            })
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let revisions = db.prepare("SELECT memory_id,version,body FROM work_revisions ORDER BY memory_id DESC,version DESC").unwrap()
        .query_map([], |r| Ok(RevisionRow{memory_id:r.get(0)?,version:r.get(1)?,body:r.get(2)?})).unwrap()
        .collect::<Result<Vec<_>,_>>().unwrap();
    let events = db
        .prepare("SELECT event_id,request,response FROM work_events ORDER BY event_id DESC")
        .unwrap()
        .query_map([], |r| {
            Ok(EventRow {
                event_id: r.get(0)?,
                request: r.get(1)?,
                response: r.get(2)?,
            })
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    drop(db);
    Snapshot {
        projects,
        memories,
        revisions,
        events,
    }
}

fn unchecked(mut payload: Snapshot) -> Vec<u8> {
    normalize(&mut payload);
    serde_json::to_vec(&Backup {
        manifest: Manifest {
            format: FORMAT.into(),
            format_version: 1,
            exported_at: DATE.into(),
            counts: counts_of(&payload),
            payload_sha256: digest_of(&payload).unwrap(),
        },
        payload,
    })
    .unwrap()
}

#[test]
fn api_history_unicode_long_text_and_event_bytes_round_trip() {
    let original = sample(&"中文🚀\n引号\"反斜线\\与制表\t".repeat(24000));
    let bytes = encode(original.clone(), DATE).unwrap();
    let decoded = decode(&bytes).unwrap();
    let mut expected = original.clone();
    normalize(&mut expected);
    assert_eq!(decoded, expected);
    assert_eq!(decoded.revisions.len(), 4);
    let current: Value = serde_json::from_str(
        &decoded
            .memories
            .iter()
            .find(|m| m.memory_scope == "personal")
            .unwrap()
            .body,
    )
    .unwrap();
    assert_eq!(current["version"], 3);
    assert_eq!(current["kind"], ""); // undo preserved the original unknown/empty kind
    assert!(current["origin_memory_id"].is_string());
    assert!(decoded
        .events
        .iter()
        .any(|e| e.request.contains("retained_unknown")));
    for event in original.events {
        let replay = decoded
            .events
            .iter()
            .find(|e| e.event_id == event.event_id)
            .unwrap();
        assert_eq!(replay.request.as_bytes(), event.request.as_bytes());
        assert_eq!(replay.response.as_bytes(), event.response.as_bytes());
    }
    assert_eq!(encode(decoded, DATE).unwrap(), bytes);
    let reordered_outer: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        decode(&serde_json::to_vec(&reordered_outer).unwrap()).unwrap(),
        expected
    );
}

#[test]
fn deterministic_order_and_golden_empty_digest() {
    let a = sample("合成内容：无需真实宿主资料");
    let mut b = a.clone();
    b.projects.reverse();
    b.memories.reverse();
    b.revisions.reverse();
    b.events.reverse();
    assert_eq!(encode(a, DATE).unwrap(), encode(b, DATE).unwrap());
    let empty = Snapshot {
        projects: vec![],
        memories: vec![],
        revisions: vec![],
        events: vec![],
    };
    let bytes = encode(empty.clone(), DATE).unwrap();
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        envelope["manifest"]["payload_sha256"],
        "2041f17a0ed4b990bda9b60cbc85960201f42f87dead4eb3d28aebb24f6dcada"
    );
    assert_eq!(decode(&bytes).unwrap(), empty);
}

#[test]
fn rejects_corruption_unknown_fields_versions_and_counts() {
    let bytes = encode(sample("不应泄漏的合成正文"), DATE).unwrap();
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    let mut bad = envelope.clone();
    bad["manifest"]["format_version"] = json!(2);
    assert!(decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    bad = envelope.clone();
    bad["manifest"]["format"] = json!("other-format");
    assert!(decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    bad = envelope.clone();
    bad["manifest"]["counts"]["memories"] = json!(0);
    assert!(decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    bad = envelope.clone();
    bad["manifest"]["exported_at"] = json!("yesterday");
    assert!(decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    bad = envelope.clone();
    bad["payload"]["projects"][0]["name"] = json!("changed");
    assert!(decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    for pointer in ["", "/manifest", "/payload", "/payload/projects/0"] {
        bad = envelope.clone();
        bad.pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unknown_field".into(), json!(true));
        assert!(decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
    bad = envelope.clone();
    let personal = bad["payload"]["memories"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|row| row["memory_scope"] == "personal")
        .unwrap();
    personal.as_object_mut().unwrap().remove("project_id");
    assert!(
        decode(&serde_json::to_vec(&bad).unwrap()).is_err(),
        "missing nullable key accepted"
    );
    let duplicate = String::from_utf8(bytes.clone()).unwrap().replacen(
        "\"format_version\": 1",
        "\"format_version\": 1, \"format_version\": 1",
        1,
    );
    assert!(decode(duplicate.as_bytes()).is_err());
    for digest in ["", "z".repeat(64).as_str(), "A".repeat(64).as_str()] {
        bad = envelope.clone();
        bad["manifest"]["payload_sha256"] = json!(digest);
        assert!(decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
    assert!(decode(&bytes[..bytes.len() / 2]).is_err());
    assert!(decode(b"not json ULTRA_PRIVATE_FIXTURE")
        .unwrap_err()
        .find("ULTRA_PRIVATE_FIXTURE")
        .is_none());
}

#[test]
fn rejects_broken_relations_even_with_recomputed_checksum() {
    let original = sample("关系校验合成样本");
    let mut variants = Vec::new();
    let mut bad = original.clone();
    bad.projects.clear();
    variants.push(bad);
    bad = original.clone();
    bad.memories[0].memory_scope = "wrong".into();
    variants.push(bad);
    bad = original.clone();
    bad.memories.push(bad.memories[0].clone());
    variants.push(bad);
    bad = original.clone();
    bad.projects.push(bad.projects[0].clone());
    variants.push(bad);
    bad = original.clone();
    bad.revisions.pop();
    variants.push(bad);
    bad = original.clone();
    bad.revisions.push(bad.revisions[0].clone());
    variants.push(bad);
    bad = original.clone();
    bad.events.clear();
    variants.push(bad);
    bad = original.clone();
    bad.events[0].request = "{}".into();
    variants.push(bad);
    bad = original.clone();
    bad.events[0].response = "{}".into();
    variants.push(bad);
    bad = original.clone();
    bad.events.push(bad.events[0].clone());
    variants.push(bad);
    bad = original.clone();
    bad.revisions[0].version = u64::MAX;
    variants.push(bad);
    bad = original.clone();
    bad.memories[0].body = "{}".into();
    variants.push(bad);
    bad = original.clone();
    bad.projects[0].directory = "same".into();
    bad.projects[1].directory = "same".into();
    variants.push(bad);
    for (key, value) in [
        ("origin_memory_id", json!("missing-origin")),
        ("content", json!("latest body mismatch")),
        ("created_at", json!("invalid timestamp")),
        ("version", json!(0)),
    ] {
        bad = original.clone();
        let mut body: Value = serde_json::from_str(&bad.memories[0].body).unwrap();
        body[key] = value;
        bad.memories[0].body = body.to_string();
        variants.push(bad);
    }
    bad = original.clone();
    let event = bad
        .events
        .iter_mut()
        .find(|e| e.event_id == "personal-event")
        .unwrap();
    let mut response: Value = serde_json::from_str(&event.response).unwrap();
    response["memory"]["version"] = json!(3);
    event.response = response.to_string();
    variants.push(bad);
    bad = original.clone();
    let mut request: Value = serde_json::from_str(&bad.events[0].request).unwrap();
    request["content"] = json!("different initial content");
    bad.events[0].request = request.to_string();
    variants.push(bad);
    for (index, bad) in variants.into_iter().enumerate() {
        assert!(
            encode(bad.clone(), DATE).is_err(),
            "encoder accepted corruption {index}"
        );
        assert!(
            decode(&unchecked(bad)).is_err(),
            "decoder trusted recomputed checksum {index}"
        );
    }
}

#[test]
fn preserves_additional_body_fields_and_raw_json_spacing() {
    let mut payload = sample("扩展字段合成样本");
    let row = payload
        .memories
        .iter_mut()
        .find(|r| r.memory_scope == "personal")
        .unwrap();
    let mut body: Value = serde_json::from_str(&row.body).unwrap();
    body["opaque_extension"] = json!({"unknown":"保留，不执行","counter":17});
    row.body = format!("\n {} \n", serde_json::to_string_pretty(&body).unwrap());
    let latest = payload
        .revisions
        .iter_mut()
        .find(|r| r.memory_id == row.id && r.version == 3)
        .unwrap();
    latest.body = row.body.clone();
    normalize(&mut payload);
    assert_eq!(
        decode(&encode(payload.clone(), DATE).unwrap()).unwrap(),
        payload
    );
}

#[test]
fn rejects_unsorted_payload_and_oversize_input() {
    let mut payload = decode(&encode(sample("排序合成样本"), DATE).unwrap()).unwrap();
    payload.projects.reverse();
    let bytes = serde_json::to_vec(&Backup {
        manifest: Manifest {
            format: FORMAT.into(),
            format_version: 1,
            exported_at: DATE.into(),
            counts: counts_of(&payload),
            payload_sha256: digest_of(&payload).unwrap(),
        },
        payload,
    })
    .unwrap();
    assert!(decode(&bytes).is_err());
    assert!(decode(&vec![b' '; MAX_BYTES + 1]).is_err());
}

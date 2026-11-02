use super::Snapshot;
use crate::memory_store::Memory;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

fn check(ok: bool, message: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

fn memory(raw: &str) -> Result<(Memory, Value), String> {
    let parsed: Memory = serde_json::from_str(raw).map_err(|_| "invalid memory body")?;
    let value: Value = serde_json::from_str(raw).map_err(|_| "invalid memory body")?;
    check(
        !parsed.id.trim().is_empty() && parsed.version > 0 && parsed.version <= i64::MAX as u64,
        "invalid memory identity or version",
    )?;
    check(!parsed.content.trim().is_empty(), "empty memory content")?;
    check(
        chrono::DateTime::parse_from_rfc3339(&parsed.created_at).is_ok()
            && chrono::DateTime::parse_from_rfc3339(&parsed.updated_at).is_ok(),
        "invalid memory timestamp",
    )?;
    Ok((parsed, value))
}

pub(super) fn validate_snapshot(snapshot: &Snapshot) -> Result<(), String> {
    let mut projects = HashSet::new();
    let mut directories = HashSet::new();
    for row in &snapshot.projects {
        check(
            !row.id.trim().is_empty() && projects.insert(row.id.as_str()),
            "duplicate or empty project id",
        )?;
        check(!row.name.trim().is_empty(), "empty project name")?;
        check(
            row.directory.is_empty() || directories.insert(row.directory.as_str()),
            "duplicate project directory",
        )?;
        check(
            chrono::DateTime::parse_from_rfc3339(&row.created_at).is_ok(),
            "invalid project timestamp",
        )?;
    }
    let mut memories = HashMap::new();
    for row in &snapshot.memories {
        let (body, value) = memory(&row.body)?;
        check(
            row.id == body.id
                && row.memory_scope == body.memory_scope
                && row.project_id == body.project_id,
            "memory index differs from body",
        )?;
        check(
            match body.memory_scope.as_str() {
                "personal" => body.project_id.is_none(),
                "project" => body
                    .project_id
                    .as_deref()
                    .is_some_and(|id| projects.contains(id)),
                _ => false,
            },
            "invalid memory scope or missing project",
        )?;
        check(
            memories.insert(row.id.as_str(), (body, value)).is_none(),
            "duplicate memory id",
        )?;
    }
    for (body, _) in memories.values() {
        check(
            body.origin_memory_id
                .as_deref()
                .map_or(true, |id| memories.contains_key(id)),
            "missing origin memory",
        )?;
    }

    let mut histories: HashMap<&str, BTreeMap<u64, Value>> = HashMap::new();
    for row in &snapshot.revisions {
        let (body, value) = memory(&row.body)?;
        let (current, _) = memories
            .get(row.memory_id.as_str())
            .ok_or("orphan revision")?;
        check(
            row.memory_id == body.id && row.version == body.version,
            "revision index differs from body",
        )?;
        check(
            body.version <= current.version
                && body.memory_scope == current.memory_scope
                && body.project_id == current.project_id
                && body.origin_memory_id == current.origin_memory_id
                && body.created_at == current.created_at
                && body.evidence_type == current.evidence_type,
            "revision immutable fields differ",
        )?;
        check(
            histories
                .entry(row.memory_id.as_str())
                .or_default()
                .insert(row.version, value)
                .is_none(),
            "duplicate revision",
        )?;
    }
    for (id, (current, value)) in &memories {
        let history = histories.get(id).ok_or("missing revision history")?;
        check(
            history.len() as u64 == current.version
                && history
                    .keys()
                    .enumerate()
                    .all(|(index, version)| *version == index as u64 + 1),
            "incomplete revision history",
        )?;
        check(
            history.get(&current.version) == Some(value),
            "latest revision differs from current memory",
        )?;
    }

    let mut event_ids = HashSet::new();
    let mut created_ids = HashSet::new();
    for row in &snapshot.events {
        check(
            !row.event_id.trim().is_empty() && event_ids.insert(row.event_id.as_str()),
            "duplicate or empty event id",
        )?;
        let request: Value =
            serde_json::from_str(&row.request).map_err(|_| "invalid event request")?;
        let response: Value =
            serde_json::from_str(&row.response).map_err(|_| "invalid event response")?;
        check(
            request.is_object() && response.is_object(),
            "invalid event envelope",
        )?;
        check(
            request["event_id"].as_str().map(str::trim) == Some(row.event_id.as_str()),
            "event request id differs",
        )?;
        let initial = response
            .get("memory")
            .ok_or("event response lacks memory")?;
        let id = initial["id"]
            .as_str()
            .ok_or("event response lacks memory id")?;
        let history = histories.get(id).ok_or("event references missing memory")?;
        check(
            history.get(&1) == Some(initial),
            "event response differs from first revision",
        )?;
        check(
            created_ids.insert(id.to_owned()),
            "multiple creation events for memory",
        )?;
        check(
            request["memory_scope"].as_str().map(str::trim) == initial["memory_scope"].as_str()
                && request["project_id"].as_str() == initial["project_id"].as_str()
                && request["content"].as_str().map(str::trim) == initial["content"].as_str(),
            "event request differs from first revision",
        )?;
    }
    check(
        created_ids.len() == memories.len(),
        "missing creation event",
    )
}

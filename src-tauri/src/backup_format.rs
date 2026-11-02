//! Bounded, pure serialization format for `graph-memory` workspace backups.
//!
//! This module performs no IO and never touches SQLite. It only turns an
//! in-memory [`Snapshot`] into a canonical JSON envelope ([`encode`]) and turns
//! that envelope back into a [`Snapshot`] ([`decode`]).
//!
//! Canonical form
//! --------------
//! Every row array of the payload is sorted by its primary key:
//!
//! * `projects`   -> `id`
//! * `memories`   -> `id`
//! * `revisions`  -> `(memory_id, version)` with `version` compared numerically
//! * `events`     -> `event_id`
//!
//! Sorting uses Rust `String`/`str` ordering, which is byte-wise and therefore
//! identical to UTF-8 lexicographic ordering. `decode` *requires* arrays to be
//! strictly sorted by those keys, so a snapshot has exactly one canonical byte
//! representation and the payload checksum is unambiguous.
//!
//! The checksum is computed over the typed [`Snapshot`] serialized compactly by
//! `serde_json`, using the declared struct field order. That means the pretty
//! outer envelope may vary in whitespace and key order (or be re-printed by any
//! other tool) without invalidating the checksum, while the payload itself must
//! stay canonical.
//!
//! The digest is an integrity check against accidental corruption (truncated
//! files, bad copies, mangled editors). It is **not** authentication: it is not
//! a MAC and carries no secret, so anyone able to rewrite the file can also
//! recompute it. Do not use it to make claims about malicious edits.
//!
//! Raw payload strings (`body`, `request`, `response`) are preserved verbatim.
//! They carry existing SQL values, so byte identity of event request/response
//! data matters; validation inspects them without rewriting the stored strings.

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod validate;

/// Magic value identifying this export format.
pub const FORMAT: &str = "graph-memory-workspace";
/// Version of the envelope layout described by this module.
pub const FORMAT_VERSION: u32 = 1;
/// Upper bound applied to both the encoded envelope and the decoded input.
pub const MAX_BYTES: usize = 64 * 1024 * 1024;

const ERR_TIMESTAMP: &str = "timestamp is not a valid RFC 3339 value";
const ERR_SERIALIZE: &str = "backup payload could not be serialized";
const ERR_TOO_LARGE: &str = "backup exceeds the maximum allowed size";
const ERR_INVALID: &str = "backup is not a valid graph-memory-workspace export";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRow {
    pub id: String,
    pub name: String,
    pub directory: String,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRow {
    pub id: String,
    pub memory_scope: String,
    #[serde(deserialize_with = "required_nullable_string")]
    pub project_id: Option<String>,
    pub body: String,
}

// Nullable is not the same as optional: the row layout requires this key.
fn required_nullable_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionRow {
    pub memory_id: String,
    pub version: u64,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventRow {
    pub event_id: String,
    pub request: String,
    pub response: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub projects: Vec<ProjectRow>,
    pub memories: Vec<MemoryRow>,
    pub revisions: Vec<RevisionRow>,
    pub events: Vec<EventRow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Counts {
    pub projects: usize,
    pub memories: usize,
    pub revisions: usize,
    pub events: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub format_version: u32,
    pub exported_at: String,
    pub counts: Counts,
    pub payload_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Backup {
    pub manifest: Manifest,
    pub payload: Snapshot,
}

/// Encode `snapshot` into a bounded, canonical backup envelope.
///
/// Validates the timestamp and the snapshot, sorts each row array into its
/// canonical primary-key order, records per-array counts, and stores a SHA-256
/// digest of the canonical payload. The returned bytes are pretty-printed JSON.
pub fn encode(mut snapshot: Snapshot, exported_at: &str) -> Result<Vec<u8>, String> {
    if !timestamp_ok(exported_at) {
        return Err(ERR_TIMESTAMP.to_string());
    }

    validate::validate_snapshot(&snapshot)?;

    normalize(&mut snapshot);

    let manifest = Manifest {
        format: FORMAT.to_string(),
        format_version: FORMAT_VERSION,
        exported_at: exported_at.to_string(),
        counts: counts_of(&snapshot),
        payload_sha256: digest_of(&snapshot)?,
    };

    let backup = Backup {
        manifest,
        payload: snapshot,
    };

    let bytes = serde_json::to_vec_pretty(&backup).map_err(|_| ERR_SERIALIZE.to_string())?;

    if bytes.len() > MAX_BYTES {
        return Err(ERR_TOO_LARGE.to_string());
    }

    Ok(bytes)
}

/// Decode and verify a backup envelope produced by [`encode`].
///
/// Rejects oversized input, unknown envelope/row fields, wrong format
/// or version, non-RFC3339 timestamps, count mismatches, malformed or
/// mismatching digests, and payloads whose arrays are not already strictly
/// sorted by their primary keys.
pub fn decode(bytes: &[u8]) -> Result<Snapshot, String> {
    if bytes.len() > MAX_BYTES {
        return Err(ERR_TOO_LARGE.to_string());
    }

    let backup: Backup = serde_json::from_slice(bytes).map_err(|_| ERR_INVALID.to_string())?;

    let Backup { manifest, payload } = backup;

    if manifest.format != FORMAT {
        return Err(ERR_INVALID.to_string());
    }

    if manifest.format_version != FORMAT_VERSION {
        return Err(ERR_INVALID.to_string());
    }

    if !timestamp_ok(&manifest.exported_at) {
        return Err(ERR_INVALID.to_string());
    }

    if manifest.counts != counts_of(&payload) {
        return Err(ERR_INVALID.to_string());
    }

    if !is_lowercase_hex64(&manifest.payload_sha256) {
        return Err(ERR_INVALID.to_string());
    }

    // Strict ordering is required so that the checksum covers exactly one byte
    // representation of the payload. Duplicate primary keys are rejected here
    // as well, because an equal key pair is not strictly increasing; any
    // remaining uniqueness invariants belong to `validate`.
    if !canonical_order_ok(&payload) {
        return Err(ERR_INVALID.to_string());
    }

    // Integrity check against accidental corruption. Not a MAC: the digest has
    // no secret and can be recomputed by anyone who can rewrite the file.
    let computed = digest_of(&payload).map_err(|_| ERR_INVALID.to_string())?;
    if computed != manifest.payload_sha256 {
        return Err(ERR_INVALID.to_string());
    }

    validate::validate_snapshot(&payload)?;

    Ok(payload)
}

fn timestamp_ok(value: &str) -> bool {
    DateTime::parse_from_rfc3339(value).is_ok()
}

fn is_lowercase_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn counts_of(snapshot: &Snapshot) -> Counts {
    Counts {
        projects: snapshot.projects.len(),
        memories: snapshot.memories.len(),
        revisions: snapshot.revisions.len(),
        events: snapshot.events.len(),
    }
}

/// SHA-256 of the compact typed serialization of `snapshot`, lowercase hex.
fn digest_of(snapshot: &Snapshot) -> Result<String, String> {
    let bytes = serde_json::to_vec(snapshot).map_err(|_| ERR_SERIALIZE.to_string())?;
    Ok(hex::encode(Sha256::digest(&bytes)))
}

/// Sort every row array into its canonical primary-key order.
fn normalize(snapshot: &mut Snapshot) {
    snapshot.projects.sort_by(|a, b| a.id.cmp(&b.id));
    snapshot.memories.sort_by(|a, b| a.id.cmp(&b.id));
    snapshot.revisions.sort_by(|a, b| {
        a.memory_id
            .cmp(&b.memory_id)
            .then_with(|| a.version.cmp(&b.version))
    });
    snapshot.events.sort_by(|a, b| a.event_id.cmp(&b.event_id));
}

fn canonical_order_ok(snapshot: &Snapshot) -> bool {
    projects_sorted_strict(&snapshot.projects)
        && memories_sorted_strict(&snapshot.memories)
        && revisions_sorted_strict(&snapshot.revisions)
        && events_sorted_strict(&snapshot.events)
}

fn projects_sorted_strict(rows: &[ProjectRow]) -> bool {
    rows.windows(2).all(|pair| pair[0].id < pair[1].id)
}

fn memories_sorted_strict(rows: &[MemoryRow]) -> bool {
    rows.windows(2).all(|pair| pair[0].id < pair[1].id)
}

fn revisions_sorted_strict(rows: &[RevisionRow]) -> bool {
    rows.windows(2).all(|pair| {
        let (first, second) = (&pair[0], &pair[1]);
        (first.memory_id.as_str(), first.version) < (second.memory_id.as_str(), second.version)
    })
}

fn events_sorted_strict(rows: &[EventRow]) -> bool {
    rows.windows(2)
        .all(|pair| pair[0].event_id < pair[1].event_id)
}

#[cfg(test)]
mod tests;

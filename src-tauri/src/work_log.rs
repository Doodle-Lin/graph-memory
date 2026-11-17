use chrono::{FixedOffset, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Allowed values for [`WorkLog::status`].
pub const STATUSES: [&str; 4] = ["in_progress", "completed", "blocked", "planned"];

/// One day's work log entry, exchanged as JSON.
///
/// All fields are required; unknown keys are rejected (`deny_unknown_fields`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkLog {
    /// Calendar day, strict `YYYY-MM-DD`; day precision only, no time invented.
    pub date: String,
    /// Author's UTC offset, strict `+HH:MM`/`-HH:MM`, magnitude at most `23:59`.
    pub utc_offset: String,
    /// One of [`STATUSES`].
    pub status: String,
    /// What actually happened that day; must not be blank.
    pub outcome: String,
    /// Follow-up items for the next session.
    pub next_steps: Vec<String>,
}

impl WorkLog {
    /// Semantic validation beyond serde's structural checks.
    pub fn validate(&self) -> Result<(), String> {
        Self::validate_date(&self.date)?;
        validate_utc_offset(&self.utc_offset)?;
        if !STATUSES.contains(&self.status.as_str()) {
            return Err(format!(
                "status must be one of {STATUSES:?}, got {:?}",
                self.status
            ));
        }
        if self.outcome.trim().is_empty() {
            return Err("outcome must not be blank".to_string());
        }
        Ok(())
    }

    /// Strict `YYYY-MM-DD` that is also a real calendar date.
    /// Public so query paths can validate bare date strings.
    pub fn validate_date(value: &str) -> Result<(), String> {
        let b = value.as_bytes();
        let shape = b.len() == 10
            && b[..4].iter().all(|d| d.is_ascii_digit())
            && b[4] == b'-'
            && b[5..7].iter().all(|d| d.is_ascii_digit())
            && b[7] == b'-'
            && b[8..10].iter().all(|d| d.is_ascii_digit());
        if !shape {
            return Err(format!("date must be strict YYYY-MM-DD, got {value:?}"));
        }
        // Only the date is parsed: no midnight/timestamp is fabricated
        // for day-precision fields.
        NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map_err(|_| format!("date is not a real calendar date: {value:?}"))
            .map(|_| ())
    }

    /// JSON Schema describing the wire shape: same fields, all required, no extras.
    pub fn schema() -> serde_json::Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "date": {
                    "type": "string",
                    "pattern": "^\\d{4}-\\d{2}-\\d{2}$"
                },
                "utc_offset": {
                    "type": "string",
                    "pattern": "^[+-]\\d{2}:\\d{2}$"
                },
                "status": {
                    "type": "string",
                    "enum": (STATUSES.to_vec())
                },
                "outcome": {
                    "type": "string",
                    "minLength": 1
                },
                "next_steps": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            },
            "required": ["date", "utc_offset", "status", "outcome", "next_steps"]
        })
    }
}

/// Strict `+HH:MM`/`-HH:MM`, magnitude at most `23:59`, confirmed via chrono.
fn validate_utc_offset(value: &str) -> Result<(), String> {
    let b = value.as_bytes();
    let bad_shape = || format!("utc_offset must be strict +/-HH:MM, got {value:?}");
    let shape = b.len() == 6
        && matches!(b[0], b'+' | b'-')
        && b[1..3].iter().all(|d| d.is_ascii_digit())
        && b[3] == b':'
        && b[4..6].iter().all(|d| d.is_ascii_digit());
    if !shape {
        return Err(bad_shape());
    }
    let hours = value[1..3].parse::<u32>().map_err(|_| bad_shape())?;
    let minutes = value[4..6].parse::<u32>().map_err(|_| bad_shape())?;
    if hours > 23 || minutes > 59 {
        return Err(format!("utc_offset out of range, max 23:59, got {value:?}"));
    }
    // Route through chrono's FixedOffset so the accepted range is
    // confirmed by the same type used elsewhere for offsets.
    let secs = (hours * 3600 + minutes * 60) as i32;
    let offset = if b[0] == b'-' {
        FixedOffset::west_opt(secs)
    } else {
        FixedOffset::east_opt(secs)
    };
    offset
        .ok_or_else(|| format!("utc_offset out of range, max 23:59, got {value:?}"))
        .map(|_| ())
}

//! Normalization of Mastodon timeline entries into stored status records.
//!
//! The input is a timeline entry whose object keys the Mastodon adapter has
//! already converted to camelCase (docs/adr/0001). The output is the row of
//! the `mastodon_statuses` table.

use std::collections::HashSet;
use std::fmt;

use jiff::Timestamp;
use serde_json::Value;

use crate::js::{coalesce, is_truthy, to_js_string};

/// The Mastodon status id. Mastodon sends it as a string of digits; the
/// service compares and stores it as text and never does arithmetic on it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StatusId(String);

impl StatusId {
    /// Wraps a stored or received id. The value must not be empty.
    #[must_use]
    pub fn new(value: String) -> Option<Self> {
        (!value.is_empty()).then_some(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StatusId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One row of `mastodon_statuses`.
#[derive(Debug, Clone, PartialEq)]
pub struct StatusRecord {
    pub id: StatusId,
    pub created_at: Timestamp,
    pub edited_at: Option<Timestamp>,
    pub url: String,
    pub content: String,
    pub spoiler_text: String,
    pub account_id: String,
    pub account_username: String,
    pub account_display_name: String,
    pub account_url: String,
    /// The unwrapped status object with camelCase keys. The feed reads the
    /// avatar and the media attachments from it.
    pub raw: Value,
}

/// Why a timeline entry produced no record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// The status has no usable `id`.
    MissingId,
    /// The status has neither a usable `url` nor a `uri`.
    MissingUrl,
    /// `createdAt` or `editedAt` is not an RFC 3339 instant.
    InvalidTime,
}

impl Rejection {
    /// A fixed code for diagnostics.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::MissingId => "missing_id",
            Self::MissingUrl => "missing_url",
            Self::InvalidTime => "invalid_time",
        }
    }
}

/// The records of one poll and the entries that produced no record.
#[derive(Debug, Default)]
pub struct Normalized {
    pub records: Vec<StatusRecord>,
    pub rejected: Vec<Rejection>,
}

/// Normalizes timeline entries in order and keeps the first record of each
/// id. A boost and its original status share the id of the original.
///
/// `now` replaces a missing `createdAt`. This keeps the behaviour of the
/// TypeScript service; Mastodon always sends `created_at`.
#[must_use]
pub fn normalize_timeline(entries: impl IntoIterator<Item = Value>, now: Timestamp) -> Normalized {
    let mut seen = HashSet::new();
    let mut normalized = Normalized::default();
    for entry in entries {
        match normalize_status(entry, now) {
            Ok(record) => {
                if seen.insert(record.id.clone()) {
                    normalized.records.push(record);
                }
            }
            Err(rejection) => normalized.rejected.push(rejection),
        }
    }
    normalized
}

/// Normalizes one timeline entry. A boost becomes its original status.
///
/// # Errors
///
/// Returns a [`Rejection`] when the status has no id, no url, or an invalid
/// time value.
pub fn normalize_status(entry: Value, now: Timestamp) -> Result<StatusRecord, Rejection> {
    let item = unwrap_reblog(entry);

    let url_value = coalesce(item.get("url"), item.get("uri"));
    if !is_truthy(item.get("id")) {
        return Err(Rejection::MissingId);
    }
    if !is_truthy(url_value) {
        return Err(Rejection::MissingUrl);
    }
    let id = StatusId::new(to_js_string(item.get("id"))).ok_or(Rejection::MissingId)?;
    let url = to_js_string(url_value);

    let created_at = match item.get("createdAt") {
        None | Some(Value::Null) => truncate_to_millisecond(now),
        Some(value) => parse_instant(value).ok_or(Rejection::InvalidTime)?,
    };
    let edited_at = if is_truthy(item.get("editedAt")) {
        Some(
            item.get("editedAt")
                .and_then(parse_instant)
                .ok_or(Rejection::InvalidTime)?,
        )
    } else {
        None
    };

    let account = item.get("account");
    let account_field = |name: &str| to_js_string(account.and_then(|account| account.get(name)));

    Ok(StatusRecord {
        id,
        created_at,
        edited_at,
        url,
        content: to_js_string(item.get("content")),
        spoiler_text: to_js_string(item.get("spoilerText")),
        account_id: account_field("id"),
        account_username: account_field("acct"),
        account_display_name: account_field("displayName"),
        account_url: account_field("url"),
        raw: item,
    })
}

fn unwrap_reblog(mut entry: Value) -> Value {
    match entry.get_mut("reblog").map(Value::take) {
        None | Some(Value::Null) => entry,
        Some(reblog) => reblog,
    }
}

/// Parses an RFC 3339 instant. JavaScript `Date` kept milliseconds only, so
/// the value is truncated to milliseconds; stored rows then compare equal to
/// rows that the TypeScript service wrote.
fn parse_instant(value: &Value) -> Option<Timestamp> {
    let timestamp = match value {
        Value::String(text) => text.parse::<Timestamp>().ok()?,
        Value::Number(number) => Timestamp::from_millisecond(number.as_i64()?).ok()?,
        _ => return None,
    };
    Some(truncate_to_millisecond(timestamp))
}

/// Truncates toward the earlier millisecond, like JavaScript `Date`.
#[must_use]
pub fn truncate_to_millisecond(timestamp: Timestamp) -> Timestamp {
    let millisecond = timestamp.as_millisecond();
    let truncated = if Timestamp::from_millisecond(millisecond).is_ok_and(|t| t > timestamp) {
        millisecond - 1
    } else {
        millisecond
    };
    Timestamp::from_millisecond(truncated).unwrap_or(timestamp)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn now() -> Timestamp {
        "2026-10-14T10:00:00.500Z".parse().unwrap()
    }

    fn status(id: &str) -> Value {
        json!({
            "id": id,
            "createdAt": "2026-10-01T12:00:00.123456Z",
            "editedAt": null,
            "url": format!("https://home.example/@a/{id}"),
            "uri": "https://home.example/users/a/statuses/1",
            "content": "<p>x</p>",
            "spoilerText": "cw",
            "account": {"id": "9", "acct": "a@remote.example", "displayName": "A", "url": "https://home.example/@a"},
        })
    }

    #[test]
    fn maps_the_typed_columns_and_keeps_the_raw_object() {
        let record = normalize_status(status("1"), now()).unwrap();
        assert_eq!(record.id.as_str(), "1");
        assert_eq!(record.created_at.to_string(), "2026-10-01T12:00:00.123Z");
        assert_eq!(record.edited_at, None);
        assert_eq!(record.url, "https://home.example/@a/1");
        assert_eq!(record.spoiler_text, "cw");
        assert_eq!(record.account_id, "9");
        assert_eq!(record.account_username, "a@remote.example");
        assert_eq!(record.account_display_name, "A");
        assert_eq!(record.account_url, "https://home.example/@a");
        assert_eq!(record.raw, status("1"));
    }

    #[test]
    fn a_boost_becomes_its_original_status() {
        let mut boost = status("2");
        boost["reblog"] = status("1");
        let record = normalize_status(boost, now()).unwrap();
        assert_eq!(record.id.as_str(), "1");
        assert_eq!(record.raw, status("1"));
    }

    #[test]
    fn falls_back_to_uri_only_when_url_is_missing_or_null() {
        let mut entry = status("1");
        entry["url"] = Value::Null;
        assert_eq!(
            normalize_status(entry, now()).unwrap().url,
            "https://home.example/users/a/statuses/1"
        );

        // `url ?? uri` keeps an empty string, which then fails the check.
        let mut entry = status("1");
        entry["url"] = json!("");
        assert_eq!(normalize_status(entry, now()), Err(Rejection::MissingUrl));

        let mut entry = status("1");
        entry["url"] = Value::Null;
        entry["uri"] = json!("");
        assert_eq!(normalize_status(entry, now()), Err(Rejection::MissingUrl));
    }

    #[test]
    fn rejects_a_missing_or_empty_id() {
        let mut entry = status("1");
        entry["id"] = json!("");
        assert_eq!(normalize_status(entry, now()), Err(Rejection::MissingId));
        let mut entry = status("1");
        entry.as_object_mut().unwrap().remove("id");
        assert_eq!(normalize_status(entry, now()), Err(Rejection::MissingId));
    }

    #[test]
    fn a_missing_creation_time_becomes_now() {
        let mut entry = status("1");
        entry.as_object_mut().unwrap().remove("createdAt");
        assert_eq!(
            normalize_status(entry, now())
                .unwrap()
                .created_at
                .to_string(),
            "2026-10-14T10:00:00.5Z"
        );
    }

    #[test]
    fn an_invalid_time_rejects_the_status() {
        let mut entry = status("1");
        entry["createdAt"] = json!("yesterday");
        assert_eq!(normalize_status(entry, now()), Err(Rejection::InvalidTime));
        let mut entry = status("1");
        entry["editedAt"] = json!("soon");
        assert_eq!(normalize_status(entry, now()), Err(Rejection::InvalidTime));
    }

    #[test]
    fn an_empty_edit_time_means_not_edited() {
        let mut entry = status("1");
        entry["editedAt"] = json!("");
        assert_eq!(normalize_status(entry, now()).unwrap().edited_at, None);
        let mut entry = status("1");
        entry["editedAt"] = json!("2026-10-02T00:00:00.999999Z");
        assert_eq!(
            normalize_status(entry, now())
                .unwrap()
                .edited_at
                .unwrap()
                .to_string(),
            "2026-10-02T00:00:00.999Z"
        );
    }

    #[test]
    fn missing_account_fields_become_empty_strings() {
        let mut entry = status("1");
        entry["account"] = json!({"id": 7});
        let record = normalize_status(entry, now()).unwrap();
        assert_eq!(record.account_id, "7");
        assert_eq!(record.account_username, "");
        assert_eq!(record.account_display_name, "");
        assert_eq!(record.account_url, "");
    }

    #[test]
    fn the_first_record_of_an_id_wins() {
        let mut duplicate = status("1");
        duplicate["content"] = json!("<p>second</p>");
        let invalid = json!({"id": "", "url": "x"});
        let normalized = normalize_timeline([status("1"), invalid, duplicate, status("2")], now());
        let ids: Vec<_> = normalized.records.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["1", "2"]);
        assert_eq!(normalized.records[0].content, "<p>x</p>");
        assert_eq!(normalized.rejected, [Rejection::MissingId]);
    }

    #[test]
    fn truncation_moves_to_the_earlier_millisecond() {
        let before_epoch: Timestamp = "1969-12-31T23:59:59.9995Z".parse().unwrap();
        assert_eq!(
            truncate_to_millisecond(before_epoch).to_string(),
            "1969-12-31T23:59:59.999Z"
        );
        let after_epoch: Timestamp = "2026-01-01T00:00:00.0019Z".parse().unwrap();
        assert_eq!(
            truncate_to_millisecond(after_epoch).to_string(),
            "2026-01-01T00:00:00.001Z"
        );
    }
}

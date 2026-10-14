//! JSON Feed rendering.
//!
//! The output reproduces `feed` 5.1.0 `json1()` as the TypeScript service
//! used it: JSON Feed version 1, four-space indentation, the same key order,
//! and the same rules for omitted keys. Feed readers see no change.

use jiff::Timestamp;
use serde::Serialize;
use serde_json::Value;

use crate::html::escape_html;
use crate::js::{coalesce, is_truthy};
use crate::status::{StatusId, truncate_to_millisecond};

const JSON_FEED_VERSION: &str = "https://jsonfeed.org/version/1";

/// Feed-level settings from the service configuration.
#[derive(Debug, Clone)]
pub struct FeedSettings {
    pub title: String,
    pub description: Option<String>,
    /// The public base URL of the service, exactly as configured.
    pub home_page_url: String,
    /// The base URL of the Mastodon instance, exactly as configured.
    pub mastodon_base_url: String,
}

/// The stored values that one feed item needs.
#[derive(Debug, Clone)]
pub struct FeedRow {
    pub id: StatusId,
    pub created_at: Timestamp,
    pub edited_at: Option<Timestamp>,
    pub url: String,
    pub content: String,
    pub account_username: String,
    pub account_display_name: String,
    pub account_url: String,
    pub raw: Value,
}

/// The feed could not be serialized.
#[derive(Debug)]
pub struct RenderError(serde_json::Error);

impl std::fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "the feed could not be serialized: {}", self.0)
    }
}

impl std::error::Error for RenderError {}

/// Renders the feed document. `newest_first` holds the newest rows first,
/// as the database returns them; the feed lists them oldest first.
///
/// # Errors
///
/// Returns [`RenderError`] when serialization fails. The document has only
/// string keys, so this does not happen in practice.
pub fn render_feed(
    settings: &FeedSettings,
    newest_first: &[FeedRow],
) -> Result<String, RenderError> {
    let home = trim_trailing_slashes(&settings.home_page_url);
    let mastodon_base = trim_trailing_slashes(&settings.mastodon_base_url);
    let feed = JsonFeed {
        version: JSON_FEED_VERSION,
        title: &settings.title,
        home_page_url: non_empty(&settings.home_page_url),
        feed_url: format!("{home}/feed.json"),
        description: settings.description.as_deref().and_then(non_empty),
        icon: format!("{home}/favicon.png"),
        items: newest_first
            .iter()
            .rev()
            .map(|row| item(row, mastodon_base))
            .collect(),
    };

    let mut output = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut output, formatter);
    feed.serialize(&mut serializer).map_err(RenderError)?;
    // serde_json writes valid UTF-8, and this conversion does not copy.
    String::from_utf8(output).map_err(|_| RenderError(serde::ser::Error::custom("invalid UTF-8")))
}

#[derive(Serialize)]
struct JsonFeed<'a> {
    version: &'static str,
    title: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    home_page_url: Option<&'a str>,
    feed_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    icon: String,
    items: Vec<JsonItem<'a>>,
}

#[derive(Serialize)]
struct JsonItem<'a> {
    id: &'a str,
    content_html: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    date_modified: String,
    date_published: String,
    author: JsonAuthor<'a>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    attachments: Vec<JsonAttachment<'a>>,
}

#[derive(Serialize)]
struct JsonAuthor<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar: Option<&'a str>,
}

#[derive(Serialize)]
struct JsonAttachment<'a> {
    url: &'a str,
    mime_type: &'static str,
}

fn item<'a>(row: &'a FeedRow, mastodon_base: &str) -> JsonItem<'a> {
    let author_name = if row.account_display_name.is_empty() {
        row.account_username.as_str()
    } else {
        row.account_display_name.as_str()
    };
    let avatar = author_avatar(&row.raw);
    let attachments = media_attachments(&row.raw);

    let content_html = format!(
        "{}{}{}",
        row.content,
        attachments_html(&attachments),
        author_footer_html(author_name, &row.account_url, avatar)
    );
    let link = if row.account_username.is_empty() {
        row.url.clone()
    } else {
        format!("{mastodon_base}/@{}/{}", row.account_username, row.id)
    };
    let modified_at = row.edited_at.unwrap_or(row.created_at);

    JsonItem {
        id: row.id.as_str(),
        content_html,
        url: (!link.is_empty()).then_some(link),
        title: non_empty(author_name),
        date_modified: iso_string(modified_at),
        date_published: iso_string(row.created_at),
        author: JsonAuthor {
            name: non_empty(author_name),
            url: non_empty(&row.account_url),
            avatar,
        },
        attachments: attachments
            .iter()
            .map(|attachment| JsonAttachment {
                url: attachment.url,
                mime_type: attachment.kind.mime_type(),
            })
            .collect(),
    }
}

/// `account.avatar`, else `account.avatarStatic`, from the raw status.
fn author_avatar(raw: &Value) -> Option<&str> {
    let account = raw.get("account")?;
    ["avatar", "avatarStatic"]
        .into_iter()
        .find_map(|key| account.get(key).and_then(Value::as_str).and_then(non_empty))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaKind {
    Image,
    Video,
    Audio,
    Other,
}

impl MediaKind {
    fn from_type(value: Option<&Value>) -> Self {
        match value.and_then(Value::as_str) {
            Some("image") => Self::Image,
            Some("video" | "gifv") => Self::Video,
            Some("audio") => Self::Audio,
            _ => Self::Other,
        }
    }

    fn mime_type(self) -> &'static str {
        match self {
            Self::Image => "image/*",
            Self::Video => "video/*",
            Self::Audio => "audio/*",
            Self::Other => "application/octet-stream",
        }
    }
}

struct MediaAttachment<'a> {
    kind: MediaKind,
    url: &'a str,
    description: Option<&'a str>,
}

/// The renderable media attachments: `url ?? previewUrl` must be a
/// non-empty string. Other entries are left out.
fn media_attachments(raw: &Value) -> Vec<MediaAttachment<'_>> {
    let Some(Value::Array(entries)) = raw.get("mediaAttachments") else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let url = coalesce(entry.get("url"), entry.get("previewUrl"));
            if !is_truthy(url) {
                return None;
            }
            Some(MediaAttachment {
                kind: MediaKind::from_type(entry.get("type")),
                url: url?.as_str()?,
                description: entry.get("description").and_then(Value::as_str),
            })
        })
        .collect()
}

fn attachments_html(attachments: &[MediaAttachment<'_>]) -> String {
    let html: String = attachments.iter().map(attachment_html).collect();
    if html.is_empty() {
        html
    } else {
        format!(r#"<div class="attachments">{html}</div>"#)
    }
}

fn attachment_html(attachment: &MediaAttachment<'_>) -> String {
    let url = escape_html(attachment.url);
    let description = attachment.description.map(escape_html).unwrap_or_default();
    let caption = if description.is_empty() {
        String::new()
    } else {
        format!("<figcaption>{description}</figcaption>")
    };
    match attachment.kind {
        MediaKind::Image => format!(
            r#"<figure class="attachment attachment-image"><img src="{url}" alt="{description}" loading="lazy" />{caption}</figure>"#
        ),
        MediaKind::Video => format!(
            r#"<figure class="attachment attachment-video"><video controls src="{url}" preload="metadata"></video>{caption}</figure>"#
        ),
        MediaKind::Audio => {
            format!(
                r#"<figure class="attachment attachment-audio"><audio controls src="{url}"></audio>{caption}</figure>"#
            )
        }
        MediaKind::Other => {
            format!(r#"<p class="attachment attachment-link"><a href="{url}">Attachment</a></p>"#)
        }
    }
}

fn author_footer_html(name: &str, url: &str, avatar: Option<&str>) -> String {
    if name.is_empty() && avatar.is_none() {
        return String::new();
    }
    let name = escape_html(if name.is_empty() { "Author" } else { name });
    let name_html = if url.is_empty() {
        format!("<span>{name}</span>")
    } else {
        format!(r#"<a href="{}">{name}</a>"#, escape_html(url))
    };
    match avatar {
        None => format!(r#"<p class="feed-author">{name_html}</p>"#),
        Some(avatar) => format!(
            r#"<p class="feed-author">{name_html}<br /><img src="{}" alt="{name} avatar" width="50" height="50" style="border-radius:8px" loading="lazy" /></p>"#,
            escape_html(avatar)
        ),
    }
}

/// `Date.prototype.toISOString`: UTC with exactly three fraction digits.
fn iso_string(timestamp: Timestamp) -> String {
    truncate_to_millisecond(timestamp)
        .strftime("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn trim_trailing_slashes(value: &str) -> &str {
    value.trim_end_matches('/')
}

fn non_empty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn settings() -> FeedSettings {
        FeedSettings {
            title: "Feed".to_owned(),
            description: None,
            home_page_url: "https://feed.example//".to_owned(),
            mastodon_base_url: "https://home.example/".to_owned(),
        }
    }

    fn row(id: &str, raw: Value) -> FeedRow {
        FeedRow {
            id: StatusId::new(id.to_owned()).unwrap(),
            created_at: "2026-10-01T12:00:00Z".parse().unwrap(),
            edited_at: None,
            url: format!("https://remote.example/@a/{id}"),
            content: "<p>c</p>".to_owned(),
            account_username: "a".to_owned(),
            account_display_name: String::new(),
            account_url: String::new(),
            raw,
        }
    }

    fn render(rows: &[FeedRow]) -> Value {
        serde_json::from_str(&render_feed(&settings(), rows).unwrap()).unwrap()
    }

    #[test]
    fn an_empty_feed_has_the_feed_level_keys_only() {
        let output = render_feed(&settings(), &[]).unwrap();
        assert_eq!(
            output,
            "{\n    \"version\": \"https://jsonfeed.org/version/1\",\n    \"title\": \"Feed\",\n    \"home_page_url\": \"https://feed.example//\",\n    \"feed_url\": \"https://feed.example/feed.json\",\n    \"icon\": \"https://feed.example/favicon.png\",\n    \"items\": []\n}"
        );
    }

    #[test]
    fn items_are_listed_oldest_first() {
        let feed = render(&[row("2", json!({})), row("1", json!({}))]);
        let ids: Vec<_> = feed["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].clone())
            .collect();
        assert_eq!(ids, [json!("1"), json!("2")]);
    }

    #[test]
    fn the_link_uses_the_local_instance_form() {
        let feed = render(&[row("1", json!({}))]);
        assert_eq!(feed["items"][0]["url"], "https://home.example/@a/1");

        let mut without_account = row("1", json!({}));
        without_account.account_username = String::new();
        let feed = render(&[without_account]);
        assert_eq!(feed["items"][0]["url"], "https://remote.example/@a/1");
    }

    #[test]
    fn an_empty_author_gives_an_empty_author_object_and_no_title() {
        let mut anonymous = row("1", json!({}));
        anonymous.account_username = String::new();
        let feed = render(&[anonymous]);
        assert_eq!(feed["items"][0]["author"], json!({}));
        assert!(feed["items"][0].get("title").is_none());
        assert_eq!(feed["items"][0]["content_html"], "<p>c</p>");
    }

    #[test]
    fn the_footer_shows_a_fallback_name_when_only_an_avatar_exists() {
        assert_eq!(
            author_footer_html("", "", Some("https://a.example/x.png")),
            r#"<p class="feed-author"><span>Author</span><br /><img src="https://a.example/x.png" alt="Author avatar" width="50" height="50" style="border-radius:8px" loading="lazy" /></p>"#
        );
    }

    #[test]
    fn non_string_media_values_are_left_out() {
        let raw = json!({"mediaAttachments": [
            {"type": "image", "url": 5},
            "not an object",
            {"type": 3, "url": "https://m.example/a"},
        ], "account": {"avatar": 7, "avatarStatic": "https://a.example/s.png"}});
        let feed = render(&[row("1", raw)]);
        assert_eq!(
            feed["items"][0]["attachments"],
            json!([{"url": "https://m.example/a", "mime_type": "application/octet-stream"}])
        );
        assert_eq!(
            feed["items"][0]["author"]["avatar"],
            "https://a.example/s.png"
        );
    }

    #[test]
    fn dates_have_three_fraction_digits() {
        let mut edited = row("1", json!({}));
        edited.created_at = "2026-10-01T12:00:00.1Z".parse().unwrap();
        edited.edited_at = Some("2026-10-02T00:00:00.123999Z".parse().unwrap());
        let feed = render(&[edited]);
        assert_eq!(
            feed["items"][0]["date_published"],
            "2026-10-01T12:00:00.100Z"
        );
        assert_eq!(
            feed["items"][0]["date_modified"],
            "2026-10-02T00:00:00.123Z"
        );
    }
}

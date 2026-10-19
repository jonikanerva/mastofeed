//! The Mastodon REST adapter: the home timeline request.
//!
//! The adapter decodes the response at this boundary. It checks that the
//! body is a JSON array and converts the object keys to camelCase
//! (`keys.rs`). Status normalization happens in the domain crate.

pub mod keys;

use std::time::Duration;

use mastofeed_domain::status::StatusId;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue};
use reqwest::{Url, redirect};
use serde_json::Value;

use crate::config::Secret;

/// The page size of each timeline request. The TypeScript service used the
/// same value. Mastodon accepts at most 40.
pub const TIMELINE_LIMIT: u8 = 40;

/// The default deadline of one request, from the connection to the last
/// body byte. The sync deadline of the poller depends on it.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Limits of the outbound requests.
#[derive(Debug, Clone, Copy)]
pub struct ClientSettings {
    pub connect_timeout: Duration,
    /// The deadline of one attempt, from the connection to the last body byte.
    pub request_timeout: Duration,
    /// The largest accepted body after decompression.
    pub max_body_bytes: usize,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            request_timeout: REQUEST_TIMEOUT,
            // 40 statuses are usually below 200 KiB. The poller keeps at most
            // two maximal bodies per sync (`BODY_BUDGET_PAGES`). Parsed JSON
            // can need about 36 times the body size, so two 4 MiB bodies
            // used about 300 MiB, more than the 250 MB container limit. Two
            // 2 MiB bodies used about 150 MiB (`tests/integration/memory.rs`).
            max_body_bytes: 2 * 1024 * 1024,
        }
    }
}

/// A failed timeline request. The variants carry no URL, header, or body,
/// so they are safe to log.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MastodonError {
    #[error("the HTTP client could not be built")]
    Client,
    #[error("the request timed out")]
    Timeout,
    #[error("the connection failed")]
    Connect,
    #[error("the request failed")]
    Request,
    #[error("the server answered with HTTP status {0}")]
    Status(u16),
    #[error("the response body is larger than the limit")]
    BodyTooLarge,
    #[error("the response body is not a JSON array")]
    MalformedBody,
}

impl MastodonError {
    /// A fixed code for diagnostics.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Timeout => "timeout",
            Self::Connect => "connect",
            Self::Request => "request",
            Self::Status(_) => "status",
            Self::BodyTooLarge => "body_too_large",
            Self::MalformedBody => "malformed_body",
        }
    }
}

/// One page of the home timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelinePage {
    /// The entries with camelCase keys, newest first.
    pub entries: Vec<Value>,
    /// The size of the response body after decompression.
    pub body_bytes: usize,
}

/// A client for the home timeline of one account.
#[derive(Debug, Clone)]
pub struct MastodonClient {
    http: reqwest::Client,
    timeline_url: Url,
    max_body_bytes: usize,
}

impl MastodonClient {
    /// Builds the client. The token goes into a sensitive default header.
    ///
    /// # Errors
    ///
    /// Returns [`MastodonError::Client`] when the base URL cannot be a base
    /// or the HTTP client cannot be built.
    pub fn new(
        base_url: &Url,
        token: &Secret,
        settings: ClientSettings,
    ) -> Result<Self, MastodonError> {
        let mut base = base_url.clone();
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        let timeline_url = base
            .join("api/v1/timelines/home")
            .map_err(|_| MastodonError::Client)?;

        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", token.expose()))
            .map_err(|_| MastodonError::Client)?;
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization);
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let http = reqwest::Client::builder()
            .user_agent(concat!("mastofeed/", env!("CARGO_PKG_VERSION")))
            .default_headers(headers)
            // The token is in a header. A redirect must not carry it anywhere.
            .redirect(redirect::Policy::none())
            .connect_timeout(settings.connect_timeout)
            .timeout(settings.request_timeout)
            .build()
            .map_err(|_| MastodonError::Client)?;
        Ok(Self {
            http,
            timeline_url,
            max_body_bytes: settings.max_body_bytes,
        })
    }

    /// The largest accepted body of one response.
    #[must_use]
    pub fn max_body_bytes(&self) -> usize {
        self.max_body_bytes
    }

    /// Requests one page of at most `limit` statuses of the home timeline,
    /// newest first. With `since_id`, the page holds only statuses newer
    /// than that id. With `max_id`, the page holds only statuses older than
    /// that id.
    ///
    /// # Errors
    ///
    /// Returns a [`MastodonError`] for a transport failure, a non-success
    /// status, an oversized body, or a body that is not a JSON array.
    pub async fn home_timeline(
        &self,
        since_id: Option<&StatusId>,
        max_id: Option<&StatusId>,
        limit: u8,
    ) -> Result<TimelinePage, MastodonError> {
        // `max_id` comes last, so the first two requests of a sync keep the
        // query of the TypeScript service.
        let mut query = vec![("limit", limit.to_string())];
        if let Some(since_id) = since_id {
            query.push(("since_id", since_id.as_str().to_owned()));
        }
        if let Some(max_id) = max_id {
            query.push(("max_id", max_id.as_str().to_owned()));
        }
        let mut response = self
            .http
            .get(self.timeline_url.clone())
            .query(&query)
            .send()
            .await
            .map_err(|error| transport_error(&error))?;

        let status = response.status();
        if !status.is_success() {
            return Err(MastodonError::Status(status.as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > self.max_body_bytes as u64)
        {
            return Err(MastodonError::BodyTooLarge);
        }

        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| transport_error(&error))?
        {
            if body.len() + chunk.len() > self.max_body_bytes {
                return Err(MastodonError::BodyTooLarge);
            }
            body.extend_from_slice(&chunk);
        }

        // `Value` keeps the last of repeated keys, like `JSON.parse`.
        match serde_json::from_slice::<Value>(&body) {
            Ok(Value::Array(entries)) => Ok(TimelinePage {
                entries: entries.into_iter().map(keys::camel_case_keys).collect(),
                body_bytes: body.len(),
            }),
            _ => Err(MastodonError::MalformedBody),
        }
    }
}

/// Maps a transport error to a fixed kind. The error text can contain the
/// request URL, so it never leaves this function.
fn transport_error(error: &reqwest::Error) -> MastodonError {
    if error.is_timeout() {
        MastodonError::Timeout
    } else if error.is_connect() {
        MastodonError::Connect
    } else {
        MastodonError::Request
    }
}

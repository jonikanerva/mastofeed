//! HTTP routes: `/feed.json`, `/health`, and the static files in `public/`.

use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{MatchedPath, Request, State};
use axum::handler::HandlerWithoutStateExt as _;
use axum::http::header::{CONTENT_TYPE, ETAG, HeaderName, IF_NONE_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mastofeed_domain::feed::{FeedSettings, render_feed};
use serde::Serialize;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::store::Store;

/// The media type of `/feed.json`, as the TypeScript service sent it.
pub const FEED_CONTENT_TYPE: &str = "application/feed+json; charset=utf-8";

/// The deadline of the feed query and rendering.
const FEED_DEADLINE: Duration = Duration::from_secs(20);

/// The shared state of the handlers.
#[derive(Debug, Clone)]
pub struct AppState {
    pub store: Store,
    pub feed: Arc<FeedSettings>,
    pub feed_limit: u16,
    pub version: Option<Arc<str>>,
}

/// Builds the router with all middleware.
pub fn router(state: AppState, public_dir: &Path) -> Router {
    let static_files = ServeDir::new(public_dir)
        .call_fallback_on_method_not_allowed(true)
        .fallback(not_found.into_service());

    let mut router = Router::new()
        .route("/feed.json", get(feed).fallback(not_found))
        .route("/health", get(health).fallback(not_found))
        .fallback_service(static_files)
        .with_state(state)
        .layer(middleware::from_fn(log_request))
        .layer(CatchPanicLayer::custom(|_| {
            Problem::internal_error().into_response()
        }));
    for (name, value) in SECURITY_HEADERS {
        router = router.layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        ));
    }
    router
}

/// The default response headers of Helmet 8, which the TypeScript service
/// sent on every response.
pub const SECURITY_HEADERS: [(&str, &str); 12] = [
    (
        "content-security-policy",
        "default-src 'self';base-uri 'self';font-src 'self' https: data:;form-action 'self';frame-ancestors 'self';img-src 'self' data:;object-src 'none';script-src 'self';script-src-attr 'none';style-src 'self' https: 'unsafe-inline';upgrade-insecure-requests",
    ),
    ("cross-origin-opener-policy", "same-origin"),
    ("cross-origin-resource-policy", "same-origin"),
    ("origin-agent-cluster", "?1"),
    ("referrer-policy", "no-referrer"),
    (
        "strict-transport-security",
        "max-age=31536000; includeSubDomains",
    ),
    ("x-content-type-options", "nosniff"),
    ("x-dns-prefetch-control", "off"),
    ("x-download-options", "noopen"),
    ("x-frame-options", "SAMEORIGIN"),
    ("x-permitted-cross-domain-policies", "none"),
    ("x-xss-protection", "0"),
];

async fn feed(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, Problem> {
    let rows = tokio::time::timeout(FEED_DEADLINE, state.store.newest_rows(state.feed_limit))
        .await
        .map_err(|_| Problem::unavailable())?
        .map_err(|error| {
            tracing::warn!(code = error.code(), "the feed query failed");
            Problem::unavailable()
        })?;
    let body = render_feed(&state.feed, &rows).map_err(|_| Problem::internal_error())?;

    let etag = weak_etag(&body);
    let mut response = if if_none_match(&headers, &etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        (
            [(CONTENT_TYPE, HeaderValue::from_static(FEED_CONTENT_TYPE))],
            body,
        )
            .into_response()
    };
    if let Ok(value) = HeaderValue::from_str(&etag) {
        response.headers_mut().insert(ETAG, value);
    }
    Ok(response)
}

/// A weak validator from the body length and a 64-bit hash. Feed readers
/// send it back in `If-None-Match` and receive `304` while the feed is
/// unchanged. The value is stable within one build of the service.
fn weak_etag(body: &str) -> String {
    let mut hasher = DefaultHasher::new();
    body.hash(&mut hasher);
    format!("W/\"{:x}-{:016x}\"", body.len(), hasher.finish())
}

/// `If-None-Match` with weak comparison (RFC 9110 §13.1.2).
fn if_none_match(headers: &HeaderMap, etag: &str) -> bool {
    let opaque = |tag: &str| tag.trim().trim_start_matches("W/").to_owned();
    let current = opaque(etag);
    headers
        .get_all(IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|tag| tag.trim() == "*" || opaque(tag) == current)
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<Arc<str>>,
}

/// Liveness. It does not query the database or Mastodon.
async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        version: state.version,
    })
}

async fn not_found() -> Problem {
    Problem::not_found()
}

/// An RFC 9457 problem details response with a stable `code`.
#[derive(Debug, Serialize)]
pub struct Problem {
    #[serde(rename = "type")]
    kind: &'static str,
    title: &'static str,
    status: u16,
    code: &'static str,
}

impl Problem {
    fn new(status: StatusCode, title: &'static str, code: &'static str) -> Self {
        Self {
            kind: "about:blank",
            title,
            status: status.as_u16(),
            code,
        }
    }

    fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "Not Found", "not_found")
    }

    fn unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Service Unavailable",
            "unavailable",
        )
    }

    fn internal_error() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal Server Error",
            "internal_error",
        )
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let body = serde_json::to_string(&self).unwrap_or_default();
        (
            status,
            [(
                CONTENT_TYPE,
                HeaderValue::from_static("application/problem+json"),
            )],
            Body::from(body),
        )
            .into_response()
    }
}

/// A process-local request number for log correlation. It needs no random
/// source, so it cannot fail.
static REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Logs one line per request: method, route template, status, and time.
/// The path of an unmatched request is not logged, because it is free text.
async fn log_request(request: Request, next: Next) -> Response {
    let request_id = REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let method = request.method().as_str().to_owned();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "static".to_owned(), |path| path.as_str().to_owned());
    let started = tokio::time::Instant::now();
    let response = next.run(request).await;
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    tracing::info!(
        request_id,
        method,
        route,
        status = response.status().as_u16(),
        elapsed_ms,
        "request finished"
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(IF_NONE_MATCH, HeaderValue::from_str(value).unwrap());
        headers
    }

    #[test]
    fn the_etag_depends_on_the_body() {
        assert_eq!(weak_etag("a"), weak_etag("a"));
        assert_ne!(weak_etag("a"), weak_etag("b"));
        assert!(weak_etag("abc").starts_with("W/\"3-"));
    }

    #[test]
    fn if_none_match_uses_weak_comparison() {
        let etag = weak_etag("body");
        let strong = etag.trim_start_matches("W/").to_owned();
        assert!(if_none_match(&headers(&etag), &etag));
        assert!(if_none_match(&headers(&strong), &etag));
        assert!(if_none_match(
            &headers(&format!("\"other\", {etag}")),
            &etag
        ));
        assert!(if_none_match(&headers("*"), &etag));
        assert!(!if_none_match(&headers("\"other\""), &etag));
        assert!(!if_none_match(&HeaderMap::new(), &etag));
    }
}

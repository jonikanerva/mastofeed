//! HTTP behaviour of the router: routes, headers, conditional requests,
//! static files, and failure responses.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use axum::response::Response;
use mastofeed_domain::feed::FeedSettings;
use mastofeed_server::feed_cache::FeedCache;
use mastofeed_server::http::{AppState, FEED_CONTENT_TYPE, SECURITY_HEADERS, router};
use mastofeed_server::store::Store;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt as _;

use crate::common::public_dir;

/// An empty cache, as at the start of the process.
fn feed_cache(pool: &PgPool) -> FeedCache {
    FeedCache::new(
        Store::new(pool.clone()),
        Arc::new(FeedSettings {
            title: "Feed".to_owned(),
            description: None,
            home_page_url: "https://feed.example".to_owned(),
            mastodon_base_url: "https://home.example".to_owned(),
        }),
        200,
    )
}

fn app_with(feed: &FeedCache, version: Option<&str>) -> axum::Router {
    let state = AppState {
        feed: feed.clone(),
        version: version.map(Arc::from),
    };
    router(state, &public_dir())
}

/// A router with an empty cache: `/feed.json` loads the feed from the
/// database, as before the first refresh of the process.
fn app(pool: &PgPool, version: Option<&str>) -> axum::Router {
    app_with(&feed_cache(pool), version)
}

async fn insert_status(pool: &PgPool, id: &str) {
    sqlx::query(
        "INSERT INTO mastodon_statuses (id, created_at, url, content, account_id, account_username,
                                        account_display_name, account_url, raw)
         VALUES ($1, '2026-10-01T12:00:00Z'::timestamptz + ($1::int * interval '1 minute'),
                 'https://home.example/@a/' || $1, '<p>x</p>', '9', 'a', 'A',
                 'https://home.example/@a', '{}')",
    )
    .bind(id)
    .execute(pool)
    .await
    .unwrap();
}

fn item_urls(body: &[u8]) -> Vec<String> {
    let feed: Value = serde_json::from_slice(body).unwrap();
    feed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["url"].as_str().unwrap().to_owned())
        .collect()
}

async fn call(
    app: axum::Router,
    method: Method,
    uri: &str,
    headers: &[(&str, &str)],
) -> (Response, Vec<u8>) {
    let mut request = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = app
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX).await.unwrap().to_vec();
    (Response::from_parts(parts, Body::empty()), bytes)
}

fn header_value<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get(name)
        .map(|value| value.to_str().unwrap())
}

fn assert_security_headers(response: &Response) {
    for (name, value) in SECURITY_HEADERS {
        assert_eq!(header_value(response, name), Some(value), "{name}");
    }
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn the_feed_has_its_media_type_and_a_validator(pool: PgPool) {
    let (response, body) = call(app(&pool, None), Method::GET, "/feed.json", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_value(&response, "content-type"),
        Some(FEED_CONTENT_TYPE)
    );
    assert!(header_value(&response, "etag").unwrap().starts_with("W/\""));
    assert_security_headers(&response);
    let feed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(feed["items"], json!([]));
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_matching_validator_gives_not_modified(pool: PgPool) {
    let (first, _) = call(app(&pool, None), Method::GET, "/feed.json", &[]).await;
    let etag = header_value(&first, "etag").unwrap().to_owned();
    let (response, body) = call(
        app(&pool, None),
        Method::GET,
        "/feed.json",
        &[("if-none-match", &etag)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(header_value(&response, "etag"), Some(etag.as_str()));
    assert_eq!(body, Vec::<u8>::new());

    let (response, _) = call(
        app(&pool, None),
        Method::GET,
        "/feed.json",
        &[("if-none-match", "W/\"stale\"")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn head_requests_have_no_body(pool: PgPool) {
    let (response, body) = call(app(&pool, None), Method::HEAD, "/feed.json", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_value(&response, "content-type"),
        Some(FEED_CONTENT_TYPE)
    );
    assert_eq!(body, Vec::<u8>::new());
}

/// The production path: after a refresh, the feed, the validator, the
/// conditional request, and HEAD need no database.
#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_cached_feed_needs_no_database(pool: PgPool) {
    insert_status(&pool, "1").await;
    let (_, from_database) = call(app(&pool, None), Method::GET, "/feed.json", &[]).await;
    let feed = feed_cache(&pool);
    feed.refresh().await.unwrap();
    pool.close().await;

    let (response, body) = call(app_with(&feed, None), Method::GET, "/feed.json", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_value(&response, "content-type"),
        Some(FEED_CONTENT_TYPE)
    );
    assert_security_headers(&response);
    assert_eq!(body, from_database);
    assert_eq!(item_urls(&body), ["https://home.example/@a/1"]);
    let etag = header_value(&response, "etag").unwrap().to_owned();
    assert!(etag.starts_with("W/\""));

    let (response, body) = call(
        app_with(&feed, None),
        Method::GET,
        "/feed.json",
        &[("if-none-match", &etag)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(header_value(&response, "etag"), Some(etag.as_str()));
    assert_eq!(body, Vec::<u8>::new());

    let (response, body) = call(app_with(&feed, None), Method::HEAD, "/feed.json", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_value(&response, "etag"), Some(etag.as_str()));
    assert_eq!(body, Vec::<u8>::new());
}

/// A cached feed does not read the table. A new row shows after the next
/// refresh, which the poller runs after each sync.
#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_cached_feed_changes_only_at_a_refresh(pool: PgPool) {
    insert_status(&pool, "1").await;
    let feed = feed_cache(&pool);
    feed.refresh().await.unwrap();
    let (first, _) = call(app_with(&feed, None), Method::GET, "/feed.json", &[]).await;
    let first_etag = header_value(&first, "etag").unwrap().to_owned();

    insert_status(&pool, "2").await;
    let (response, body) = call(app_with(&feed, None), Method::GET, "/feed.json", &[]).await;
    assert_eq!(header_value(&response, "etag"), Some(first_etag.as_str()));
    assert_eq!(item_urls(&body), ["https://home.example/@a/1"]);

    feed.refresh().await.unwrap();
    let (response, body) = call(app_with(&feed, None), Method::GET, "/feed.json", &[]).await;
    assert_ne!(header_value(&response, "etag"), Some(first_etag.as_str()));
    assert_eq!(
        item_urls(&body),
        ["https://home.example/@a/1", "https://home.example/@a/2"]
    );
}

/// A failed refresh keeps the document of the last successful refresh.
#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_failed_refresh_keeps_the_previous_feed(pool: PgPool) {
    insert_status(&pool, "1").await;
    let feed = feed_cache(&pool);
    feed.refresh().await.unwrap();
    let before = feed.cached().unwrap();
    pool.close().await;

    let error = feed.refresh().await.unwrap_err();
    assert_eq!(error.code(), "query");
    assert_eq!(feed.cached().unwrap(), before);
    let (response, body) = call(app_with(&feed, None), Method::GET, "/feed.json", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_value(&response, "etag"), Some(before.etag()));
    assert_eq!(item_urls(&body), ["https://home.example/@a/1"]);
}

/// Before the first refresh the cache is empty, so the handler needs the
/// database. A load for one request does not fill the cache.
#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_database_failure_gives_a_problem_response(pool: PgPool) {
    let feed = feed_cache(&pool);
    let (response, _) = call(app_with(&feed, None), Method::GET, "/feed.json", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(feed.cached(), None);

    let app = app(&pool, None);
    pool.close().await;
    let (response, body) = call(app, Method::GET, "/feed.json", &[]).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        header_value(&response, "content-type"),
        Some("application/problem+json")
    );
    assert_security_headers(&response);
    let problem: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        problem,
        json!({"type": "about:blank", "title": "Service Unavailable", "status": 503, "code": "unavailable"})
    );
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn health_reports_the_version_without_the_database(pool: PgPool) {
    let app_with_version = app(&pool, Some("abc123"));
    let app_without_version = app(&pool, None);
    pool.close().await;
    let (response, body) = call(app_with_version, Method::GET, "/health", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"status": "ok", "version": "abc123"})
    );
    assert_security_headers(&response);
    let (_, body) = call(app_without_version, Method::GET, "/health", &[]).await;
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap(),
        json!({"status": "ok"})
    );
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn the_landing_page_and_the_favicon_are_static_files(pool: PgPool) {
    let (response, body) = call(app(&pool, None), Method::GET, "/", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_value(&response, "content-type"), Some("text/html"));
    assert_eq!(
        body,
        std::fs::read(public_dir().join("index.html")).unwrap()
    );
    assert_security_headers(&response);

    let (response, body) = call(app(&pool, None), Method::GET, "/favicon.png", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_value(&response, "content-type"), Some("image/png"));
    assert_eq!(
        body,
        std::fs::read(public_dir().join("favicon.png")).unwrap()
    );

    let (response, _) = call(app(&pool, None), Method::GET, "/index.html", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn unknown_paths_and_methods_give_not_found_problems(pool: PgPool) {
    for (method, uri) in [
        (Method::GET, "/missing"),
        (Method::GET, "/../Cargo.toml"),
        (Method::GET, "/%2e%2e/Cargo.toml"),
        (Method::POST, "/feed.json"),
        (Method::DELETE, "/health"),
        (Method::POST, "/"),
    ] {
        let (response, body) = call(app(&pool, None), method.clone(), uri, &[]).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {uri}");
        assert_eq!(
            header_value(&response, "content-type"),
            Some("application/problem+json")
        );
        assert_security_headers(&response);
        let problem: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(problem["code"], "not_found");
    }
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn the_feed_lists_stored_statuses(pool: PgPool) {
    sqlx::query(
        "INSERT INTO mastodon_statuses (id, created_at, url, content, account_id, account_username,
                                        account_display_name, account_url, raw)
         VALUES ('1', '2026-10-01T12:00:00Z', 'https://home.example/@a/1', '<p>x</p>', '9', 'a', 'A',
                 'https://home.example/@a', '{}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let (_, body) = call(app(&pool, None), Method::GET, "/feed.json", &[]).await;
    let feed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(feed["items"][0]["url"], "https://home.example/@a/1");
    assert_eq!(
        feed["items"][0]["date_published"],
        "2026-10-01T12:00:00.000Z"
    );
}

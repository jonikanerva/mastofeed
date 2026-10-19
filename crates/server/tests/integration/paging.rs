//! The paging of the newer statuses in one sync (issue #9).
//!
//! The fake server holds a home timeline with numeric ids and answers like
//! Mastodon: newest first, at most `limit` entries, only ids above `since_id`
//! and below `max_id`. Like Mastodon, it applies the limit before it removes
//! deleted statuses. The database holds one status with id 100 before the
//! sync. `parity.rs` checks a small gap against the TypeScript service.

use std::sync::Arc;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use mastofeed_domain::status::normalize_status;
use mastofeed_server::clock::FixedClock;
use mastofeed_server::config::Secret;
use mastofeed_server::mastodon::keys::camel_case_keys;
use mastofeed_server::mastodon::{ClientSettings, MastodonClient, MastodonError};
use mastofeed_server::poller::{MAX_SYNC_REQUESTS, Paging, Poller, SyncError, SyncReport};
use mastofeed_server::store::Store;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::common::{FakeServer, json_response};

/// The id of the status in the database before the sync.
const STORED: u64 = 100;

/// One timeline entry as Mastodon sends it (`snake_case` keys). The creation
/// time grows with the id, like a Mastodon snowflake id.
fn entry(id: u64) -> Value {
    json!({
        "id": id.to_string(),
        "created_at": format!(
            "2026-10-02T{:02}:{:02}:{:02}.000Z",
            id / 3600,
            id / 60 % 60,
            id % 60
        ),
        "edited_at": null,
        "url": format!("https://home.example/@a/{id}"),
        "content": format!("<p>status {id}</p>"),
        "spoiler_text": "",
        "account": {
            "id": "9",
            "acct": "a",
            "display_name": "A",
            "url": "https://home.example/@a",
            "avatar": "https://files.example/a.png",
            "avatar_static": "https://files.example/a-static.png",
        },
        "media_attachments": [],
    })
}

fn query_id(request: &Request, key: &str) -> Option<u64> {
    let query = request.uri().query().unwrap_or_default();
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == key)
        .map(|(_, value)| value.parse().unwrap())
}

/// One page of a timeline with the ids `STORED..=STORED + new`, newest first.
fn timeline_page(new: u64, request: &Request) -> Response {
    timeline_page_with_deleted(new, &[], request)
}

/// One page of the same timeline, after the deletion of `deleted`.
fn timeline_page_with_deleted(new: u64, deleted: &[u64], request: &Request) -> Response {
    let limit = usize::try_from(query_id(request, "limit").unwrap()).unwrap();
    let since = query_id(request, "since_id").unwrap_or(0);
    let max = query_id(request, "max_id").unwrap_or(u64::MAX);
    let page: Vec<Value> = (STORED..=STORED + new)
        .rev()
        .filter(|id| *id > since && *id < max)
        .take(limit)
        .filter(|id| !deleted.contains(id))
        .map(entry)
        .collect();
    json_response(Value::Array(page).to_string())
}

fn poller(server: &FakeServer, pool: &PgPool) -> Poller {
    poller_with(server, pool, ClientSettings::default())
}

fn poller_with(server: &FakeServer, pool: &PgPool, settings: ClientSettings) -> Poller {
    let client = MastodonClient::new(
        &server.base_url(),
        &Secret::new("test-token".to_owned()),
        settings,
    )
    .unwrap();
    let clock = Arc::new(FixedClock("2026-10-14T00:00:00Z".parse().unwrap()));
    Poller::new(client, Store::new(pool.clone()), clock)
}

async fn store_status(pool: &PgPool, id: u64) {
    let record = normalize_status(
        camel_case_keys(entry(id)),
        "2026-10-14T00:00:00Z".parse().unwrap(),
    )
    .unwrap();
    let written = Store::new(pool.clone())
        .upsert_statuses(&[record])
        .await
        .unwrap();
    assert_eq!(written, 1);
}

async fn stored_ids(pool: &PgPool) -> Vec<u64> {
    let ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM mastodon_statuses ORDER BY created_at")
            .fetch_all(pool)
            .await
            .unwrap();
    ids.into_iter().map(|id| id.parse().unwrap()).collect()
}

fn paths(server: &FakeServer) -> Vec<String> {
    server
        .recorded()
        .into_iter()
        .map(|recorded| recorded.path)
        .collect()
}

const HOME: &str = "/api/v1/timelines/home";

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_burst_of_more_than_40_statuses_is_stored_completely(pool: PgPool) {
    store_status(&pool, STORED).await;
    let server = FakeServer::start(|request| timeline_page(95, request)).await;
    let report = poller(&server, &pool).sync_once().await.unwrap();

    assert_eq!(
        paths(&server),
        [
            format!("{HOME}?limit=40"),
            format!("{HOME}?limit=40&since_id=100"),
            format!("{HOME}?limit=40&since_id=100&max_id=156"),
            format!("{HOME}?limit=40&since_id=100&max_id=116"),
            format!("{HOME}?limit=40&since_id=100&max_id=101"),
        ]
    );
    assert_eq!(
        report,
        SyncReport {
            received: 40 + 95,
            records: 95,
            rejected: 0,
            written: 95,
            paging: Paging::Complete,
        }
    );
    assert_eq!(stored_ids(&pool).await, (100..=195).collect::<Vec<_>>());
}

/// Mastodon can send a short page while older statuses exist: it applies
/// the limit before it removes deleted statuses. A short page must not end
/// the paging.
#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_short_page_does_not_end_the_paging(pool: PgPool) {
    store_status(&pool, STORED).await;
    let server = FakeServer::start(|request| timeline_page_with_deleted(95, &[190], request)).await;
    let report = poller(&server, &pool).sync_once().await.unwrap();

    assert_eq!(
        paths(&server)[2..],
        [
            format!("{HOME}?limit=40&since_id=100&max_id=156"),
            format!("{HOME}?limit=40&since_id=100&max_id=116"),
            format!("{HOME}?limit=40&since_id=100&max_id=101"),
        ]
    );
    assert_eq!((report.written, report.paging), (94, Paging::Complete));
    let expected: Vec<u64> = (100..=195).filter(|id| *id != 190).collect();
    assert_eq!(stored_ids(&pool).await, expected);
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn an_empty_page_ends_the_paging(pool: PgPool) {
    store_status(&pool, STORED).await;
    let server = FakeServer::start(|request| timeline_page(40, request)).await;
    let report = poller(&server, &pool).sync_once().await.unwrap();

    assert_eq!(
        paths(&server),
        [
            format!("{HOME}?limit=40"),
            format!("{HOME}?limit=40&since_id=100"),
            format!("{HOME}?limit=40&since_id=100&max_id=101"),
        ]
    );
    assert_eq!((report.written, report.paging), (40, Paging::Complete));
    assert_eq!(stored_ids(&pool).await, (100..=140).collect::<Vec<_>>());
}

fn max_requests() -> usize {
    usize::try_from(MAX_SYNC_REQUESTS).unwrap()
}

/// At the cap, the sync stores the newest part of the gap and reports the
/// cap. The oldest part of the gap stays missing: the next sync starts from
/// the new newest stored status. The entries of the check request after
/// the cap are not stored.
#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn the_page_cap_stops_the_requests_and_is_reported(pool: PgPool) {
    store_status(&pool, STORED).await;
    let server = FakeServer::start(|request| timeline_page(300, request)).await;
    let report = poller(&server, &pool).sync_once().await.unwrap();

    let recorded = paths(&server);
    assert_eq!(recorded.len(), max_requests());
    assert_eq!(
        recorded[recorded.len() - 2..],
        [
            format!("{HOME}?limit=40&since_id=100&max_id=241"),
            format!("{HOME}?limit=40&since_id=100&max_id=201"),
        ]
    );
    assert_eq!(
        report,
        SyncReport {
            received: 40 + 200,
            records: 200,
            rejected: 0,
            written: 200,
            paging: Paging::PageCap,
        }
    );
    let mut expected = vec![100];
    expected.extend(201..=400);
    assert_eq!(stored_ids(&pool).await, expected);
}

/// A gap of exactly five pages fills the cap, but nothing is missing.
#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_gap_that_ends_at_the_cap_is_complete(pool: PgPool) {
    store_status(&pool, STORED).await;
    let server = FakeServer::start(|request| timeline_page(200, request)).await;
    let report = poller(&server, &pool).sync_once().await.unwrap();

    let recorded = paths(&server);
    assert_eq!(recorded.len(), max_requests());
    assert_eq!(
        recorded.last().unwrap(),
        &format!("{HOME}?limit=40&since_id=100&max_id=101")
    );
    assert_eq!((report.written, report.paging), (200, Paging::Complete));
    assert_eq!(stored_ids(&pool).await, (100..=300).collect::<Vec<_>>());
}

/// A deleted status right below the cap does not hide the rest of the gap
/// from the check request.
#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_deleted_status_below_the_cap_does_not_hide_the_cap(pool: PgPool) {
    store_status(&pool, STORED).await;
    let server =
        FakeServer::start(|request| timeline_page_with_deleted(300, &[200], request)).await;
    let report = poller(&server, &pool).sync_once().await.unwrap();

    assert_eq!((report.written, report.paging), (200, Paging::PageCap));
}

/// The bodies of one sync stay within two maximal bodies. A request starts
/// only if a maximal body still fits.
#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn the_body_budget_stops_the_requests_and_is_reported(pool: PgPool) {
    store_status(&pool, STORED).await;
    let full_page = Value::Array((156..=195).rev().map(entry).collect())
        .to_string()
        .len();
    let settings = ClientSettings {
        max_body_bytes: full_page + 100,
        ..ClientSettings::default()
    };
    let server = FakeServer::start(|request| timeline_page(95, request)).await;
    let report = poller_with(&server, &pool, settings)
        .sync_once()
        .await
        .unwrap();

    assert_eq!(
        paths(&server),
        [
            format!("{HOME}?limit=40"),
            format!("{HOME}?limit=40&since_id=100"),
        ]
    );
    assert_eq!((report.written, report.paging), (40, Paging::BodyBudget));
}

/// The cursor is the id of the timeline entry. For a boost, that is the id
/// of the boost, not of the boosted status.
#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn the_cursor_of_a_boost_is_the_id_of_the_boost(pool: PgPool) {
    store_status(&pool, STORED).await;
    let server = FakeServer::start(|request| {
        if query_id(request, "since_id").is_none() || query_id(request, "max_id").is_some() {
            return json_response("[]");
        }
        let mut boost = entry(101);
        boost["reblog"] = entry(50);
        json_response(Value::Array(vec![entry(102), boost]).to_string())
    })
    .await;
    let report = poller(&server, &pool).sync_once().await.unwrap();

    assert_eq!(
        paths(&server).last().unwrap(),
        &format!("{HOME}?limit=40&since_id=100&max_id=101")
    );
    assert_eq!((report.written, report.paging), (2, Paging::Complete));
    assert_eq!(stored_ids(&pool).await, [50, 100, 102]);
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_failure_on_a_later_page_fails_the_sync_and_writes_nothing(pool: PgPool) {
    store_status(&pool, STORED).await;
    let server = FakeServer::start(|request| {
        if query_id(request, "max_id") == Some(116) {
            StatusCode::BAD_GATEWAY.into_response()
        } else {
            timeline_page(95, request)
        }
    })
    .await;
    let result = poller(&server, &pool).sync_once().await;

    assert_eq!(result, Err(SyncError::Mastodon(MastodonError::Status(502))));
    assert_eq!(paths(&server).len(), 4);
    assert_eq!(stored_ids(&pool).await, [STORED]);
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_page_without_a_last_id_stops_the_requests_and_is_reported(pool: PgPool) {
    store_status(&pool, STORED).await;
    let server = FakeServer::start(|request| {
        if query_id(request, "since_id").is_none() {
            return json_response("[]");
        }
        assert_eq!(query_id(request, "max_id"), None, "no cursor exists");
        let mut page: Vec<Value> = (101..=140).rev().map(entry).collect();
        page.last_mut()
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("id");
        json_response(Value::Array(page).to_string())
    })
    .await;
    let report = poller(&server, &pool).sync_once().await.unwrap();

    assert_eq!(paths(&server).len(), 2);
    assert_eq!(
        (report.records, report.rejected, report.paging),
        (39, 1, Paging::MissingCursor)
    );
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn an_empty_database_requests_one_page_only(pool: PgPool) {
    let server = FakeServer::start(|request| timeline_page(95, request)).await;
    let report = poller(&server, &pool).sync_once().await.unwrap();

    assert_eq!(paths(&server), [format!("{HOME}?limit=40")]);
    assert_eq!((report.written, report.paging), (40, Paging::Complete));
}

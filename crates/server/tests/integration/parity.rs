//! Parity with the TypeScript service (commit d17168d).
//!
//! `tests/fixtures/golden` holds the output of the TypeScript service for the
//! synthetic timeline in `tests/fixtures/mastodon`. `generate.ts` and the
//! README in that directory record how it was produced. These tests run the
//! same timeline through the Rust poller and compare the requests, the
//! stored rows, and the rendered feed.

use std::sync::Arc;

use mastofeed_domain::feed::{FeedSettings, render_feed};
use mastofeed_server::clock::FixedClock;
use mastofeed_server::config::Secret;
use mastofeed_server::mastodon::{ClientSettings, MastodonClient};
use mastofeed_server::poller::Poller;
use mastofeed_server::store::Store;
use serde_json::Value;
use sqlx::PgPool;

use crate::common::{FakeServer, Recorded, fixture, read_json, timeline_fixture};

/// The settings that produced the golden feeds. The golden generator served
/// the fake timeline at this base URL, so the item links contain it.
fn golden_settings(description: Option<&str>) -> FeedSettings {
    FeedSettings {
        title: "Fixture feed".to_owned(),
        description: description.map(str::to_owned),
        home_page_url: "https://feed.example/".to_owned(),
        mastodon_base_url: "http://127.0.0.1:48080".to_owned(),
    }
}

async fn sync_twice(pool: &PgPool) -> FakeServer {
    let server = FakeServer::start(timeline_fixture).await;
    let client = MastodonClient::new(
        &server.base_url(),
        &Secret::new("fixture-token".to_owned()),
        ClientSettings::default(),
    )
    .unwrap();
    let clock = Arc::new(FixedClock("2026-10-14T00:00:00Z".parse().unwrap()));
    let poller = Poller::new(client, Store::new(pool.clone()), clock);
    let first = poller.sync_once().await.unwrap();
    assert_eq!(
        (first.received, first.records, first.rejected, first.written),
        (7, 5, 1, 5)
    );
    let second = poller.sync_once().await.unwrap();
    // The second poll writes only the fresh status. The change guard skips
    // the five unchanged statuses.
    assert_eq!(
        (
            second.received,
            second.records,
            second.rejected,
            second.written
        ),
        (8, 6, 1, 1)
    );
    server
}

async fn render(pool: &PgPool, settings: &FeedSettings, limit: u16) -> String {
    let rows = Store::new(pool.clone()).newest_rows(limit).await.unwrap();
    render_feed(settings, &rows).unwrap()
}

fn golden_text(relative: &str) -> String {
    std::fs::read_to_string(fixture(relative)).unwrap()
}

/// The gate is equality of the parsed JSON. Byte equality is checked as
/// well, because feed readers can compare raw bytes.
fn assert_same_feed(actual: &str, golden: &str) {
    let actual_value: Value = serde_json::from_str(actual).unwrap();
    let golden_value: Value = serde_json::from_str(golden).unwrap();
    assert_eq!(actual_value, golden_value);
    assert_eq!(actual, golden, "the feed bytes differ from the golden file");
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn requests_match_the_typescript_service(pool: PgPool) {
    let server = sync_twice(&pool).await;
    let golden: Vec<Recorded> =
        serde_json::from_value(read_json("golden/sync/requests.json")).unwrap();
    assert_eq!(server.recorded(), golden);
}

/// The comparison uses jsonb equality, the equality of the change guard.
/// JavaScript wrote `60.0` as `60`; Rust keeps `60.0`. jsonb compares both
/// as the same number, so the guard and the feed see no difference.
#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn stored_raw_objects_match_the_typescript_service(pool: PgPool) {
    sync_twice(&pool).await;
    let golden = read_json("golden/sync/raw.json");
    let (stored, matching): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE m.raw = g.value)
         FROM mastodon_statuses m JOIN jsonb_each($1::jsonb) g ON g.key = m.id",
    )
    .bind(&golden)
    .fetch_one(&pool)
    .await
    .unwrap();
    let golden_count = i64::try_from(golden.as_object().unwrap().len()).unwrap();
    assert_eq!((stored, matching), (golden_count, golden_count));

    let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM mastodon_statuses ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    let golden_ids: Vec<&String> = golden.as_object().unwrap().keys().collect();
    assert_eq!(ids.iter().collect::<Vec<_>>(), golden_ids);
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn stored_columns_match_the_typescript_service(pool: PgPool) {
    sync_twice(&pool).await;
    let rows: Vec<(Value,)> = sqlx::query_as(
        r#"
        SELECT json_build_object(
            'id', id,
            'created_at', to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
            'edited_at', to_char(edited_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
            'url', url,
            'content', content,
            'spoiler_text', spoiler_text,
            'account_id', account_id,
            'account_username', account_username,
            'account_display_name', account_display_name,
            'account_url', account_url
        )::jsonb
        FROM mastodon_statuses ORDER BY id
        "#,
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let actual = Value::Array(rows.into_iter().map(|(row,)| row).collect());
    assert_eq!(actual, read_json("golden/sync/rows.json"));
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn the_feed_matches_the_typescript_service(pool: PgPool) {
    sync_twice(&pool).await;
    let feed = render(&pool, &golden_settings(Some("Fixture description")), 200).await;
    assert_same_feed(&feed, &golden_text("golden/sync/feed.json"));
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_limited_feed_matches_the_typescript_service(pool: PgPool) {
    sync_twice(&pool).await;
    let feed = render(&pool, &golden_settings(None), 3).await;
    assert_same_feed(&feed, &golden_text("golden/limit3/feed.json"));
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn an_empty_feed_matches_the_typescript_service(pool: PgPool) {
    let feed = render(&pool, &golden_settings(None), 200).await;
    assert_same_feed(&feed, &golden_text("golden/empty/feed.json"));
}

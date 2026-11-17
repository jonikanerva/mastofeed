//! The poller and the feed cache together: `Poller::run` refreshes the
//! cache after each sync, and `/feed.json` serves the cached document.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse as _, Response};
use mastofeed_domain::feed::FeedSettings;
use mastofeed_server::clock::FixedClock;
use mastofeed_server::config::Secret;
use mastofeed_server::feed_cache::FeedCache;
use mastofeed_server::http::{AppState, router};
use mastofeed_server::mastodon::{ClientSettings, MastodonClient};
use mastofeed_server::poller::Poller;
use mastofeed_server::store::Store;
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::watch;
use tower::ServiceExt as _;

use crate::common::{FakeServer, public_dir, timeline_fixture};

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

/// Starts `Poller::run` with a schedule that has no time during the test,
/// so only the sync at the start runs. Waits until the poller has filled
/// the cache, then stops the poller.
async fn run_first_sync(server: &FakeServer, pool: &PgPool, feed: &FeedCache) {
    let client = MastodonClient::new(
        &server.base_url(),
        &Secret::new("fixture-token".to_owned()),
        ClientSettings::default(),
    )
    .unwrap();
    let clock = Arc::new(FixedClock("2026-10-14T00:00:00Z".parse().unwrap()));
    let poller = Poller::new(client, Store::new(pool.clone()), clock);
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(poller.run("0 0 1 1 *".parse().unwrap(), feed.clone(), stopped));

    tokio::time::timeout(Duration::from_secs(20), async {
        while feed.cached().is_none() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.send_replace(true);
    task.await.unwrap();
}

async fn item_ids(feed: &FeedCache) -> Vec<String> {
    let app = router(
        AppState {
            feed: feed.clone(),
            version: None,
        },
        &public_dir(),
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/feed.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let document: Value = serde_json::from_slice(&body).unwrap();
    document["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect()
}

/// The complete path: a sync stores the timeline, the poller refreshes the
/// cache, and the router serves the stored statuses without the database.
#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn the_poller_fills_the_cache_after_a_sync(pool: PgPool) {
    let server = FakeServer::start(timeline_fixture).await;
    let feed = feed_cache(&pool);
    assert_eq!(feed.cached(), None);
    run_first_sync(&server, &pool, &feed).await;

    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM mastodon_statuses")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, 5);
    pool.close().await;
    assert_eq!(item_ids(&feed).await.len(), 5);
}

/// A failed sync writes nothing, but the poller still refreshes the cache,
/// so the feed shows the stored statuses (VISION.md failure behaviour).
#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_failed_sync_still_fills_the_cache(pool: PgPool) {
    sqlx::query(
        "INSERT INTO mastodon_statuses (id, created_at, url, content, account_id, account_username,
                                        account_display_name, account_url, raw)
         VALUES ('1', '2026-10-01T12:00:00Z', 'https://home.example/@a/1', '<p>x</p>', '9', 'a', 'A',
                 'https://home.example/@a', '{}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let server =
        FakeServer::start(|_| -> Response { StatusCode::INTERNAL_SERVER_ERROR.into_response() })
            .await;
    let feed = feed_cache(&pool);
    run_first_sync(&server, &pool, &feed).await;

    // The sync reached Mastodon and failed there, not earlier.
    assert_eq!(server.recorded().len(), 1);
    pool.close().await;
    assert_eq!(item_ids(&feed).await, ["1"]);
}

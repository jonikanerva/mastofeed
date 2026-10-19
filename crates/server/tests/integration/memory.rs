//! The memory of one sync at the body budget (PR #10, STACK.md §2.3).
//!
//! These tests measure; they check nothing. They are ignored by default.
//! Run them on an idle machine with a release build:
//!
//! ```text
//! mise run db:start
//! DATABASE_URL=postgres://postgres@127.0.0.1:55432/postgres SQLX_OFFLINE=true \
//!   cargo nextest run --release --locked --run-ignored only \
//!   -E 'test(memory::)' --test-threads 1 --no-capture
//! ```
//!
//! Each test serves two pages just below the body limit, so the body budget
//! stops the paging after the second page. It samples the resident set size
//! of the test process with `ps` every 10 ms and prints the increase. The
//! `tags` shape holds hashtag objects, like a Mastodon status. The other
//! shapes are synthetic and dense: a long array of small numbers, many
//! one-element arrays, and many one-key objects. The memory bound covers
//! Mastodon status entities only (STACK.md §2.3). The bodies are built as
//! text, so their construction leaves no freed memory that the sync can
//! reuse.

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::response::Response;
use mastofeed_domain::status::normalize_status;
use mastofeed_server::clock::FixedClock;
use mastofeed_server::config::Secret;
use mastofeed_server::mastodon::keys::camel_case_keys;
use mastofeed_server::mastodon::{ClientSettings, MastodonClient};
use mastofeed_server::poller::Poller;
use mastofeed_server::store::Store;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::common::FakeServer;

fn base_entry(id: u64) -> Value {
    json!({
        "id": id.to_string(),
        "created_at": format!("2026-10-02T{:02}:{:02}:{:02}.000Z", id / 3600, id / 60 % 60, id % 60),
        "url": format!("https://home.example/@a/{id}"),
        "content": "<p>x</p>",
        "spoiler_text": "",
        "account": {"id": "9", "acct": "a", "display_name": "A", "url": "https://home.example/@a",
            "avatar": "https://files.example/a.png", "avatar_static": "https://files.example/a-static.png"},
        "media_attachments": [],
    })
}

fn page(ids: impl Iterator<Item = u64>, shape: &str, target: usize) -> Bytes {
    let ids: Vec<u64> = ids.collect();
    let per_entry = target / ids.len();
    let mut out = String::with_capacity(target + 4096);
    out.push('[');
    for (k, id) in ids.iter().enumerate() {
        if k > 0 {
            out.push(',');
        }
        let base = base_entry(*id).to_string();
        out.push_str(&base[..base.len() - 1]);
        out.push_str(",\"tags\":[");
        if shape == "tags" {
            for i in 0..per_entry / 62 {
                if i > 0 {
                    out.push(',');
                }
                write!(
                    out,
                    "{{\"name\":\"tag{i:05}\",\"url\":\"https://h.example/tags/t{i:05}\"}}"
                )
                .unwrap();
            }
        } else {
            let (item, size) = match shape {
                "ones" => ("1", 2),
                "single_arrays" => ("[1]", 4),
                _ => ("{\"a\":1}", 8),
            };
            for i in 0..per_entry / size {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(item);
            }
        }
        out.push_str("]}");
    }
    out.push(']');
    Bytes::from(out)
}

fn rss_kib() -> u64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[expect(clippy::print_stdout, reason = "the measurement result is the output")]
async fn measure(pool: PgPool, shape: &'static str) {
    let record = normalize_status(
        camel_case_keys(base_entry(100)),
        "2026-10-14T00:00:00Z".parse().unwrap(),
    )
    .unwrap();
    Store::new(pool.clone())
        .upsert_statuses(&[record])
        .await
        .unwrap();
    let target = ClientSettings::default().max_body_bytes - 32 * 1024;
    let first = page((261..=300).rev(), shape, target);
    let second = page((221..=260).rev(), shape, target);
    let sizes = (first.len(), second.len());
    let bodies = Arc::new((first, second));
    let server = FakeServer::start(move |request| {
        let query = request.uri().query().unwrap_or_default();
        let body = if query.contains("max_id") {
            Bytes::from_static(b"[]")
        } else if query.contains("since_id") {
            bodies.1.clone()
        } else {
            bodies.0.clone()
        };
        Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap()
    })
    .await;
    let client = MastodonClient::new(
        &server.base_url(),
        &Secret::new("t".to_owned()),
        ClientSettings::default(),
    )
    .unwrap();
    let poller = Poller::new(
        client,
        Store::new(pool.clone()),
        Arc::new(FixedClock("2026-10-14T00:00:00Z".parse().unwrap())),
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let idle = rss_kib();
    let peak = Arc::new(AtomicU64::new(idle));
    let stop = Arc::new(AtomicBool::new(false));
    let sampler = {
        let (peak, stop) = (peak.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                peak.fetch_max(rss_kib(), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };
    let report = poller.sync_once().await.unwrap();
    stop.store(true, Ordering::Relaxed);
    sampler.join().unwrap();
    let peak = peak.load(Ordering::Relaxed);
    println!(
        "memory shape={shape} bodies={sizes:?} report={report:?} idle_kib={idle} peak_kib={peak} increase_kib={}",
        peak - idle
    );
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
#[ignore = "a measurement, not a check; see the module documentation"]
async fn tags_shape(pool: PgPool) {
    measure(pool, "tags").await;
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
#[ignore = "a measurement, not a check; see the module documentation"]
async fn ones_shape(pool: PgPool) {
    measure(pool, "ones").await;
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
#[ignore = "a measurement, not a check; see the module documentation"]
async fn single_arrays_shape(pool: PgPool) {
    measure(pool, "single_arrays").await;
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
#[ignore = "a measurement, not a check; see the module documentation"]
async fn small_objects_shape(pool: PgPool) {
    measure(pool, "small_objects").await;
}

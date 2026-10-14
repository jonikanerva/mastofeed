//! The change guard and the atomicity of the status upsert.

use jiff::Timestamp;
use mastofeed_domain::status::{StatusRecord, normalize_status};
use mastofeed_server::store::{Store, StoreError};
use serde_json::{Value, json};
use sqlx::PgPool;

fn now() -> Timestamp {
    "2026-10-14T00:00:00Z".parse().unwrap()
}

fn raw(id: &str) -> Value {
    json!({
        "id": id,
        "createdAt": "2026-10-01T12:00:00.000Z",
        "editedAt": null,
        "url": format!("https://home.example/@a/{id}"),
        "content": "<p>original</p>",
        "spoilerText": "",
        "favouritesCount": 1,
        "reblogsCount": 0,
        "repliesCount": 0,
        "account": {
            "id": "9",
            "acct": "a",
            "displayName": "A",
            "url": "https://home.example/@a",
            "avatar": "https://files.example/a.png",
            "avatarStatic": "https://files.example/a-static.png",
        },
        "mediaAttachments": [{"type": "image", "url": "https://files.example/1.jpg"}],
    })
}

fn record(raw: Value) -> StatusRecord {
    normalize_status(raw, now()).unwrap()
}

/// The PostgreSQL row version. It changes on every rewrite of the row.
async fn row_version(pool: &PgPool, id: &str) -> String {
    sqlx::query_scalar("SELECT xmin::text FROM mastodon_statuses WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn stored_raw(pool: &PgPool, id: &str) -> Value {
    sqlx::query_scalar("SELECT raw FROM mastodon_statuses WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Stores `raw("1")`, then upserts `changed` and returns the number of
/// written rows and whether the row version changed.
async fn upsert_change(pool: &PgPool, changed: Value) -> (u64, bool) {
    let store = Store::new(pool.clone());
    assert_eq!(store.upsert_statuses(&[record(raw("1"))]).await.unwrap(), 1);
    let before = row_version(pool, "1").await;
    let written = store.upsert_statuses(&[record(changed)]).await.unwrap();
    (written, row_version(pool, "1").await != before)
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn an_unchanged_status_is_not_rewritten(pool: PgPool) {
    assert_eq!(upsert_change(&pool, raw("1")).await, (0, false));
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_change_of_interaction_counts_only_is_not_rewritten(pool: PgPool) {
    let mut changed = raw("1");
    changed["favouritesCount"] = json!(99);
    changed["reblogsCount"] = json!(5);
    changed["repliesCount"] = json!(3);
    assert_eq!(upsert_change(&pool, changed).await, (0, false));
    assert_eq!(stored_raw(&pool, "1").await["favouritesCount"], 1);
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_change_of_the_creation_time_only_is_not_rewritten(pool: PgPool) {
    let mut changed = raw("1");
    changed["createdAt"] = json!("2026-10-01T13:00:00.000Z");
    assert_eq!(upsert_change(&pool, changed).await, (0, false));
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn an_edit_is_rewritten(pool: PgPool) {
    let mut changed = raw("1");
    changed["content"] = json!("<p>edited</p>");
    changed["editedAt"] = json!("2026-10-01T12:30:00.000Z");
    changed["favouritesCount"] = json!(42);
    assert_eq!(upsert_change(&pool, changed).await, (1, true));
    let stored = stored_raw(&pool, "1").await;
    assert_eq!(stored["content"], "<p>edited</p>");
    // A rewrite stores the whole new raw object, counts included.
    assert_eq!(stored["favouritesCount"], 42);
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn each_rendered_raw_path_triggers_a_rewrite(pool: PgPool) {
    type Change = fn(&mut Value);
    let changes: [(&str, Change); 3] = [
        ("avatar", |raw| {
            raw["account"]["avatar"] = json!("https://files.example/new.png");
        }),
        ("avatarStatic", |raw| {
            raw["account"]["avatarStatic"] = json!("https://files.example/new-static.png");
        }),
        ("mediaAttachments", |raw| {
            raw["mediaAttachments"] = json!([]);
        }),
    ];
    for (index, (name, change)) in changes.into_iter().enumerate() {
        sqlx::query("TRUNCATE mastodon_statuses")
            .execute(&pool)
            .await
            .unwrap();
        let mut changed = raw("1");
        change(&mut changed);
        assert_eq!(
            upsert_change(&pool, changed).await,
            (1, true),
            "{index}: {name}"
        );
    }
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn each_typed_column_triggers_a_rewrite(pool: PgPool) {
    let changes: [(&str, Value); 6] = [
        ("url", json!("https://home.example/@a/other")),
        ("spoilerText", json!("cw")),
        ("account.id", json!("10")),
        ("account.acct", json!("b")),
        ("account.displayName", json!("B")),
        ("account.url", json!("https://home.example/@b")),
    ];
    for (path, value) in changes {
        sqlx::query("TRUNCATE mastodon_statuses")
            .execute(&pool)
            .await
            .unwrap();
        let mut changed = raw("1");
        match path.split_once('.') {
            Some((parent, key)) => changed[parent][key] = value,
            None => changed[path] = value,
        }
        assert_eq!(upsert_change(&pool, changed).await, (1, true), "{path}");
    }
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn a_failed_statement_writes_nothing(pool: PgPool) {
    let store = Store::new(pool.clone());
    let mut invalid = raw("2");
    // PostgreSQL text cannot hold a NUL character.
    invalid["content"] = json!("bad\u{0}content");
    let result = store
        .upsert_statuses(&[record(raw("1")), record(invalid)])
        .await;
    assert_eq!(result, Err(StoreError::Query));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM mastodon_statuses")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn the_newest_id_follows_the_creation_time(pool: PgPool) {
    let store = Store::new(pool.clone());
    assert_eq!(store.newest_status_id().await.unwrap(), None);
    let mut older = raw("9");
    older["createdAt"] = json!("2026-09-01T00:00:00.000Z");
    store
        .upsert_statuses(&[record(raw("1")), record(older)])
        .await
        .unwrap();
    assert_eq!(
        store.newest_status_id().await.unwrap().unwrap().as_str(),
        "1"
    );
}

/// Live parity found this: the default float parser of `serde_json` can miss the
/// nearest double by one unit in the last place. JavaScript (and PostgreSQL)
/// parse the same text exactly, so the stored `mediaAttachments` differed,
/// and the guard rewrote rows that the TypeScript service had written.
#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn floats_in_media_metadata_round_trip_exactly(pool: PgPool) {
    let text = r#"{
        "id": "1", "createdAt": "2026-10-01T12:00:00.000Z", "url": "https://home.example/@a/1",
        "content": "<p>x</p>", "account": {"id": "9", "acct": "a", "avatar": "https://files.example/a.png"},
        "mediaAttachments": [{"type": "image", "url": "https://files.example/1.jpg",
            "meta": {"original": {"aspect": 0.36035866877676836},
                     "focus": {"x": -1.8600795271598733, "y": -1.2706591418579039}}}]
    }"#;
    // The TypeScript service stored the same values; PostgreSQL parses the
    // text exactly, like JavaScript does.
    sqlx::query(
        "INSERT INTO mastodon_statuses (id, created_at, url, content, account_id, account_username,
                                        account_display_name, account_url, raw)
         VALUES ('1', '2026-10-01T12:00:00Z', 'https://home.example/@a/1', '<p>x</p>', '9', 'a', '',
                 '', $1::jsonb)",
    )
    .bind(text)
    .execute(&pool)
    .await
    .unwrap();

    let parsed: Value = serde_json::from_str(text).unwrap();
    let written = Store::new(pool.clone())
        .upsert_statuses(&[record(parsed)])
        .await
        .unwrap();
    assert_eq!(
        written, 0,
        "the guard saw a change in identical media metadata"
    );
}

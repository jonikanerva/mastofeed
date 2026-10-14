//! Migration paths: an empty database, a database from the Drizzle
//! migrations of the TypeScript service, and a repeated run.

use mastofeed_server::store::MIGRATOR;
use serde_json::Value;
use sqlx::PgPool;

/// The columns, defaults, and indexes of `mastodon_statuses`.
async fn schema(pool: &PgPool) -> Value {
    sqlx::query_scalar(
        r"
        SELECT jsonb_build_object(
            'columns', (
                SELECT jsonb_agg(jsonb_build_array(column_name, data_type, is_nullable, column_default)
                                 ORDER BY column_name)
                FROM information_schema.columns
                WHERE table_schema = 'public' AND table_name = 'mastodon_statuses'
            ),
            'indexes', (
                SELECT jsonb_agg(indexdef ORDER BY indexname)
                FROM pg_indexes
                WHERE schemaname = 'public' AND tablename = 'mastodon_statuses'
            )
        )
        ",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The two Drizzle migrations of the TypeScript service, unchanged.
async fn apply_drizzle_migrations(pool: &PgPool) {
    for sql in [
        include_str!("../fixtures/drizzle/0000_init.sql"),
        include_str!("../fixtures/drizzle/0001_add_edited_at.sql"),
    ] {
        sqlx::raw_sql(sql).execute(pool).await.unwrap();
    }
}

#[sqlx::test(migrations = false)]
async fn the_baseline_on_a_drizzle_database_changes_nothing(pool: PgPool) {
    apply_drizzle_migrations(&pool).await;
    sqlx::query(
        "INSERT INTO mastodon_statuses (id, created_at, url, content, account_id, account_username,
                                        account_display_name, account_url, raw)
         VALUES ('1', now(), 'u', 'c', 'a', 'b', 'd', 'e', '{\"k\": 1}')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let before = schema(&pool).await;

    MIGRATOR.run(&pool).await.unwrap();

    assert_eq!(schema(&pool).await, before);
    let raw: Value = sqlx::query_scalar("SELECT raw FROM mastodon_statuses WHERE id = '1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(raw, serde_json::json!({"k": 1}));
}

#[sqlx::test(migrations = false)]
async fn the_baseline_on_an_empty_database_creates_the_drizzle_schema(pool: PgPool) {
    MIGRATOR.run(&pool).await.unwrap();
    let from_baseline = schema(&pool).await;

    sqlx::raw_sql("DROP TABLE mastodon_statuses")
        .execute(&pool)
        .await
        .unwrap();
    apply_drizzle_migrations(&pool).await;

    assert_eq!(schema(&pool).await, from_baseline);
}

#[sqlx::test(migrations = false)]
async fn a_repeated_run_applies_nothing(pool: PgPool) {
    MIGRATOR.run(&pool).await.unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(applied, 1);
}

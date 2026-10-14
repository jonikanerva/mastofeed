//! PostgreSQL access: migrations, the status upsert, and the feed query.

use std::str::FromStr;
use std::time::Duration;

use jiff_sqlx::ToSqlx;
use mastofeed_domain::feed::FeedRow;
use mastofeed_domain::status::{StatusId, StatusRecord};
use sqlx::PgPool;
use sqlx::migrate::Migrator;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

use crate::config::Secret;

/// The SQL migrations in `migrations/` at the repository root.
pub static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

/// Limits of the database work.
#[derive(Debug, Clone, Copy)]
pub struct PoolSettings {
    pub max_connections: u32,
    pub acquire_timeout: Duration,
    /// PostgreSQL `statement_timeout` of each connection.
    pub statement_timeout: Duration,
}

impl Default for PoolSettings {
    fn default() -> Self {
        Self {
            max_connections: 4,
            acquire_timeout: Duration::from_secs(5),
            statement_timeout: Duration::from_secs(15),
        }
    }
}

/// A failed database operation. The variants carry no SQL, value, or
/// connection detail, so they are safe to log.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("the database URL is invalid")]
    InvalidUrl,
    #[error("the database migration failed")]
    Migration,
    #[error("the database query failed")]
    Query,
    #[error("a stored row is invalid")]
    InvalidRow,
}

impl StoreError {
    /// A fixed code for diagnostics.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidUrl => "invalid_database_url",
            Self::Migration => "migration",
            Self::Query => "query",
            Self::InvalidRow => "invalid_row",
        }
    }
}

/// Creates a lazy pool. No connection opens before the first query.
///
/// # Errors
///
/// Returns [`StoreError::InvalidUrl`] when the URL cannot be parsed.
pub fn connect_lazy(database_url: &Secret, settings: PoolSettings) -> Result<PgPool, StoreError> {
    let statement_timeout = format!("{}ms", settings.statement_timeout.as_millis());
    let options = PgConnectOptions::from_str(database_url.expose())
        .map_err(|_| StoreError::InvalidUrl)?
        .options([("statement_timeout", statement_timeout.as_str())]);
    Ok(PgPoolOptions::new()
        .max_connections(settings.max_connections)
        .acquire_timeout(settings.acquire_timeout)
        .connect_lazy_with(options))
}

/// Applies the pending migrations.
///
/// # Errors
///
/// Returns [`StoreError::Migration`] when a migration fails.
pub async fn migrate(pool: &PgPool) -> Result<(), StoreError> {
    MIGRATOR.run(pool).await.map_err(|_| StoreError::Migration)
}

/// The status table.
#[derive(Debug, Clone)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The id of the status with the newest `created_at`, if any.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the query fails or the id is empty.
    pub async fn newest_status_id(&self) -> Result<Option<StatusId>, StoreError> {
        let id = sqlx::query_scalar!(
            "SELECT id FROM mastodon_statuses ORDER BY created_at DESC LIMIT 1"
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| StoreError::Query)?;
        id.map(|id| StatusId::new(id).ok_or(StoreError::InvalidRow))
            .transpose()
    }

    /// Inserts new statuses and updates changed ones in one transaction.
    /// Returns the number of rows written.
    ///
    /// A row is rewritten only when a value that the feed renders has
    /// changed. Every poll sends the same recent statuses again; without the
    /// guard, each poll would rewrite each of them (a new row version, a new
    /// TOAST entry for `raw`, an index update, and a WAL record).
    ///
    /// The guard compares the typed columns, not the whole `raw` object,
    /// because `raw` also holds reply, boost, and favourite counts. Nothing
    /// reads them, and they change whenever someone interacts with a status.
    /// The guard also compares the three `raw` paths that the feed renders:
    /// the media attachments and the two avatar URLs. Mastodon avatar URLs
    /// are content-addressed, so a frozen old URL would return 404.
    ///
    /// `created_at` is not compared: a status keeps its creation time, and a
    /// missing upstream value becomes the poll time, which differs each run.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Query`] when a statement fails. Nothing is
    /// written then.
    pub async fn upsert_statuses(&self, records: &[StatusRecord]) -> Result<u64, StoreError> {
        let mut transaction = self.pool.begin().await.map_err(|_| StoreError::Query)?;
        let mut written = 0;
        for record in records {
            let result = sqlx::query!(
                r#"
                INSERT INTO mastodon_statuses (
                    id, created_at, edited_at, url, content, spoiler_text, account_id,
                    account_username, account_display_name, account_url, raw
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                ON CONFLICT (id) DO UPDATE SET
                    created_at = excluded.created_at,
                    edited_at = excluded.edited_at,
                    url = excluded.url,
                    content = excluded.content,
                    spoiler_text = excluded.spoiler_text,
                    account_id = excluded.account_id,
                    account_username = excluded.account_username,
                    account_display_name = excluded.account_display_name,
                    account_url = excluded.account_url,
                    raw = excluded.raw
                WHERE (
                    mastodon_statuses.edited_at,
                    mastodon_statuses.url,
                    mastodon_statuses.content,
                    mastodon_statuses.spoiler_text,
                    mastodon_statuses.account_id,
                    mastodon_statuses.account_username,
                    mastodon_statuses.account_display_name,
                    mastodon_statuses.account_url,
                    mastodon_statuses.raw -> 'mediaAttachments',
                    mastodon_statuses.raw -> 'account' -> 'avatar',
                    mastodon_statuses.raw -> 'account' -> 'avatarStatic'
                ) IS DISTINCT FROM (
                    excluded.edited_at,
                    excluded.url,
                    excluded.content,
                    excluded.spoiler_text,
                    excluded.account_id,
                    excluded.account_username,
                    excluded.account_display_name,
                    excluded.account_url,
                    excluded.raw -> 'mediaAttachments',
                    excluded.raw -> 'account' -> 'avatar',
                    excluded.raw -> 'account' -> 'avatarStatic'
                )
                "#,
                record.id.as_str(),
                record.created_at.to_sqlx(),
                record.edited_at.map(ToSqlx::to_sqlx),
                record.url,
                record.content,
                record.spoiler_text,
                record.account_id,
                record.account_username,
                record.account_display_name,
                record.account_url,
                record.raw,
            )
            .execute(&mut *transaction)
            .await
            .map_err(|_| StoreError::Query)?;
            written += result.rows_affected();
        }
        transaction.commit().await.map_err(|_| StoreError::Query)?;
        Ok(written)
    }

    /// The newest `limit` rows by `created_at`, newest first.
    ///
    /// # Errors
    ///
    /// Returns a [`StoreError`] when the query fails or a row has an empty id.
    pub async fn newest_rows(&self, limit: u16) -> Result<Vec<FeedRow>, StoreError> {
        let rows = sqlx::query!(
            r#"
            SELECT id, created_at, edited_at, url, content, account_username,
                   account_display_name, account_url, raw
            FROM mastodon_statuses
            ORDER BY created_at DESC
            LIMIT $1
            "#,
            i64::from(limit),
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|_| StoreError::Query)?;

        rows.into_iter()
            .map(|row| {
                Ok(FeedRow {
                    id: StatusId::new(row.id).ok_or(StoreError::InvalidRow)?,
                    created_at: row.created_at.to_jiff(),
                    edited_at: row.edited_at.map(jiff_sqlx::Timestamp::to_jiff),
                    url: row.url,
                    content: row.content,
                    account_username: row.account_username,
                    account_display_name: row.account_display_name,
                    account_url: row.account_url,
                    raw: row.raw,
                })
            })
            .collect()
    }
}

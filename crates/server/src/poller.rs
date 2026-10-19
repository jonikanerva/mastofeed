//! The timeline poller: one sync now, then one sync at each cron time.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use croner::Cron;
use futures_util::FutureExt as _;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use mastofeed_domain::status::{StatusId, normalize_timeline};
use serde_json::Value;
use tokio::sync::watch;

use crate::clock::Clock;
use crate::mastodon::{MastodonClient, MastodonError, TIMELINE_LIMIT};
use crate::store::{Store, StoreError};

/// The deadline of one complete sync: all requests and the upsert. With the
/// page cap, a sync makes at most `1 + NEWER_PAGE_CAP` requests. Each request
/// has its own 30 s timeout, so a slow server can reach this deadline first.
/// The sync then fails as a whole, and the next sync requests the same pages.
pub const SYNC_DEADLINE: Duration = Duration::from_secs(120);

/// The largest number of pages of statuses newer than the newest stored
/// status in one sync. Five pages hold 200 statuses, the default feed
/// length. A page is usually below 200 KiB.
pub const NEWER_PAGE_CAP: usize = 5;

/// How the requests for the newer statuses ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paging {
    /// The last page was not full, or no stored status exists.
    Complete,
    /// The sync requested `NEWER_PAGE_CAP` full pages. Older statuses in
    /// the gap can be missing.
    PageCap,
    /// The oldest entry of a full page has no string id, so the next page
    /// cannot be requested. Older statuses in the gap can be missing.
    MissingCursor,
}

impl Paging {
    /// A fixed code for diagnostics. `None` for a complete result.
    #[must_use]
    pub fn code(self) -> Option<&'static str> {
        match self {
            Self::Complete => None,
            Self::PageCap => Some("page_cap"),
            Self::MissingCursor => Some("missing_cursor"),
        }
    }
}

/// The result of one successful sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncReport {
    /// Timeline entries in all responses.
    pub received: usize,
    /// Distinct valid statuses sent to the database.
    pub records: usize,
    /// Entries without a usable id, url, or time.
    pub rejected: usize,
    /// Rows that the upsert inserted or changed.
    pub written: u64,
    /// How the requests for the newer statuses ended.
    pub paging: Paging,
}

/// A failed sync. Nothing is written when a sync fails.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SyncError {
    #[error("the Mastodon request failed: {0}")]
    Mastodon(#[from] MastodonError),
    #[error("the database operation failed: {0}")]
    Store(#[from] StoreError),
    #[error("the sync did not finish before its deadline")]
    Deadline,
}

impl SyncError {
    /// A fixed code for diagnostics.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Mastodon(error) => error.code(),
            Self::Store(error) => error.code(),
            Self::Deadline => "deadline",
        }
    }
}

/// Fetches the timeline and stores it.
#[derive(Clone)]
pub struct Poller {
    client: MastodonClient,
    store: Store,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for Poller {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Poller").finish_non_exhaustive()
    }
}

impl Poller {
    #[must_use]
    pub fn new(client: MastodonClient, store: Store, clock: Arc<dyn Clock>) -> Self {
        Self {
            client,
            store,
            clock,
        }
    }

    /// Runs one sync:
    ///
    /// 1. Read the id of the newest stored status.
    /// 2. Request the newest page of the home timeline.
    /// 3. If a stored status exists, also request the statuses newer than it,
    ///    page by page (`newer_entries`).
    /// 4. Normalize all pages in that order, keep the first record of each
    ///    id, and upsert the records.
    ///
    /// The TypeScript service made steps 1 to 4 with one page in step 3.
    /// When at most 39 statuses are newer than the stored one, the requests
    /// and the result are the same as in the TypeScript service.
    ///
    /// # Errors
    ///
    /// Returns a [`SyncError`] when a request or a database operation fails.
    /// A failed request on any page fails the whole sync, so nothing is
    /// written. A partial write would move the newest stored status past
    /// the gap, and the next sync could not request the rest of the gap.
    pub async fn sync_once(&self) -> Result<SyncReport, SyncError> {
        let newest = self.store.newest_status_id().await?;
        let mut entries = self.client.home_timeline(None, None).await?;
        let paging = match &newest {
            Some(newest) => self.newer_entries(newest, &mut entries).await?,
            None => Paging::Complete,
        };
        let received = entries.len();
        if received == 0 {
            return Ok(SyncReport {
                received,
                records: 0,
                rejected: 0,
                written: 0,
                paging,
            });
        }

        let normalized = normalize_timeline(entries, self.clock.now());
        let written = if normalized.records.is_empty() {
            0
        } else {
            self.store.upsert_statuses(&normalized.records).await?
        };
        Ok(SyncReport {
            received,
            records: normalized.records.len(),
            rejected: normalized.rejected.len(),
            written,
            paging,
        })
    }

    /// Appends the statuses newer than `newest` to `entries`, newest first.
    /// The first request has only `since_id`. While a page is full, the next
    /// request also has `max_id`: the id of the oldest entry of that page.
    /// For a boost, that id is the id of the boost, not of the original
    /// status, because the home timeline orders the boosts by their own id.
    ///
    /// A page with fewer than `TIMELINE_LIMIT` entries ends the requests.
    /// This saves one request in each normal sync. It assumes that Mastodon
    /// fills each page when more statuses exist.
    async fn newer_entries(
        &self,
        newest: &StatusId,
        entries: &mut Vec<Value>,
    ) -> Result<Paging, MastodonError> {
        let mut max_id = None;
        for _ in 0..NEWER_PAGE_CAP {
            let page = self
                .client
                .home_timeline(Some(newest), max_id.as_ref())
                .await?;
            let full = page.len() >= usize::from(TIMELINE_LIMIT);
            let cursor = page.last().and_then(entry_id);
            entries.extend(page);
            match (full, cursor) {
                (false, _) => return Ok(Paging::Complete),
                (true, None) => return Ok(Paging::MissingCursor),
                (true, Some(cursor)) => max_id = Some(cursor),
            }
        }
        Ok(Paging::PageCap)
    }

    /// Runs one sync now and then one sync at each schedule time, until
    /// `shutdown` changes to `true`. A sync that runs past a schedule time
    /// skips that time, so syncs never overlap. A failed or panicking sync is
    /// logged, and the next scheduled sync runs normally.
    pub async fn run(self, schedule: Cron, mut shutdown: watch::Receiver<bool>) {
        loop {
            tokio::select! {
                () = self.logged_sync() => {}
                _ = shutdown.wait_for(|stop| *stop) => return,
            }
            let Some(delay) = delay_until_next(&schedule, self.clock.now()) else {
                tracing::error!(
                    code = "schedule",
                    "the cron schedule has no next time; polling stopped"
                );
                return;
            };
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                _ = shutdown.wait_for(|stop| *stop) => return,
            }
        }
    }

    async fn logged_sync(&self) {
        let started = tokio::time::Instant::now();
        let outcome = AssertUnwindSafe(tokio::time::timeout(SYNC_DEADLINE, self.sync_once()))
            .catch_unwind()
            .await;
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match outcome {
            Ok(Ok(Ok(report))) => {
                if let Some(code) = report.paging.code() {
                    tracing::warn!(
                        code,
                        "timeline paging stopped before the newest stored status; new statuses can be missing"
                    );
                }
                tracing::info!(
                    received = report.received,
                    records = report.records,
                    rejected = report.rejected,
                    written = report.written,
                    elapsed_ms,
                    "timeline sync finished"
                );
            }
            Ok(Ok(Err(error))) => {
                tracing::warn!(code = error.code(), elapsed_ms, "timeline sync failed");
            }
            Ok(Err(_)) => tracing::warn!(
                code = SyncError::Deadline.code(),
                elapsed_ms,
                "timeline sync failed"
            ),
            Err(_) => tracing::error!(code = "panic", elapsed_ms, "timeline sync panicked"),
        }
    }
}

/// The id of one timeline entry as Mastodon sent it, before a boost is
/// unwrapped.
fn entry_id(entry: &Value) -> Option<StatusId> {
    entry
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| StatusId::new(id.to_owned()))
}

/// The time from `now` until the next schedule time in UTC. `None` when the
/// schedule has no future time.
#[must_use]
pub fn delay_until_next(schedule: &Cron, now: Timestamp) -> Option<Duration> {
    let now = now.to_zoned(TimeZone::UTC);
    let next = schedule.find_next_occurrence(&now, false).ok()?;
    let delay = now.timestamp().duration_until(next.timestamp());
    Duration::try_from(delay).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delay(schedule: &str, now: &str) -> Option<Duration> {
        delay_until_next(&schedule.parse().unwrap(), now.parse().unwrap())
    }

    #[test]
    fn waits_until_the_next_five_minute_mark() {
        assert_eq!(
            delay("*/5 * * * *", "2026-10-14T10:02:30Z"),
            Some(Duration::from_secs(150))
        );
        assert_eq!(
            delay("*/5 * * * *", "2026-10-14T10:04:59.750Z"),
            Some(Duration::from_millis(250))
        );
    }

    #[test]
    fn exactly_on_a_mark_waits_for_the_following_mark() {
        assert_eq!(
            delay("*/5 * * * *", "2026-10-14T10:05:00Z"),
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn the_schedule_is_in_utc() {
        assert_eq!(
            delay("0 12 * * *", "2026-10-14T11:00:00Z"),
            Some(Duration::from_secs(3600))
        );
    }

    #[test]
    fn accepts_an_optional_seconds_field() {
        assert_eq!(
            delay("30 */5 * * * *", "2026-10-14T10:00:00Z"),
            Some(Duration::from_secs(30))
        );
    }
}

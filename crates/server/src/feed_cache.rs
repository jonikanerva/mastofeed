//! The feed document that `/feed.json` serves from memory.
//!
//! The poller is the only writer of the status table, and the service runs
//! one replica. The poller therefore renders the feed after each sync, and
//! the HTTP handler serves that document without a database query. On Neon,
//! a query starts the compute for at least 5 minutes, so a feed request
//! must not need one (docs/adr/0006).

use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use mastofeed_domain::feed::{FeedSettings, render_feed};
use tokio::sync::watch;

use crate::store::{Store, StoreError};

/// The deadline of one feed query and its rendering.
const LOAD_DEADLINE: Duration = Duration::from_secs(20);

/// One rendered feed document and its validator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedFeed {
    body: Bytes,
    etag: String,
}

impl RenderedFeed {
    fn new(body: String) -> Self {
        let etag = weak_etag(&body);
        Self {
            body: Bytes::from(body),
            etag,
        }
    }

    /// The JSON Feed document. The clone shares the bytes.
    #[must_use]
    pub fn body(&self) -> Bytes {
        self.body.clone()
    }

    /// The weak validator of the document.
    #[must_use]
    pub fn etag(&self) -> &str {
        &self.etag
    }
}

/// A failed feed load. The variants carry no SQL, value, or status content,
/// so they are safe to log.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FeedError {
    #[error("the feed query failed: {0}")]
    Store(#[from] StoreError),
    #[error("the feed rendering failed")]
    Render,
    #[error("the feed load did not finish before its deadline")]
    Deadline,
}

impl FeedError {
    /// A fixed code for diagnostics.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Store(error) => error.code(),
            Self::Render => "render",
            Self::Deadline => "deadline",
        }
    }
}

/// The feed in memory, and the way to load it from the database.
///
/// Clones share one document. `refresh` replaces the document as a whole, so
/// a reader sees the old document or the new one, never a mix.
#[derive(Debug, Clone)]
pub struct FeedCache {
    store: Store,
    settings: Arc<FeedSettings>,
    limit: u16,
    current: watch::Sender<Option<Arc<RenderedFeed>>>,
}

impl FeedCache {
    /// An empty cache. `refresh` fills it.
    #[must_use]
    pub fn new(store: Store, settings: Arc<FeedSettings>, limit: u16) -> Self {
        let (current, _) = watch::channel(None);
        Self {
            store,
            settings,
            limit,
            current,
        }
    }

    /// The document of the last successful `refresh`. `None` before the
    /// first one.
    #[must_use]
    pub fn cached(&self) -> Option<Arc<RenderedFeed>> {
        self.current.borrow().clone()
    }

    /// Reads the newest statuses and renders the feed. It does not change
    /// the cache.
    ///
    /// The HTTP handler uses this only while the cache is empty. It must not
    /// store the result: a load that started before a sync could then
    /// replace the newer document of that sync.
    ///
    /// # Errors
    ///
    /// Returns a [`FeedError`] when the query or the rendering fails, or
    /// when both together pass the deadline.
    pub async fn load(&self) -> Result<RenderedFeed, FeedError> {
        let rows = tokio::time::timeout(LOAD_DEADLINE, self.store.newest_rows(self.limit))
            .await
            .map_err(|_| FeedError::Deadline)??;
        let body = render_feed(&self.settings, &rows).map_err(|_| FeedError::Render)?;
        Ok(RenderedFeed::new(body))
    }

    /// Loads the feed and replaces the cached document. Only the poller
    /// calls this, after each sync.
    ///
    /// # Errors
    ///
    /// Returns the [`FeedError`] of the load. The cache then keeps the
    /// previous document.
    pub async fn refresh(&self) -> Result<(), FeedError> {
        let feed = self.load().await?;
        self.current.send_replace(Some(Arc::new(feed)));
        Ok(())
    }
}

/// A weak validator from the body length and a 64-bit hash. Feed readers
/// send it back in `If-None-Match` and receive `304` while the feed is
/// unchanged. The value is stable within one build of the service.
fn weak_etag(body: &str) -> String {
    let mut hasher = DefaultHasher::new();
    body.hash(&mut hasher);
    format!("W/\"{:x}-{:016x}\"", body.len(), hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_etag_depends_on_the_body() {
        assert_eq!(weak_etag("a"), weak_etag("a"));
        assert_ne!(weak_etag("a"), weak_etag("b"));
        assert!(weak_etag("abc").starts_with("W/\"3-"));
    }

    #[test]
    fn a_rendered_feed_has_the_validator_of_its_body() {
        let feed = RenderedFeed::new("body".to_owned());
        assert_eq!(feed.body(), Bytes::from_static(b"body"));
        assert_eq!(feed.etag(), weak_etag("body"));
    }

    #[test]
    fn each_failure_has_a_fixed_code() {
        assert_eq!(FeedError::Store(StoreError::Query).code(), "query");
        assert_eq!(FeedError::Render.code(), "render");
        assert_eq!(FeedError::Deadline.code(), "deadline");
    }
}

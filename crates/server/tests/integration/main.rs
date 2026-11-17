//! Integration tests. They need PostgreSQL: `DATABASE_URL` must point to an
//! isolated server where the test user can create databases. `mise run
//! test` starts a new test server for each run (STACK.md §3). Each
//! `#[sqlx::test]` gets its own fresh database.

#![expect(
    clippy::unwrap_used,
    reason = "the helpers of this test binary fail a check by panicking"
)]

mod common;
mod feed_cache;
mod http;
mod lifecycle;
mod mastodon;
mod memory;
mod migrations;
mod paging;
mod parity;
mod upsert;

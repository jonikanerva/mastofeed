//! The Mastofeed service: a Mastodon home-timeline poller and a JSON Feed
//! server that share one PostgreSQL table.
//!
//! `main.rs` wires these modules. The modules are public so that the
//! integration tests in `tests/` can use them.

pub mod clock;
pub mod config;
pub mod http;
pub mod lifecycle;
pub mod mastodon;
pub mod observability;
pub mod poller;
pub mod store;

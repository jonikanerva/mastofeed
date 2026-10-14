//! Pure rules of Mastofeed: status normalization and JSON Feed rendering.
//!
//! This crate does no I/O. It reads no clock, environment, or file. The
//! server crate passes the current time and the settings in explicitly.
//!
//! The rules reproduce the TypeScript service of commit d17168d. The stored
//! `raw` value and the feed output stay compatible in both directions, so a
//! revert to that service needs no data change (docs/adr/0001).

pub mod feed;
pub mod html;
mod js;
pub mod status;

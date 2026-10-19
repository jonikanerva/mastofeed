//! Rebuilds the crate when the migrations change.
//!
//! `sqlx::migrate!` in `src/store.rs` embeds the migrations at compile time.
//! On stable Rust, the macro cannot tell Cargo to watch the directory, so a
//! new migration file alone does not rebuild the crate. The documentation of
//! `sqlx::migrate!` recommends this build script. Cargo scans the whole
//! directory for changes.

fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}

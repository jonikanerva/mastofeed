//! Process entry point.
//!
//! `mastofeed migrate` applies the database migrations and exits.
//! `mastofeed serve` (the default) serves HTTP and polls Mastodon.

use std::process::ExitCode;
use std::time::Duration;

use mastofeed_server::config::Config;
use mastofeed_server::{lifecycle, observability};

/// The exit code of a usage or configuration error.
const EXIT_CONFIG: u8 = 2;

/// The time for blocking tasks after the main future ends.
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(5);

enum Command {
    Serve,
    Migrate,
}

fn main() -> ExitCode {
    let command = match std::env::args().nth(1).as_deref() {
        None | Some("serve") => Command::Serve,
        Some("migrate") => Command::Migrate,
        Some(_) => {
            report("usage: mastofeed [serve|migrate]");
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            // The text names settings, never their values.
            report(&format!("mastofeed: {error}"));
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    if observability::init(config.log_format).is_err() {
        report("mastofeed: logging could not be set up");
        return ExitCode::FAILURE;
    }
    observability::install_panic_hook();

    let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    else {
        tracing::error!(stage = "runtime", "the async runtime could not start");
        return ExitCode::FAILURE;
    };
    let result = match command {
        Command::Serve => runtime.block_on(lifecycle::serve(config)),
        Command::Migrate => runtime.block_on(lifecycle::migrate(&config)),
    };
    // Dropping a runtime waits for its blocking tasks without a limit.
    runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(stage) => {
            tracing::error!(stage, "the service stopped with an error");
            ExitCode::FAILURE
        }
    }
}

/// Writes a message to standard error before logging is set up.
#[expect(clippy::print_stderr, reason = "logging is not set up yet")]
fn report(message: &str) {
    eprintln!("{message}");
}

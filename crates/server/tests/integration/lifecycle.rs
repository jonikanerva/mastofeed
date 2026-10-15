//! The process: configuration errors, the migrate command, and a clean
//! stop on SIGTERM.

use std::io::{BufRead as _, BufReader};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sqlx::PgPool;
use sqlx::postgres::PgConnectOptions;

const BINARY: &str = env!("CARGO_BIN_EXE_mastofeed");

/// A database URL for `pool`'s test database.
fn database_url(pool: &PgPool) -> String {
    let options: std::sync::Arc<PgConnectOptions> = pool.connect_options();
    let host = options.get_host();
    let port = options.get_port();
    let user = options.get_username();
    let database = options.get_database().unwrap();
    // The test server trusts local connections, so the URL has no password.
    format!("postgres://{user}@{host}:{port}/{database}")
}

/// A port that was free a moment ago. The service rejects port 0, like the
/// TypeScript service did.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn command(database_url: &str) -> Command {
    let mut command = Command::new(BINARY);
    command
        .env_clear()
        .env("DATABASE_URL", database_url)
        // A closed port: the poller fails fast and keeps running.
        .env("MASTODON_BASE_URL", "http://127.0.0.1:9/")
        .env("MASTODON_ACCESS_TOKEN", "lifecycle-secret-token")
        .env("HOME_PAGE_URL", "https://feed.example")
        .env("PORT", free_port().to_string())
        .env(
            "PUBLIC_DIR",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../public"),
        );
    command
}

#[test]
fn an_invalid_setting_stops_with_exit_code_2_and_names_it() {
    let output = command("postgres://user:secret-password@127.0.0.1:1/db")
        .env("FEED_LIMIT", "1001")
        .env_remove("MASTODON_ACCESS_TOKEN")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr.trim(),
        "mastofeed: invalid environment variables: MASTODON_ACCESS_TOKEN, FEED_LIMIT"
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
}

#[test]
fn an_unknown_command_stops_with_exit_code_2() {
    let output = command("postgres://127.0.0.1:1/db")
        .arg("deploy")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[sqlx::test(migrations = false)]
async fn the_migrate_command_applies_the_migrations(pool: PgPool) {
    let url = database_url(&pool);
    let output =
        tokio::task::spawn_blocking(move || command(&url).arg("migrate").output().unwrap())
            .await
            .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(applied, 1);
    let log = String::from_utf8(output.stdout).unwrap();
    assert!(log.contains("migrations are up to date"));
    // The test server offers TLS, and the default `sslmode=prefer` uses it.
    assert!(log.contains("database connection uses TLS"), "{log}");
    assert!(!log.contains("postgres://"));
}

/// Runs `migrate` with `sslmode` appended to the URL and returns the log.
async fn migrate_with_sslmode(pool: &PgPool, sslmode: &str) -> String {
    let url = format!("{}?sslmode={sslmode}", database_url(pool));
    let output =
        tokio::task::spawn_blocking(move || command(&url).arg("migrate").output().unwrap())
            .await
            .unwrap();
    let log = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{log}");
    log
}

// Without a SQLx TLS feature, `sslmode=require` fails at the connection.
#[sqlx::test(migrations = false)]
async fn the_migrate_command_connects_with_sslmode_require(pool: PgPool) {
    let log = migrate_with_sslmode(&pool, "require").await;
    assert!(log.contains("database connection uses TLS"), "{log}");
}

#[sqlx::test(migrations = false)]
async fn the_migrate_command_reports_a_connection_without_tls(pool: PgPool) {
    let log = migrate_with_sslmode(&pool, "disable").await;
    assert!(
        log.contains("database connection does not use TLS"),
        "{log}"
    );
    assert!(!log.contains("database connection uses TLS"), "{log}");
}

#[test]
fn the_migrate_command_fails_without_a_database() {
    let output = command("postgres://user:secret-password@127.0.0.1:9/db")
        .arg("migrate")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let log = String::from_utf8(output.stdout).unwrap();
    assert!(log.contains(r#""stage":"migrate""#), "{log}");
    assert!(!log.contains("secret-password"));
}

#[sqlx::test(migrator = "mastofeed_server::store::MIGRATOR")]
async fn sigterm_stops_the_service_cleanly(pool: PgPool) {
    let url = database_url(&pool);
    let lines = tokio::task::spawn_blocking(move || {
        let mut child = command(&url)
            .arg("serve")
            .env("RAILWAY_GIT_COMMIT_SHA", "lifecycle-test")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let mut lines = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        // Wait until the service has started and the first sync has ended.
        while Instant::now() < deadline {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                break;
            }
            let done = line.contains("timeline sync");
            lines.push(line);
            if done {
                break;
            }
        }
        let status = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
        let started = Instant::now();
        let exit = child.wait().unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        for line in reader.lines() {
            lines.push(line.unwrap());
        }
        assert_eq!(exit.code(), Some(0), "{lines:?}");
        lines
    })
    .await
    .unwrap();

    let log = lines.join("\n");
    assert!(log.contains(r#""message":"service started""#), "{log}");
    assert!(log.contains(r#""version":"lifecycle-test""#), "{log}");
    assert!(log.contains(r#""code":"connect""#), "{log}");
    assert!(log.contains(r#""message":"service stopped""#), "{log}");
    assert!(!log.contains("lifecycle-secret-token"));
    for line in &lines {
        serde_json::from_str::<serde_json::Value>(line).unwrap();
    }
}

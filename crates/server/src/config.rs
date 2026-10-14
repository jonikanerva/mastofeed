//! Startup configuration from environment variables.
//!
//! The service validates every setting once, before it opens the database
//! or the port. An error names the setting and never prints its value.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use croner::Cron;
use mastofeed_domain::feed::FeedSettings;
use reqwest::Url;

const DEFAULT_PORT: u16 = 3000;
const DEFAULT_CRON_SCHEDULE: &str = "*/5 * * * *";
const DEFAULT_FEED_LIMIT: u16 = 200;
const MAX_FEED_LIMIT: u16 = 1000;
const DEFAULT_FEED_TITLE: &str = "Mastodon timeline";
const DEFAULT_PUBLIC_DIR: &str = "public";

/// A secret value. `Debug` and `Display` never show it.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([redacted])")
    }
}

/// The log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// One JSON object per line (production).
    Json,
    /// Readable text (local development).
    Text,
}

/// The validated service configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub database_url: Secret,
    pub mastodon_base_url: Url,
    pub mastodon_access_token: Secret,
    pub cron_schedule: Cron,
    pub feed_limit: u16,
    pub feed: FeedSettings,
    pub public_dir: PathBuf,
    pub log_format: LogFormat,
    /// The deployed commit, when Railway provides it.
    pub version: Option<String>,
}

/// One or more settings are missing or invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// The names of the invalid settings, in a fixed order.
    pub invalid: Vec<&'static str>,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid environment variables: {}",
            self.invalid.join(", ")
        )
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Reads the configuration from the process environment.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] with the names of all invalid settings.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| {
            std::env::var_os(name).map(|value| value.into_string().map_err(|_| ()))
        })
    }

    /// Reads the configuration through `lookup`. A value that is not valid
    /// Unicode is `Some(Err(()))`.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] with the names of all invalid settings.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<Result<String, ()>>,
    ) -> Result<Self, ConfigError> {
        let mut reader = Reader {
            lookup,
            invalid: Vec::new(),
        };
        let port = reader.read("PORT", |v| parse_number(v, DEFAULT_PORT, 1, u16::MAX));
        let database_url = reader.read("DATABASE_URL", required);
        let mastodon_base_url = reader.read("MASTODON_BASE_URL", |v| parse_http_url(&required(v)?));
        let mastodon_access_token = reader.read("MASTODON_ACCESS_TOKEN", required);
        let home_page_url = reader.read("HOME_PAGE_URL", |v| {
            required(v).filter(|v| Url::parse(v).is_ok())
        });
        let cron_schedule = reader.read("CRON_SCHEDULE", |v| {
            with_default(v, DEFAULT_CRON_SCHEDULE)?.parse::<Cron>().ok()
        });
        let feed_limit = reader.read("FEED_LIMIT", |v| {
            parse_number(v, DEFAULT_FEED_LIMIT, 1, MAX_FEED_LIMIT)
        });
        let feed_title = reader.read("FEED_TITLE", |v| with_default(v, DEFAULT_FEED_TITLE));
        let feed_description = reader.read("FEED_DESCRIPTION", |v| match v {
            None => Some(None),
            Some(text) if text.is_empty() => None,
            Some(text) => Some(Some(text)),
        });
        let public_dir = reader.read("PUBLIC_DIR", |v| with_default(v, DEFAULT_PUBLIC_DIR));
        let log_format = reader.read("LOG_FORMAT", |v| match v.as_deref() {
            None | Some("json") => Some(LogFormat::Json),
            Some("text") => Some(LogFormat::Text),
            Some(_) => None,
        });
        let version = reader.read("RAILWAY_GIT_COMMIT_SHA", |v| {
            Some(v.filter(|v| !v.is_empty()))
        });

        let (
            Some(port),
            Some(database_url),
            Some((mastodon_base_url, mastodon_base_text)),
            Some(mastodon_access_token),
            Some(home_page_url),
            Some(cron_schedule),
            Some(feed_limit),
            Some(feed_title),
            Some(feed_description),
            Some(public_dir),
            Some(log_format),
            Some(version),
        ) = (
            port,
            database_url,
            mastodon_base_url,
            mastodon_access_token,
            home_page_url,
            cron_schedule,
            feed_limit,
            feed_title,
            feed_description,
            public_dir,
            log_format,
            version,
        )
        else {
            return Err(ConfigError {
                invalid: reader.invalid,
            });
        };

        Ok(Self {
            bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)),
            database_url: Secret::new(database_url),
            mastodon_base_url,
            mastodon_access_token: Secret::new(mastodon_access_token),
            cron_schedule,
            feed_limit,
            feed: FeedSettings {
                title: feed_title,
                description: feed_description,
                home_page_url,
                mastodon_base_url: mastodon_base_text,
            },
            public_dir: PathBuf::from(public_dir),
            log_format,
            version,
        })
    }
}

/// Reads settings and collects the names of the invalid ones in read order.
struct Reader<L> {
    lookup: L,
    invalid: Vec<&'static str>,
}

impl<L: Fn(&str) -> Option<Result<String, ()>>> Reader<L> {
    /// Reads one setting. `parse` gets `None` when the variable is not set
    /// and returns `None` when the value is invalid.
    fn read<T>(
        &mut self,
        name: &'static str,
        parse: impl FnOnce(Option<String>) -> Option<T>,
    ) -> Option<T> {
        let parsed = match (self.lookup)(name) {
            None => parse(None),
            Some(Ok(value)) => parse(Some(value)),
            Some(Err(())) => None,
        };
        if parsed.is_none() {
            self.invalid.push(name);
        }
        parsed
    }
}

/// A required non-empty value.
fn required(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty())
}

/// A missing value takes the default. A present value must not be empty.
fn with_default(value: Option<String>, default: &str) -> Option<String> {
    match value {
        None => Some(default.to_owned()),
        Some(text) if text.is_empty() => None,
        Some(text) => Some(text),
    }
}

/// A decimal integer in `min..=max`. Surrounding whitespace is allowed.
fn parse_number(value: Option<String>, default: u16, min: u16, max: u16) -> Option<u16> {
    let Some(text) = value else {
        return Some(default);
    };
    let text = text.trim();
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u16>().ok().filter(|n| (min..=max).contains(n))
}

/// An `http` or `https` URL. Returns the parsed URL and the original text;
/// the feed builds item links from the original text.
fn parse_http_url(value: &str) -> Option<(Url, String)> {
    let url = Url::parse(value).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| (url, value.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn valid() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            (
                "DATABASE_URL",
                "postgres://user:secret-password@db.internal:5432/app",
            ),
            ("MASTODON_BASE_URL", "https://mastodon.example/"),
            ("MASTODON_ACCESS_TOKEN", "secret-token"),
            ("HOME_PAGE_URL", "https://feed.example"),
        ])
    }

    fn load(vars: &HashMap<&'static str, &'static str>) -> Result<Config, ConfigError> {
        Config::from_lookup(|name| vars.get(name).map(|v| Ok((*v).to_owned())))
    }

    #[test]
    fn applies_the_defaults() {
        let config = load(&valid()).unwrap();
        assert_eq!(config.bind, "0.0.0.0:3000".parse().unwrap());
        assert_eq!(config.feed_limit, 200);
        assert_eq!(config.feed.title, "Mastodon timeline");
        assert_eq!(config.feed.description, None);
        assert_eq!(config.feed.mastodon_base_url, "https://mastodon.example/");
        assert_eq!(config.cron_schedule.as_str(), "*/5 * * * *");
        assert_eq!(config.public_dir, PathBuf::from("public"));
        assert_eq!(config.log_format, LogFormat::Json);
        assert_eq!(config.version, None);
    }

    #[test]
    fn reads_every_setting() {
        let mut vars = valid();
        vars.extend([
            ("PORT", "8080"),
            ("CRON_SCHEDULE", "0 */10 * * * *"),
            ("FEED_LIMIT", " 1000 "),
            ("FEED_TITLE", "Title"),
            ("FEED_DESCRIPTION", "Description"),
            ("PUBLIC_DIR", "/app/public"),
            ("LOG_FORMAT", "text"),
            ("RAILWAY_GIT_COMMIT_SHA", "abc123"),
        ]);
        let config = load(&vars).unwrap();
        assert_eq!(config.bind.port(), 8080);
        assert_eq!(config.feed_limit, 1000);
        assert_eq!(config.feed.description.as_deref(), Some("Description"));
        assert_eq!(config.log_format, LogFormat::Text);
        assert_eq!(config.version.as_deref(), Some("abc123"));
    }

    #[test]
    fn the_production_schedule_is_valid() {
        let mut vars = valid();
        vars.insert("CRON_SCHEDULE", "*/5 * * * *");
        assert!(load(&vars).is_ok());
    }

    #[test]
    fn names_every_invalid_setting_in_a_fixed_order() {
        let mut vars = valid();
        vars.remove("DATABASE_URL");
        vars.extend([
            ("PORT", "0"),
            ("MASTODON_BASE_URL", "ftp://mastodon.example"),
            ("HOME_PAGE_URL", "not a url"),
            ("CRON_SCHEDULE", "every five minutes"),
            ("FEED_LIMIT", "1001"),
            ("FEED_TITLE", ""),
            ("FEED_DESCRIPTION", ""),
            ("LOG_FORMAT", "xml"),
        ]);
        let error = load(&vars).unwrap_err();
        assert_eq!(
            error.invalid,
            [
                "PORT",
                "DATABASE_URL",
                "MASTODON_BASE_URL",
                "HOME_PAGE_URL",
                "CRON_SCHEDULE",
                "FEED_LIMIT",
                "FEED_TITLE",
                "FEED_DESCRIPTION",
                "LOG_FORMAT"
            ]
        );
    }

    #[test]
    fn rejects_non_decimal_numbers() {
        for value in ["", "1e2", "0x10", "-1", "2.5"] {
            let mut vars = valid();
            vars.insert("FEED_LIMIT", value);
            assert_eq!(
                load(&vars).unwrap_err().invalid,
                ["FEED_LIMIT"],
                "{value:?}"
            );
        }
    }

    #[test]
    fn a_non_unicode_value_is_invalid() {
        let vars = valid();
        let error = Config::from_lookup(|name| {
            if name == "MASTODON_ACCESS_TOKEN" {
                Some(Err(()))
            } else {
                vars.get(name).map(|v| Ok((*v).to_owned()))
            }
        })
        .unwrap_err();
        assert_eq!(error.invalid, ["MASTODON_ACCESS_TOKEN"]);
    }

    #[test]
    fn diagnostics_never_show_secret_values() {
        let config = load(&valid()).unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("secret-token"));
        assert!(!debug.contains("secret-password"));

        let mut vars = valid();
        vars.insert("MASTODON_ACCESS_TOKEN", "");
        let error = load(&vars).unwrap_err().to_string();
        assert_eq!(
            error,
            "invalid environment variables: MASTODON_ACCESS_TOKEN"
        );
    }
}

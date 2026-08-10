//! Application configuration loaded from the process environment.
//!
//! Anubis applications are configured twelve-factor style: plain environment
//! variables with sensible development defaults, so a freshly stamped app boots
//! with zero configuration. [`AppConfig::from_env`] reads the process
//! environment; [`AppConfig::from_lookup`] accepts any lookup function, which
//! keeps configuration fully testable without mutating global state.
//!
//! Recognized variables:
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `ANUBIS_ENV` | `development` | One of `development`, `test`, `production` |
//! | `HOST` | `127.0.0.1` | IP address the server binds to |
//! | `PORT` | `3000` | TCP port the server binds to |
//! | `DATABASE_URL` | unset | Postgres connection URL, e.g. `postgres://user:pass@host/db` |
//! | `APP_URL` | `http://<host>:<port>` | Public base URL used in email links |
//! | `ANUBIS_SECRET_KEY` | development key | Base64 for exactly 32 bytes; encrypts recoverable secrets at rest. Required in production |
//!
//! Each OpenID Connect provider in [`crate::auth::oauth::known_providers`]
//! adds three more, named after the provider:
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `GOOGLE_OAUTH_CLIENT_ID` | unset | OAuth client id; setting it and the secret enables the provider |
//! | `GOOGLE_OAUTH_CLIENT_SECRET` | unset | OAuth client secret |
//! | `GOOGLE_OAUTH_ISSUER` | `https://accounts.google.com` | Issuer whose discovery document configures the flow |
//!
//! The set grows milestone by milestone.
//!
//! # The secret key
//!
//! `ANUBIS_SECRET_KEY` keys [`crate::auth::secret_box`], which seals secrets
//! the application has to read back (today, TOTP seeds). Generate one with
//! `openssl rand -base64 32`, or any source of 32 random bytes rendered as
//! base64; padding is optional. Production fails to start without it.
//! Development and test fall back to a fixed, public development key so a
//! freshly stamped application boots with no configuration, and
//! [`crate::telemetry::init`] warns whenever that fallback is in use.
//! Rotating the key makes values sealed under the old one unreadable, so
//! affected users re-enroll their second factor.
//!
//! # OAuth providers
//!
//! A provider is enabled by setting both its client id and its client secret;
//! setting one without the other is a misconfiguration and refuses to start,
//! because a half-configured provider is a sign-in button that always fails.
//! The issuer variable overrides the registry's issuer, which is what a
//! self-hosted or single-tenant deployment needs. Register the redirect URI
//! `<APP_URL>/auth/oauth/<provider>/callback` with the provider, and add the
//! button with `anubis scaffold oauth <provider>`.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use crate::auth::oauth::{OauthProviderConfig, known_providers};
use crate::auth::secret_box::SecretKey;

/// Selects the runtime environment.
const ENV_VAR: &str = "ANUBIS_ENV";

/// IP address the server binds to.
const HOST_VAR: &str = "HOST";

/// TCP port the server binds to.
const PORT_VAR: &str = "PORT";

/// Postgres connection URL.
const DATABASE_URL_VAR: &str = "DATABASE_URL";

/// Public base URL of the application, used when building email links.
const APP_URL_VAR: &str = "APP_URL";

/// Base64 key that encrypts recoverable secrets at rest.
const SECRET_KEY_VAR: &str = "ANUBIS_SECRET_KEY";

/// What a valid `ANUBIS_SECRET_KEY` looks like, quoted back in errors.
const SECRET_KEY_FORM: &str = "base64 for exactly 32 random bytes, e.g. `openssl rand -base64 32`";

/// Loopback keeps development servers off the network unless opted in.
const DEFAULT_HOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Matches the port the starter frontend proxies to during development.
const DEFAULT_PORT: u16 = 3000;

/// The runtime environment an application is running in.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Environment {
    /// Local development, the default when `ANUBIS_ENV` is unset.
    #[default]
    Development,
    /// Automated test runs.
    Test,
    /// Live deployments.
    Production,
}

impl Environment {
    /// Returns `true` in local development.
    #[must_use]
    pub fn is_development(self) -> bool {
        self == Self::Development
    }

    /// Returns `true` in automated test runs.
    #[must_use]
    pub fn is_test(self) -> bool {
        self == Self::Test
    }

    /// Returns `true` in live deployments.
    #[must_use]
    pub fn is_production(self) -> bool {
        self == Self::Production
    }
}

impl Display for Environment {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Development => "development",
            Self::Test => "test",
            Self::Production => "production",
        };
        f.write_str(name)
    }
}

/// Where the application server binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerConfig {
    /// IP address the server binds to.
    pub host: IpAddr,
    /// TCP port the server binds to.
    pub port: u16,
}

impl ServerConfig {
    /// Returns the address the server should bind to.
    #[must_use]
    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }
}

/// Postgres connection settings.
///
/// The URL embeds credentials, so this type never exposes it through `Debug`
/// or `Display`; read it deliberately with [`DatabaseConfig::url`].
#[derive(Clone, PartialEq, Eq)]
pub struct DatabaseConfig {
    url: String,
}

impl DatabaseConfig {
    /// Returns the Postgres connection URL, credentials included.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl fmt::Debug for DatabaseConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("DatabaseConfig(...)")
    }
}

/// Top-level application configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    /// The runtime environment.
    pub environment: Environment,
    /// Where the application server binds.
    pub server: ServerConfig,
    /// Postgres connection settings, when `DATABASE_URL` is set.
    pub database: Option<DatabaseConfig>,
    /// Public base URL of the application, without a trailing slash.
    ///
    /// Used when building links in outgoing email. Defaults to the bind
    /// address, which is right for development and must be set explicitly in
    /// production.
    pub app_url: String,
    /// Key that encrypts recoverable secrets at rest, from `ANUBIS_SECRET_KEY`.
    ///
    /// Production requires it. Development and test fall back to the built-in
    /// development key; see the module docs.
    pub secret_key: SecretKey,
    /// The OpenID Connect providers the environment enabled, in registry order.
    ///
    /// Empty unless a provider's client id and secret are both set.
    pub oauth: Vec<OauthProviderConfig>,
}

impl AppConfig {
    /// Loads configuration from the process environment.
    ///
    /// # Errors
    /// Returns an [`Error`] when a recognized variable is set to a value that
    /// does not parse.
    pub fn from_env() -> Result<Self, Error> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Loads configuration from an arbitrary variable lookup.
    ///
    /// The lookup receives a variable name and returns its value, if set.
    /// Unset variables fall back to their documented defaults. This is the
    /// seam that keeps configuration testable: pass a closure over a map
    /// instead of touching the process environment.
    ///
    /// # Errors
    /// Returns an [`Error`] when a recognized variable is set to a value that
    /// does not parse.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, Error> {
        let environment = match lookup(ENV_VAR) {
            None => Environment::default(),
            Some(value) => parse_environment(&value).ok_or_else(|| {
                Error::invalid(ENV_VAR, &value, "one of development, test, production")
            })?,
        };

        let host = match lookup(HOST_VAR) {
            None => DEFAULT_HOST,
            Some(value) => value
                .parse()
                .map_err(|_error| Error::invalid(HOST_VAR, &value, "an IP address"))?,
        };

        let port = match lookup(PORT_VAR) {
            None => DEFAULT_PORT,
            Some(value) => value
                .parse()
                .map_err(|_error| Error::invalid(PORT_VAR, &value, "a TCP port number"))?,
        };

        let database = match lookup(DATABASE_URL_VAR) {
            None => None,
            Some(url) if url.trim().is_empty() => {
                return Err(Error::invalid(
                    DATABASE_URL_VAR,
                    &url,
                    "a non-empty Postgres connection URL",
                ));
            }
            Some(url) => Some(DatabaseConfig { url }),
        };

        let app_url = match lookup(APP_URL_VAR) {
            None => format!("http://{host}:{port}"),
            Some(url) => {
                let trimmed = url.trim().trim_end_matches('/');
                if trimmed.is_empty() || !trimmed.starts_with("http") {
                    return Err(Error::invalid(APP_URL_VAR, &url, "an http(s) base URL"));
                }
                trimmed.to_owned()
            }
        };

        let secret_key = match lookup(SECRET_KEY_VAR) {
            Some(encoded) => SecretKey::from_base64(&encoded)
                .map_err(|_error| Error::invalid_secret(SECRET_KEY_VAR, SECRET_KEY_FORM))?,
            // Live deployments store real secrets, so the key is mandatory
            // there; anywhere else the public development key keeps a fresh
            // checkout running with no configuration.
            None if environment.is_production() => {
                return Err(Error::missing(SECRET_KEY_VAR, SECRET_KEY_FORM));
            }
            None => SecretKey::development(),
        };

        let oauth = oauth_providers(&lookup)?;

        Ok(Self {
            environment,
            server: ServerConfig { host, port },
            database,
            app_url,
            secret_key,
            oauth,
        })
    }
}

/// Resolves every provider whose credentials the environment carries.
///
/// A provider with neither credential is simply not enabled. A provider with
/// one of the two is a misconfiguration: the button would be rendered and
/// every click would fail, so startup stops and names the missing variable.
fn oauth_providers(
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<Vec<OauthProviderConfig>, Error> {
    let mut configured = Vec::new();

    for provider in known_providers() {
        let client_id = non_empty(lookup(provider.client_id_var));
        let client_secret = non_empty(lookup(provider.client_secret_var));

        let (client_id, client_secret) = match (client_id, client_secret) {
            (None, None) => continue,
            (Some(client_id), Some(client_secret)) => (client_id, client_secret),
            (Some(_client_id), None) => {
                return Err(Error::missing(
                    provider.client_secret_var,
                    "the client secret that goes with the client id",
                ));
            }
            (None, Some(_client_secret)) => {
                return Err(Error::missing(
                    provider.client_id_var,
                    "the client id that goes with the client secret",
                ));
            }
        };

        let issuer = non_empty(lookup(provider.issuer_var));
        let config = OauthProviderConfig::new(provider, client_id, client_secret, issuer.clone())
            .map_err(|_error| {
            Error::invalid(
                provider.issuer_var,
                issuer.as_deref().unwrap_or(provider.issuer),
                "an OpenID Connect issuer: an http(s) URL with no query string or fragment",
            )
        })?;
        configured.push(config);
    }

    Ok(configured)
}

/// Treats a variable set to whitespace as unset, the way a `.env` line reads.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

fn parse_environment(value: &str) -> Option<Environment> {
    match value.to_ascii_lowercase().as_str() {
        "development" | "dev" => Some(Environment::Development),
        "test" => Some(Environment::Test),
        "production" | "prod" => Some(Environment::Production),
        _ => None,
    }
}

/// A configuration variable is missing, or set to a value that does not parse.
#[derive(Debug)]
pub struct Error {
    variable: &'static str,
    message: String,
    backtrace: Backtrace,
}

impl Error {
    /// The variable holds a value that does not parse.
    ///
    /// The value is quoted back to make the fix obvious, so never use this for
    /// a variable holding a secret; use [`Error::invalid_secret`] instead.
    fn invalid(variable: &'static str, value: &str, expected: &'static str) -> Self {
        Self::new(
            variable,
            format!("invalid value for {variable}: expected {expected}, got {value:?}"),
        )
    }

    /// The variable holds a value that does not parse and must not be logged.
    fn invalid_secret(variable: &'static str, expected: &'static str) -> Self {
        Self::new(
            variable,
            format!("invalid value for {variable}: expected {expected}"),
        )
    }

    /// The variable is required in this environment but is not set.
    fn missing(variable: &'static str, expected: &'static str) -> Self {
        Self::new(
            variable,
            format!("{variable} is not set: expected {expected}"),
        )
    }

    fn new(variable: &'static str, message: String) -> Self {
        Self {
            variable,
            message,
            backtrace: Backtrace::capture(),
        }
    }

    /// Returns the name of the offending environment variable.
    #[must_use]
    pub fn variable(&self) -> &str {
        self.variable
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};

    use super::{AppConfig, Environment};
    use crate::auth::secret_box::SecretKey;

    /// A syntactically valid `ANUBIS_SECRET_KEY`, for tests that need one.
    const SAMPLE_SECRET_KEY: &str = "bkVLZLd1zHBqxWvKGKp5gRTZKcTf9UvHT5vXbHvWJ0M=";

    fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let config = AppConfig::from_lookup(|_name| None).expect("defaults must parse");

        assert_eq!(config.environment, Environment::Development);
        assert_eq!(config.server.host, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(config.server.port, 3000);
    }

    #[test]
    fn variables_override_defaults() {
        let lookup = lookup_from(&[
            ("ANUBIS_ENV", "production"),
            ("HOST", "0.0.0.0"),
            ("PORT", "8080"),
            ("ANUBIS_SECRET_KEY", SAMPLE_SECRET_KEY),
        ]);

        let config = AppConfig::from_lookup(lookup).expect("valid variables must parse");

        assert_eq!(config.environment, Environment::Production);
        assert_eq!(config.server.host, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.server.socket_addr().to_string(), "0.0.0.0:8080");
    }

    #[test]
    fn environment_names_parse_case_insensitively_with_short_forms() {
        for (value, expected) in [
            ("DEVELOPMENT", Environment::Development),
            ("dev", Environment::Development),
            ("Test", Environment::Test),
            ("PROD", Environment::Production),
        ] {
            // Production insists on a key; every environment accepts one.
            let lookup = lookup_from(&[
                ("ANUBIS_ENV", value),
                ("ANUBIS_SECRET_KEY", SAMPLE_SECRET_KEY),
            ]);
            let config = AppConfig::from_lookup(lookup).expect("known names must parse");
            assert_eq!(config.environment, expected, "for input {value:?}");
        }
    }

    #[test]
    fn invalid_port_is_rejected_with_context() {
        let lookup = lookup_from(&[("PORT", "not-a-port")]);

        let error = AppConfig::from_lookup(lookup).expect_err("junk ports must be rejected");

        assert_eq!(error.variable(), "PORT");
        let rendered = error.to_string();
        assert!(rendered.contains("PORT"), "got: {rendered}");
        assert!(rendered.contains("not-a-port"), "got: {rendered}");
    }

    #[test]
    fn invalid_environment_is_rejected_with_context() {
        let lookup = lookup_from(&[("ANUBIS_ENV", "staging")]);

        let error = AppConfig::from_lookup(lookup).expect_err("unknown environments are rejected");

        assert_eq!(error.variable(), "ANUBIS_ENV");
    }

    #[test]
    fn app_url_defaults_to_the_bind_address_and_trims_trailing_slashes() {
        let defaulted = AppConfig::from_lookup(|_name| None).expect("defaults must parse");
        assert_eq!(defaulted.app_url, "http://127.0.0.1:3000");

        let lookup = lookup_from(&[("APP_URL", "https://app.example.com/")]);
        let explicit = AppConfig::from_lookup(lookup).expect("a URL is fine");
        assert_eq!(explicit.app_url, "https://app.example.com");

        let lookup = lookup_from(&[("APP_URL", "not-a-url")]);
        let error = AppConfig::from_lookup(lookup).expect_err("junk URLs are rejected");
        assert_eq!(error.variable(), "APP_URL");
    }

    #[test]
    fn database_url_is_optional_but_must_be_non_empty() {
        let unset = AppConfig::from_lookup(|_name| None).expect("unset is fine");
        assert!(unset.database.is_none());

        let lookup = lookup_from(&[("DATABASE_URL", "postgres://app:hunter2@localhost/app")]);
        let set = AppConfig::from_lookup(lookup).expect("a URL is fine");
        let database = set.database.expect("database config must be present");
        assert_eq!(database.url(), "postgres://app:hunter2@localhost/app");

        let lookup = lookup_from(&[("DATABASE_URL", "   ")]);
        let error = AppConfig::from_lookup(lookup).expect_err("blank URLs are rejected");
        assert_eq!(error.variable(), "DATABASE_URL");
    }

    #[test]
    fn database_debug_output_never_leaks_credentials() {
        let lookup = lookup_from(&[("DATABASE_URL", "postgres://app:hunter2@localhost/app")]);
        let config = AppConfig::from_lookup(lookup).expect("a URL is fine");

        let rendered = format!("{config:?}");
        assert!(rendered.contains("DatabaseConfig"), "got: {rendered}");
        assert!(!rendered.contains("hunter2"), "got: {rendered}");
    }

    #[test]
    fn the_secret_key_falls_back_to_the_development_key_outside_production() {
        for environment in ["development", "test"] {
            let lookup = lookup_from(&[("ANUBIS_ENV", environment)]);
            let config = AppConfig::from_lookup(lookup).expect("no key is fine here");
            assert!(
                config.secret_key.is_development(),
                "for environment {environment}"
            );
        }
    }

    #[test]
    fn production_refuses_to_start_without_a_secret_key() {
        let lookup = lookup_from(&[("ANUBIS_ENV", "production")]);

        let error = AppConfig::from_lookup(lookup).expect_err("production requires a key");

        assert_eq!(error.variable(), "ANUBIS_SECRET_KEY");
        let rendered = error.to_string();
        assert!(rendered.contains("is not set"), "got: {rendered}");
        assert!(rendered.contains("32 random bytes"), "got: {rendered}");
    }

    #[test]
    fn a_configured_secret_key_is_used_verbatim() {
        let encoded = SecretKey::generate().to_base64();
        let lookup = lookup_from(&[
            ("ANUBIS_ENV", "production"),
            ("ANUBIS_SECRET_KEY", &encoded),
        ]);

        let config = AppConfig::from_lookup(lookup).expect("a valid key must parse");

        assert!(!config.secret_key.is_development());
        assert_eq!(config.secret_key.to_base64(), encoded);
    }

    #[test]
    fn a_malformed_secret_key_is_rejected_without_echoing_it() {
        let lookup = lookup_from(&[("ANUBIS_SECRET_KEY", "c2hvcnQ=")]);

        let error = AppConfig::from_lookup(lookup).expect_err("short keys are rejected");

        assert_eq!(error.variable(), "ANUBIS_SECRET_KEY");
        let rendered = error.to_string();
        assert!(!rendered.contains("c2hvcnQ"), "got: {rendered}");
    }

    #[test]
    fn environment_predicates_match_variants() {
        assert!(Environment::Development.is_development());
        assert!(Environment::Test.is_test());
        assert!(Environment::Production.is_production());
        assert!(!Environment::Production.is_development());
    }
}

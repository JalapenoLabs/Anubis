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
//!
//! The set grows milestone by milestone (the database URL arrives with M2).

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Selects the runtime environment.
const ENV_VAR: &str = "ANUBIS_ENV";

/// IP address the server binds to.
const HOST_VAR: &str = "HOST";

/// TCP port the server binds to.
const PORT_VAR: &str = "PORT";

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

/// Top-level application configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    /// The runtime environment.
    pub environment: Environment,
    /// Where the application server binds.
    pub server: ServerConfig,
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
                Error::invalid(ENV_VAR, value, "one of development, test, production")
            })?,
        };

        let host = match lookup(HOST_VAR) {
            None => DEFAULT_HOST,
            Some(value) => value
                .parse()
                .map_err(|_error| Error::invalid(HOST_VAR, value, "an IP address"))?,
        };

        let port = match lookup(PORT_VAR) {
            None => DEFAULT_PORT,
            Some(value) => value
                .parse()
                .map_err(|_error| Error::invalid(PORT_VAR, value, "a TCP port number"))?,
        };

        Ok(Self {
            environment,
            server: ServerConfig { host, port },
        })
    }
}

fn parse_environment(value: &str) -> Option<Environment> {
    match value.to_ascii_lowercase().as_str() {
        "development" | "dev" => Some(Environment::Development),
        "test" => Some(Environment::Test),
        "production" | "prod" => Some(Environment::Production),
        _ => None,
    }
}

/// A configuration variable was set to a value that does not parse.
#[derive(Debug)]
pub struct Error {
    variable: &'static str,
    value: String,
    expected: &'static str,
    backtrace: Backtrace,
}

impl Error {
    fn invalid(variable: &'static str, value: String, expected: &'static str) -> Self {
        Self {
            variable,
            value,
            expected,
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
        write!(
            f,
            "invalid value for {}: expected {}, got {:?}",
            self.variable, self.expected, self.value
        )?;
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
            let lookup = lookup_from(&[("ANUBIS_ENV", value)]);
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
    fn environment_predicates_match_variants() {
        assert!(Environment::Development.is_development());
        assert!(Environment::Test.is_test());
        assert!(Environment::Production.is_production());
        assert!(!Environment::Production.is_development());
    }
}

//! Structured logging and tracing for Anubis applications.
//!
//! [`init`] installs the global [`tracing`] subscriber every Anubis application
//! logs through. Events are structured (named properties over formatted
//! strings), and filtering follows standard `tracing` conventions: `RUST_LOG`
//! wins when set, otherwise the environment picks a default (`debug` in
//! development, `info` everywhere else).
//!
//! Call [`init`] once, first thing in `main`. A second call returns an
//! [`Error`], since the global subscriber can only be installed once per
//! process.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};

use tracing_subscriber::EnvFilter;

use crate::config::AppConfig;

/// Installs the global tracing subscriber for the application.
///
/// # Errors
/// Returns an [`Error`] when a global subscriber is already installed.
pub fn init(config: &AppConfig) -> Result<(), Error> {
    let default_directive = if config.environment.is_development() {
        "debug"
    } else {
        "info"
    };

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_error| EnvFilter::new(default_directive));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .try_init()
        .map_err(Error::from_source)
}

/// The global tracing subscriber could not be installed.
#[derive(Debug)]
pub struct Error {
    source: Box<dyn std::error::Error + Send + Sync>,
    backtrace: Backtrace,
}

impl Error {
    fn from_source(source: Box<dyn std::error::Error + Send + Sync>) -> Self {
        Self {
            source,
            backtrace: Backtrace::capture(),
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to install the global tracing subscriber (is one already installed?): {}",
            self.source
        )?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::init;
    use crate::config::AppConfig;

    #[test]
    fn init_installs_once_and_rejects_a_second_subscriber() {
        let config = AppConfig::from_lookup(|_name| None).expect("defaults must parse");

        init(&config).expect("first initialization must succeed");

        let error = init(&config).expect_err("second initialization must fail");
        let rendered = error.to_string();
        assert!(rendered.contains("tracing subscriber"), "got: {rendered}");
    }
}

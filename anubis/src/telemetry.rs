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
//! process. Because it is the first thing with both a live subscriber and the
//! configuration in hand, [`init`] also announces configuration that is fine
//! locally and costly in a live deployment: the built-in `ANUBIS_SECRET_KEY`
//! fallback, and a production deployment with no frontend, no mail relay, or
//! no Stripe account.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};

use tracing_subscriber::EnvFilter;

use crate::config::AppConfig;

/// Installs the global tracing subscriber for the application.
///
/// Also warns about risky defaults that are in effect.
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
        .map_err(Error::from_source)?;

    warn_about_risky_defaults(config);
    Ok(())
}

/// Announces defaults that are fine locally and costly anywhere else.
fn warn_about_risky_defaults(config: &AppConfig) {
    if config.secret_key.is_development() {
        tracing::warn!(
            app.environment = %config.environment,
            "ANUBIS_SECRET_KEY is unset: secrets at rest are encrypted with the built-in \
             development key, which is public. Set ANUBIS_SECRET_KEY before storing anything \
             real. (environment: {{app.environment}})",
        );
    }

    // An API-only deployment is a legitimate shape, so this is a warning: the
    // frontend may well live behind a CDN. It is still the likeliest reason a
    // fresh production deploy answers every page with a 404.
    if config.spa_dir.is_none() && config.environment.is_production() {
        tracing::warn!(
            app.environment = %config.environment,
            "SPA_DIR is unset: this binary serves the API only and no frontend. Point it at the \
             built frontend (for example SPA_DIR=frontend/dist) unless the SPA is hosted \
             elsewhere. (environment: {{app.environment}})",
        );
    }

    // An application that does not charge for anything is a legitimate shape,
    // so this is a warning too. It is still the likeliest reason a production
    // deployment answers every checkout with a 503.
    if config.stripe.is_none() && config.environment.is_production() {
        tracing::warn!(
            app.environment = %config.environment,
            "STRIPE_SECRET_KEY is unset: billing is disabled, every organization is on the free \
             plan, and checkout and the customer portal answer 503. (environment: \
             {{app.environment}})",
        );
    }

    // Billing that takes money without hearing what happened to it is worse
    // than billing that is switched off: a customer pays, no event is believed,
    // and the application keeps showing the free plan.
    if config
        .stripe
        .as_ref()
        .is_some_and(|stripe| stripe.webhook_secret().is_none())
        && config.environment.is_production()
    {
        tracing::warn!(
            app.environment = %config.environment,
            "STRIPE_WEBHOOK_SECRET is unset while billing is enabled: the receiver refuses \
             every event, so a completed checkout will charge the customer and leave this \
             application on the free plan. (environment: {{app.environment}})",
        );
    }

    // Deliberately a warning rather than a hard failure: a first deploy that
    // sends no email should not be blocked on mail configuration.
    if config.smtp.is_none() && config.environment.is_production() {
        tracing::warn!(
            app.environment = %config.environment,
            "SMTP_URL is unset: email delivery is disabled and every message is written to the \
             log instead. Verification, password reset, sign-in code, and invitation email will \
             not reach anyone. (environment: {{app.environment}})",
        );
    }
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

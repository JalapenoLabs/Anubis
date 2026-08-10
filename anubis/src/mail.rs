//! Outgoing email for Anubis applications.
//!
//! [`Mailer`] is the one service the framework and applications send email
//! through. Two backends exist today:
//!
//! - [`Mailer::log`]: writes each email to structured logs, action links
//!   included. The development default; nothing leaves the machine.
//! - [`Mailer::test`]: captures emails in an in-memory outbox whose handle
//!   your tests read, in the spirit of Rails' `ActionMailer::Base.deliveries`.
//!
//! An SMTP backend is on the roadmap (tracked in the repository issues); the
//! [`Mailer::send`] signature is async and fallible so transports can slot in
//! without touching call sites.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};
use std::sync::{Arc, Mutex};

/// A plain-text email ready for delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Email {
    /// Recipient address.
    pub to: String,
    /// Subject line.
    pub subject: String,
    /// Plain-text body. HTML templating arrives with a later milestone.
    pub text_body: String,
}

/// Sends email through the configured backend. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Mailer {
    backend: Backend,
}

#[derive(Debug, Clone)]
enum Backend {
    Log,
    Test(TestOutbox),
}

impl Mailer {
    /// A mailer that writes each email to structured logs. Development default.
    #[must_use]
    pub fn log() -> Self {
        Self {
            backend: Backend::Log,
        }
    }

    /// A mailer that captures emails in an in-memory outbox for assertions.
    #[must_use]
    pub fn test() -> (Self, TestOutbox) {
        let outbox = TestOutbox::default();
        let mailer = Self {
            backend: Backend::Test(outbox.clone()),
        };
        (mailer, outbox)
    }

    /// Delivers an email through the backend.
    ///
    /// # Errors
    /// The log and test backends never fail; the `Result` exists so transport
    /// backends (SMTP) can report delivery failures without an API change.
    #[expect(
        clippy::unused_async,
        reason = "the API is async so an SMTP backend can slot in without breaking call sites"
    )]
    pub async fn send(&self, email: Email) -> Result<(), Error> {
        match &self.backend {
            Backend::Log => {
                tracing::info!(
                    email.to = %email.to,
                    email.subject = %email.subject,
                    email.body = %email.text_body,
                    "email delivered to log backend for {{email.to}}: {{email.subject}}",
                );
                Ok(())
            }
            Backend::Test(outbox) => {
                outbox.push(email);
                Ok(())
            }
        }
    }
}

/// Handle to the emails a [`Mailer::test`] mailer has sent.
#[derive(Debug, Clone, Default)]
pub struct TestOutbox {
    inner: Arc<Mutex<Vec<Email>>>,
}

impl TestOutbox {
    /// Returns a snapshot of every email sent so far, oldest first.
    ///
    /// # Panics
    /// Panics if a previous holder of the outbox lock panicked; a poisoned
    /// outbox means the test is already failing.
    #[must_use]
    pub fn emails(&self) -> Vec<Email> {
        self.inner.lock().expect("outbox lock poisoned").clone()
    }

    fn push(&self, email: Email) {
        self.inner.lock().expect("outbox lock poisoned").push(email);
    }
}

/// An email delivery failure.
#[derive(Debug)]
pub struct Error {
    context: &'static str,
    backtrace: Backtrace,
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(self.context)?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::{Email, Mailer};

    fn sample_email() -> Email {
        Email {
            to: "someone@example.com".to_owned(),
            subject: "Hello".to_owned(),
            text_body: "A body with a link: https://example.com/x?token=abc".to_owned(),
        }
    }

    #[tokio::test]
    async fn the_test_outbox_captures_sent_email_in_order() {
        let (mailer, outbox) = Mailer::test();

        mailer
            .send(sample_email())
            .await
            .expect("send must succeed");
        let mut second = sample_email();
        second.subject = "Second".to_owned();
        mailer.send(second).await.expect("send must succeed");

        let emails = outbox.emails();
        assert_eq!(emails.len(), 2);
        assert_eq!(emails[0].subject, "Hello");
        assert_eq!(emails[1].subject, "Second");
    }

    #[tokio::test]
    async fn the_log_mailer_delivers_without_error() {
        let mailer = Mailer::log();
        mailer
            .send(sample_email())
            .await
            .expect("send must succeed");
    }
}

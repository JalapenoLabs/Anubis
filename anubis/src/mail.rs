//! Outgoing email for Anubis applications.
//!
//! [`Mailer`] is the one service the framework and applications send email
//! through. Three backends exist:
//!
//! - [`Mailer::log`]: writes each email to structured logs, action links
//!   included. The development default; nothing leaves the machine.
//! - [`Mailer::test`]: captures emails in an in-memory outbox whose handle
//!   your tests read, in the spirit of Rails' `ActionMailer::Base.deliveries`.
//! - [`Mailer::smtp`]: delivers through an SMTP relay over rustls. The
//!   production backend.
//!
//! [`Mailer::from_config`] picks between them: an application delivers over
//! SMTP wherever `SMTP_URL` is set, and writes to the log everywhere else, so
//! a freshly stamped app runs with no configuration. See `docs/email.md`.
//!
//! # The SMTP backend
//!
//! Connections are pooled and opened lazily, so building a mailer performs no
//! I/O and startup never waits on a relay. A wrong host, a refused credential,
//! or a blocked port therefore surfaces at the first [`Mailer::send`], as an
//! [`Error`] the caller already handles. This is deliberate: relays throttle
//! connections, and a startup probe would spend that budget on every deploy
//! and every restart.
//!
//! ```no_run
//! # async fn example() -> Result<(), anubis::mail::Error> {
//! use anubis::mail::{Email, Mailer};
//!
//! let mailer = Mailer::smtp(
//!     "smtps://apikey:secret@smtp.example.com:465",
//!     "Acme <no-reply@acme.com>",
//! )?;
//!
//! mailer
//!     .send(Email {
//!         to: "someone@example.com".to_owned(),
//!         subject: "Welcome".to_owned(),
//!         text_body: "Glad you are here.".to_owned(),
//!     })
//!     .await?;
//! # Ok(())
//! # }
//! ```

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};
use std::sync::{Arc, Mutex};

use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::config::AppConfig;

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
    Smtp(Smtp),
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

    /// A mailer that delivers through an SMTP relay.
    ///
    /// `url` is an `smtp://` or `smtps://` URL carrying the credentials, host,
    /// port, and TLS mode; `from` is the sender address every message carries,
    /// with an optional display name (`Acme <no-reply@acme.com>`). Both come
    /// from `SMTP_URL` and `MAIL_FROM`; see [`crate::config`].
    ///
    /// The relay is contacted lazily, so this call performs no I/O. A relay
    /// that refuses the connection or the credentials surfaces at the first
    /// [`Mailer::send`].
    ///
    /// # Errors
    /// Returns an [`Error`] when the URL or the sender address does not parse.
    ///
    /// # Panics
    /// Panics when called outside a tokio runtime: the connection pool spawns
    /// the task that reaps idle connections. Applications build their mailer
    /// inside `#[tokio::main]`, where a runtime is always present.
    pub fn smtp(url: &str, from: &str) -> Result<Self, Error> {
        let from = from.parse::<Mailbox>().map_err(|source| {
            Error::new("the sender address is not a valid email address", source)
        })?;

        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
            .map_err(|source| Error::new("the SMTP relay URL could not be parsed", source))?
            .build();

        Ok(Self {
            backend: Backend::Smtp(Smtp { transport, from }),
        })
    }

    /// The mailer the configuration asks for.
    ///
    /// SMTP wherever the environment configures a relay, the log backend
    /// everywhere else. Applications build their mailer with this, so the
    /// choice of backend is a deployment decision rather than a code change.
    ///
    /// # Errors
    /// Returns an [`Error`] when the configured relay URL or sender address
    /// does not parse.
    ///
    /// # Panics
    /// Panics when a relay is configured and no tokio runtime is running; see
    /// [`Mailer::smtp`].
    pub fn from_config(config: &AppConfig) -> Result<Self, Error> {
        match &config.smtp {
            Some(smtp) => Self::smtp(smtp.url(), smtp.from()),
            None => Ok(Self::log()),
        }
    }

    /// Delivers an email through the backend.
    ///
    /// # Errors
    /// The log and test backends never fail. The SMTP backend returns an
    /// [`Error`] when the recipient address does not parse, or when the relay
    /// refuses the connection, the credentials, or the message.
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
            Backend::Smtp(smtp) => {
                let message = smtp.message(&email)?;
                smtp.transport
                    .send(message)
                    .await
                    .map_err(|source| Error::new("the SMTP relay refused the message", source))?;

                // Bodies carry action links, so only the envelope is logged.
                tracing::info!(
                    email.to = %email.to,
                    email.subject = %email.subject,
                    "email delivered over SMTP to {{email.to}}: {{email.subject}}",
                );
                Ok(())
            }
        }
    }
}

/// An SMTP relay, and the sender address its messages carry.
#[derive(Clone)]
struct Smtp {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl Smtp {
    /// Renders `email` as an RFC 5322 message from this relay's sender.
    fn message(&self, email: &Email) -> Result<Message, Error> {
        let to = email.to.parse::<Mailbox>().map_err(|source| {
            Error::new("the recipient address is not a valid email address", source)
        })?;

        Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(email.subject.clone())
            .header(ContentType::TEXT_PLAIN)
            .body(email.text_body.clone())
            .map_err(|source| Error::new("the email could not be encoded as a message", source))
    }
}

impl fmt::Debug for Smtp {
    /// The transport holds the relay credentials, so only the sender is shown.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Smtp")
            .field("from", &self.from.to_string())
            .finish_non_exhaustive()
    }
}

/// Returns `true` when `value` parses as an email address.
///
/// A display name is allowed, as in `Acme <no-reply@acme.com>`. This is how
/// [`crate::config`] rejects a malformed `MAIL_FROM` at startup instead of at
/// the first send, without reaching for the mail transport itself.
pub(crate) fn is_valid_address(value: &str) -> bool {
    value.parse::<Mailbox>().is_ok()
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
    source: Box<dyn std::error::Error + Send + Sync>,
    backtrace: Backtrace,
}

impl Error {
    /// Wraps an upstream failure, such as a relay that refused a message.
    ///
    /// `context` says what the framework was doing; it never quotes the relay
    /// URL, which carries credentials.
    fn new(context: &'static str, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            context,
            source: Box::new(source),
            backtrace: Backtrace::capture(),
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.source)?;
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
    use super::{Email, Mailer, Smtp, is_valid_address};
    use crate::config::AppConfig;
    use lettre::message::Mailbox;
    use lettre::{AsyncSmtpTransport, Tokio1Executor};

    fn sample_email() -> Email {
        Email {
            to: "someone@example.com".to_owned(),
            subject: "Hello".to_owned(),
            text_body: "A body with a link: https://example.com/x?token=abc".to_owned(),
        }
    }

    /// An SMTP backend pointed at a port nothing listens on.
    ///
    /// Building it opens no connection, which is exactly what lets message
    /// rendering be tested without a relay. It still needs a runtime, because
    /// the connection pool spawns its reaper task, so every test that builds
    /// one is a `tokio::test`.
    fn offline_smtp() -> Smtp {
        Smtp {
            transport: AsyncSmtpTransport::<Tokio1Executor>::from_url("smtp://127.0.0.1:2525")
                .expect("the URL must parse")
                .build(),
            from: "Acme <no-reply@acme.com>"
                .parse::<Mailbox>()
                .expect("the sender must parse"),
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

    #[tokio::test]
    async fn a_message_carries_the_sender_recipient_subject_and_body() {
        let message = offline_smtp()
            .message(&sample_email())
            .expect("the message must render");
        let rendered = String::from_utf8(message.formatted()).expect("messages are UTF-8");

        assert!(
            rendered.contains("From: Acme <no-reply@acme.com>"),
            "got: {rendered}"
        );
        assert!(
            rendered.contains("To: someone@example.com"),
            "got: {rendered}"
        );
        assert!(rendered.contains("Subject: Hello"), "got: {rendered}");
        assert!(rendered.contains("text/plain"), "got: {rendered}");
        assert!(rendered.contains("token=abc"), "got: {rendered}");
    }

    #[tokio::test]
    async fn a_malformed_recipient_is_rejected_before_the_relay_is_dialed() {
        let mut email = sample_email();
        email.to = "not-an-address".to_owned();

        let error = offline_smtp()
            .message(&email)
            .expect_err("junk recipients must be rejected");

        assert!(error.to_string().contains("recipient"), "got: {error}");
    }

    #[tokio::test]
    async fn smtp_urls_and_sender_addresses_are_validated_up_front() {
        Mailer::smtp("smtps://user:secret@smtp.example.com:465", "a@example.com")
            .expect("implicit TLS URLs are valid");
        Mailer::smtp("smtp://smtp.example.com:587?tls=required", "a@example.com")
            .expect("STARTTLS URLs are valid");

        let error = Mailer::smtp("https://smtp.example.com", "a@example.com")
            .expect_err("only SMTP schemes are relays");
        assert!(error.to_string().contains("URL"), "got: {error}");

        let error = Mailer::smtp("smtp://smtp.example.com", "not-an-address")
            .expect_err("junk senders must be rejected");
        assert!(error.to_string().contains("sender"), "got: {error}");
    }

    #[tokio::test]
    async fn the_smtp_backend_never_debug_prints_the_relay_credentials() {
        let mailer = Mailer::smtp("smtps://user:hunter2@smtp.example.com:465", "a@example.com")
            .expect("the URL must parse");

        let rendered = format!("{mailer:?}");
        assert!(rendered.contains("Smtp"), "got: {rendered}");
        assert!(!rendered.contains("hunter2"), "got: {rendered}");
    }

    #[test]
    fn addresses_are_recognized_with_and_without_a_display_name() {
        assert!(is_valid_address("no-reply@acme.com"));
        assert!(is_valid_address("Acme <no-reply@acme.com>"));
        assert!(!is_valid_address("no-reply"));
        assert!(!is_valid_address(""));
    }

    #[tokio::test]
    async fn from_config_follows_the_environment() {
        let logging = AppConfig::from_lookup(|_name| None).expect("defaults must parse");
        let mailer = Mailer::from_config(&logging).expect("the log backend cannot fail");
        assert!(format!("{mailer:?}").contains("Log"));

        let configured = AppConfig::from_lookup(|name| match name {
            "SMTP_URL" => Some("smtps://user:secret@smtp.example.com:465".to_owned()),
            "MAIL_FROM" => Some("Acme <no-reply@acme.com>".to_owned()),
            _ => None,
        })
        .expect("a configured relay must parse");
        let mailer = Mailer::from_config(&configured).expect("the relay must build");
        assert!(format!("{mailer:?}").contains("Smtp"));
    }
}

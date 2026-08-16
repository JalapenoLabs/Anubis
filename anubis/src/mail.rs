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
//!
//! # DKIM signing
//!
//! [`Mailer::smtp_signed`] signs every message it sends, from a key the
//! deployment holds. It is the exception rather than the rule: relay providers
//! sign for you once the sender domain is verified with them, and that
//! signature is the one the world checks. Signing here is for the relay that
//! does not, such as a company MTA or a sidecar, and signing twice is harmless.
//! `DKIM_PRIVATE_KEY`, `DKIM_SELECTOR`, and `DKIM_DOMAIN` configure it; see
//! [`crate::config`] and `docs/email.md` for the DNS record and rotation.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};
use std::sync::{Arc, Mutex};

use lettre::message::Mailbox;
use lettre::message::dkim::{self, DkimSigningAlgorithm, DkimSigningKey, DkimSigningKeyError};
use lettre::message::header::ContentType;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::config::{AppConfig, DkimConfig};

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
        Self::build_smtp(url, from, None)
    }

    /// A mailer that delivers through an SMTP relay and signs what it sends.
    ///
    /// Same as [`Mailer::smtp`], plus a DKIM signature on every message, from
    /// the key, selector, and domain in `dkim`. Reach for it when the relay
    /// does not sign for you: providers sign once the sender domain is verified
    /// with them, and a second signature is harmless, so most deployments never
    /// need this. See `docs/email.md` for the DNS record the selector needs.
    ///
    /// # Errors
    /// Returns an [`Error`] when the URL or the sender address does not parse,
    /// or when the signing key cannot be read. The key is read once, here,
    /// rather than on every send.
    ///
    /// # Panics
    /// Panics when called outside a tokio runtime; see [`Mailer::smtp`].
    pub fn smtp_signed(url: &str, from: &str, dkim: &DkimConfig) -> Result<Self, Error> {
        Self::build_smtp(url, from, Some(dkim))
    }

    /// Builds the SMTP backend, signing when the deployment configured a key.
    fn build_smtp(url: &str, from: &str, dkim: Option<&DkimConfig>) -> Result<Self, Error> {
        let from = from.parse::<Mailbox>().map_err(|source| {
            Error::new("the sender address is not a valid email address", source)
        })?;

        let transport = AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
            .map_err(|source| Error::new("the SMTP relay URL could not be parsed", source))?
            .build();

        let dkim = dkim.map(signer).transpose()?.map(Arc::new);

        Ok(Self {
            backend: Backend::Smtp(Smtp {
                transport,
                from,
                dkim,
            }),
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
            Some(smtp) => Self::build_smtp(smtp.url(), smtp.from(), smtp.dkim()),
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

/// An SMTP relay, the sender address its messages carry, and how they are signed.
#[derive(Clone)]
struct Smtp {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    /// The signer, when the deployment signs its own mail.
    ///
    /// Shared rather than owned because a parsed signing key is not `Clone`,
    /// and [`Mailer`] is: every clone signs with the one key read at startup.
    dkim: Option<Arc<dkim::DkimConfig>>,
}

impl Smtp {
    /// Renders `email` as an RFC 5322 message from this relay's sender.
    fn message(&self, email: &Email) -> Result<Message, Error> {
        let to = email.to.parse::<Mailbox>().map_err(|source| {
            Error::new("the recipient address is not a valid email address", source)
        })?;

        let mut message = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(email.subject.clone())
            .header(ContentType::TEXT_PLAIN)
            .body(email.text_body.clone())
            .map_err(|source| Error::new("the email could not be encoded as a message", source))?;

        if let Some(dkim) = &self.dkim {
            // Signed last, so the signature covers the headers the builder just
            // wrote, `Date` included.
            dkim::dkim_sign(&mut message, dkim);
        }

        Ok(message)
    }
}

impl fmt::Debug for Smtp {
    /// The transport holds the relay credentials, and the signer holds the
    /// signing key, so only the sender and whether mail is signed are shown.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Smtp")
            .field("from", &self.from.to_string())
            .field("signed", &self.dkim.is_some())
            .finish_non_exhaustive()
    }
}

/// Turns configured signing settings into a signer, reading the key once.
///
/// A key that cannot be read is a deployment mistake, so it surfaces where the
/// mailer is built rather than on every send. The headers signed are lettre's
/// default set, `From`, `Subject`, `To`, and `Date`, which is the minimum every
/// verifier expects.
fn signer(dkim: &DkimConfig) -> Result<dkim::DkimConfig, Error> {
    let key = signing_key(dkim.private_key())
        .map_err(|source| Error::new("the DKIM signing key could not be read", source))?;

    Ok(dkim::DkimConfig::default_config(
        dkim.selector().to_owned(),
        dkim.domain().to_owned(),
        key,
    ))
}

/// Reads a signing key, choosing the algorithm from the value's shape.
///
/// RSA keys arrive as a PKCS#1 PEM and ed25519 keys as the base64 seed, so the
/// `-----BEGIN` a PEM opens with tells the two apart and no variable has to
/// name the algorithm.
fn signing_key(value: &str) -> Result<DkimSigningKey, DkimSigningKeyError> {
    let algorithm = if value.starts_with("-----BEGIN") {
        DkimSigningAlgorithm::Rsa
    } else {
        DkimSigningAlgorithm::Ed25519
    };

    DkimSigningKey::new(value, algorithm)
}

/// Returns the domain of `value`, when it is a valid email address.
///
/// A display name is allowed, as in `Acme <no-reply@acme.com>`. This is how
/// [`crate::config`] rejects a malformed `MAIL_FROM` at startup instead of at
/// the first send, without reaching for the mail transport itself, and how a
/// DKIM signature defaults to the sender's own domain.
pub(crate) fn address_domain(value: &str) -> Option<String> {
    value
        .parse::<Mailbox>()
        .ok()
        .map(|mailbox| mailbox.email.domain().to_owned())
}

/// Returns `true` when `value` parses as a DKIM signing key.
///
/// This is how [`crate::config`] rejects a malformed `DKIM_PRIVATE_KEY` at
/// startup instead of at the first send.
pub(crate) fn is_valid_signing_key(value: &str) -> bool {
    signing_key(value).is_ok()
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
    use std::sync::Arc;

    use super::{Email, Mailer, Smtp, address_domain, dkim, is_valid_signing_key, signing_key};
    use crate::config::{AppConfig, DkimConfig};
    use lettre::message::Mailbox;
    use lettre::{AsyncSmtpTransport, Tokio1Executor};

    /// A throwaway 2048-bit PKCS#1 key, shared with the mock-relay suite.
    ///
    /// It lives beside that suite's other fixtures so one key covers both, and
    /// it signs nothing outside these tests.
    const TEST_PRIVATE_KEY: &str = include_str!("../tests/support/dkim_test_key.pem");

    /// A throwaway ed25519 seed: 32 bytes, base64, the form lettre reads.
    const TEST_ED25519_SEED: &str = "urSx/qgLgH8LRgRC4LWmUspd20lyppax/7lmY0bhqTw=";

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
            dkim: None,
        }
    }

    /// The same backend, signing every message with the test key.
    fn signing_smtp() -> Smtp {
        let key = signing_key(TEST_PRIVATE_KEY).expect("the test key must be read");

        Smtp {
            dkim: Some(Arc::new(dkim::DkimConfig::default_config(
                "mail".to_owned(),
                "acme.com".to_owned(),
                key,
            ))),
            ..offline_smtp()
        }
    }

    /// The signing settings a `DKIM_*` environment produces.
    ///
    /// Built through [`AppConfig`] because that is the only way one exists:
    /// the settings are validated at startup and never assembled by hand.
    fn dkim_settings() -> DkimConfig {
        AppConfig::from_lookup(|name| match name {
            "SMTP_URL" => Some("smtp://127.0.0.1:2525".to_owned()),
            "MAIL_FROM" => Some("Acme <no-reply@acme.com>".to_owned()),
            "DKIM_PRIVATE_KEY" => Some(TEST_PRIVATE_KEY.to_owned()),
            "DKIM_SELECTOR" => Some("mail".to_owned()),
            _ => None,
        })
        .expect("the signing settings must parse")
        .smtp
        .and_then(|smtp| smtp.dkim().cloned())
        .expect("a configured key must reach the relay settings")
    }

    /// Joins the continuation lines a long header is folded across.
    fn unfold(rendered: &str) -> String {
        rendered.replace("\r\n ", "")
    }

    /// A subject is assembled from application data (a team's name rides in
    /// the invitation subject), so a line break in it must not become a header
    /// of the attacker's choosing. Encoding is what stops it: the word holding
    /// the break is emitted as an RFC 2047 encoded word, break and all.
    #[tokio::test]
    async fn a_line_break_in_a_subject_cannot_forge_a_header() {
        let mut email = sample_email();
        email.subject = "Ops\r\nBcc: attacker@example.com".to_owned();

        let message = offline_smtp()
            .message(&email)
            .expect("the message must render");
        let rendered = String::from_utf8(message.formatted()).expect("messages are UTF-8");

        let (_headers, body) = rendered
            .split_once("\r\n\r\n")
            .expect("a message separates its headers from its body");
        assert!(
            !rendered.contains("\r\nBcc:"),
            "the break was emitted verbatim: {rendered:?}",
        );
        assert!(
            rendered.contains("Subject: =?utf-8?b?"),
            "the subject must be encoded rather than passed through: {rendered:?}",
        );
        assert_eq!(body, email.text_body, "the body is untouched");
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
        assert!(
            !rendered.contains("DKIM-Signature"),
            "an unconfigured relay signs nothing: {rendered}",
        );
    }

    /// The signature itself is lettre's; what belongs to Anubis is that a
    /// configured key reaches it, with the selector and domain a verifier will
    /// look the public key up under.
    #[tokio::test]
    async fn a_signed_message_names_the_selector_and_the_domain_it_signs_for() {
        let message = signing_smtp()
            .message(&sample_email())
            .expect("the message must render");
        let rendered = unfold(&String::from_utf8(message.formatted()).expect("messages are UTF-8"));

        assert!(
            rendered.contains("DKIM-Signature: v=1; a=rsa-sha256;"),
            "got: {rendered}",
        );
        assert!(rendered.contains("d=acme.com;"), "got: {rendered}");
        assert!(rendered.contains("s=mail;"), "got: {rendered}");
        assert!(
            rendered.contains("h=From:Subject:To:Date;"),
            "the covered headers are the ones every verifier expects: {rendered}",
        );
        assert!(
            rendered.contains("token=abc"),
            "the body is delivered as written: {rendered}",
        );
    }

    #[tokio::test]
    async fn a_signing_mailer_builds_from_settings_and_never_shows_the_key() {
        let mailer = Mailer::smtp_signed(
            "smtps://user:hunter2@smtp.example.com:465",
            "Acme <no-reply@acme.com>",
            &dkim_settings(),
        )
        .expect("the mailer must build");

        let rendered = format!("{mailer:?}");
        assert!(rendered.contains("signed: true"), "got: {rendered}");
        assert!(!rendered.contains("hunter2"), "got: {rendered}");
        assert!(
            !rendered.contains("MII"),
            "no part of the key may be printed: {rendered}",
        );
    }

    #[test]
    fn signing_keys_are_recognized_in_both_forms_lettre_reads() {
        assert!(is_valid_signing_key(TEST_PRIVATE_KEY));
        assert!(is_valid_signing_key(TEST_ED25519_SEED));

        assert!(!is_valid_signing_key(""));
        assert!(!is_valid_signing_key("hunter2"));
        // A PKCS#8 PEM is the shape `openssl genrsa` writes by default, and the
        // one mistake worth being sure about: it is refused, and the error
        // names the `-traditional` flag that fixes it.
        assert!(!is_valid_signing_key(
            "-----BEGIN PRIVATE KEY-----\nMIIB\n-----END PRIVATE KEY-----",
        ));
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
    fn the_domain_of_an_address_is_read_with_and_without_a_display_name() {
        assert_eq!(
            address_domain("no-reply@acme.com").as_deref(),
            Some("acme.com"),
        );
        assert_eq!(
            address_domain("Acme <no-reply@mail.acme.com>").as_deref(),
            Some("mail.acme.com"),
        );
        assert!(address_domain("no-reply").is_none());
        assert!(address_domain("").is_none());
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
        let rendered = format!("{mailer:?}");
        assert!(rendered.contains("Smtp"), "got: {rendered}");
        assert!(
            rendered.contains("signed: false"),
            "a relay signs nothing until a key is configured: {rendered}",
        );

        let signing = AppConfig::from_lookup(|name| match name {
            "SMTP_URL" => Some("smtps://user:secret@smtp.example.com:465".to_owned()),
            "MAIL_FROM" => Some("Acme <no-reply@acme.com>".to_owned()),
            "DKIM_PRIVATE_KEY" => Some(TEST_PRIVATE_KEY.to_owned()),
            "DKIM_SELECTOR" => Some("mail".to_owned()),
            _ => None,
        })
        .expect("a configured key must parse");
        let mailer = Mailer::from_config(&signing).expect("the signing relay must build");
        assert!(format!("{mailer:?}").contains("signed: true"));
    }
}

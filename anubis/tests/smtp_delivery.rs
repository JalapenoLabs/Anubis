//! Delivery through the SMTP backend, against a mock relay.
//!
//! A real relay cannot run in a test, so this test *is* one: a tokio listener
//! speaks just enough SMTP (greeting, EHLO, MAIL FROM, RCPT TO, DATA, QUIT) to
//! accept one message and hand the transcript back. Nothing about the code
//! under test is special-cased for the test: the mailer is the real
//! [`anubis::mail::Mailer::smtp`] driving lettre's real tokio transport, over
//! a real TCP connection, pointed at `smtp://127.0.0.1:<port>`.
//!
//! Plaintext is the honest shape here. `smtp://` without a `tls=` parameter is
//! a mode lettre supports on purpose, for relays reached over a trusted link
//! such as a sidecar on localhost, so the test exercises a supported path
//! rather than a test-only escape hatch. TLS negotiation itself belongs to
//! rustls and is not re-tested here.

use std::time::Duration;

use anubis::mail::{Email, Mailer};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::net::tcp::OwnedWriteHalf;

/// Bounds the test if the transport never completes the session.
const SESSION_TIMEOUT: Duration = Duration::from_secs(10);

/// The command that opens the envelope, and prefixes the sender.
const MAIL_FROM: &str = "MAIL FROM:";

/// The command that adds one recipient to the envelope.
const RCPT_TO: &str = "RCPT TO:";

/// What the mock relay saw during one session.
#[derive(Debug, Default)]
struct Session {
    /// The address in `MAIL FROM:<...>`, angle brackets included.
    mail_from: String,
    /// Every address in `RCPT TO:<...>`, angle brackets included.
    recipients: Vec<String>,
    /// The message the client sent between `DATA` and its terminating dot.
    data: String,
}

/// Accepts one SMTP session on `listener` and returns what it received.
///
/// The session ends at the message terminator or at `QUIT`, whichever comes
/// first: lettre pools connections, so it holds the socket open after a
/// successful send rather than saying goodbye.
async fn serve_one_session(listener: TcpListener) -> Session {
    let (stream, _peer) = listener.accept().await.expect("the client must connect");
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut session = Session::default();
    let mut in_data = false;

    reply(&mut writer, "220 mock.anubis.test ESMTP").await;

    while let Some(line) = lines.next_line().await.expect("the client must send lines") {
        if in_data {
            if line == "." {
                reply(&mut writer, "250 2.0.0 Ok: queued").await;
                return session;
            }
            session.data.push_str(&line);
            session.data.push('\n');
            continue;
        }

        // Commands are case-insensitive; addresses are not, so the verb is
        // matched on an uppercased copy and the argument taken from the line.
        let command = line.to_ascii_uppercase();
        if command.starts_with("EHLO") || command.starts_with("HELO") {
            // No capabilities: no AUTH to negotiate, no STARTTLS to offer.
            reply(&mut writer, "250 mock.anubis.test").await;
        } else if command.starts_with(MAIL_FROM) {
            session.mail_from = line[MAIL_FROM.len()..].trim().to_owned();
            reply(&mut writer, "250 2.1.0 Ok").await;
        } else if command.starts_with(RCPT_TO) {
            session
                .recipients
                .push(line[RCPT_TO.len()..].trim().to_owned());
            reply(&mut writer, "250 2.1.5 Ok").await;
        } else if command.starts_with("DATA") {
            in_data = true;
            reply(&mut writer, "354 End data with <CR><LF>.<CR><LF>").await;
        } else if command.starts_with("QUIT") {
            reply(&mut writer, "221 2.0.0 Bye").await;
            return session;
        } else {
            reply(&mut writer, "250 2.0.0 Ok").await;
        }
    }

    session
}

/// Writes one CRLF-terminated SMTP reply line.
async fn reply(writer: &mut OwnedWriteHalf, line: &str) {
    writer
        .write_all(format!("{line}\r\n").as_bytes())
        .await
        .expect("the mock relay must reply");
}

#[tokio::test]
async fn a_message_travels_through_the_real_transport_to_the_relay() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port must be available");
    let port = listener
        .local_addr()
        .expect("the listener must have an address")
        .port();
    let relay = tokio::spawn(serve_one_session(listener));

    let mailer = Mailer::smtp(
        &format!("smtp://127.0.0.1:{port}"),
        "Anubis <no-reply@anubis.test>",
    )
    .expect("the mailer must build");

    mailer
        .send(Email {
            to: "someone@example.com".to_owned(),
            subject: "Verify your email".to_owned(),
            text_body: "Confirm with https://app.example.com/verify-email?token=abc".to_owned(),
        })
        .await
        .expect("the relay must accept the message");

    let session = tokio::time::timeout(SESSION_TIMEOUT, relay)
        .await
        .expect("the session must finish")
        .expect("the mock relay must not panic");

    assert_eq!(session.mail_from, "<no-reply@anubis.test>");
    assert_eq!(session.recipients, ["<someone@example.com>"]);
    assert!(
        session.data.contains("From: Anubis <no-reply@anubis.test>"),
        "got: {}",
        session.data
    );
    assert!(
        session.data.contains("To: someone@example.com"),
        "got: {}",
        session.data
    );
    assert!(
        session.data.contains("Subject: Verify your email"),
        "got: {}",
        session.data
    );
    assert!(
        session.data.contains("verify-email?token=abc"),
        "got: {}",
        session.data
    );
}

#[tokio::test]
async fn a_relay_that_is_not_listening_surfaces_as_a_send_error() {
    // Bind and drop, so the port is one nothing answers on.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port must be available");
    let port = listener
        .local_addr()
        .expect("the listener must have an address")
        .port();
    drop(listener);

    let mailer = Mailer::smtp(&format!("smtp://127.0.0.1:{port}"), "no-reply@anubis.test")
        .expect("the mailer must build without contacting the relay");

    let error = mailer
        .send(Email {
            to: "someone@example.com".to_owned(),
            subject: "Verify your email".to_owned(),
            text_body: "Confirm your address.".to_owned(),
        })
        .await
        .expect_err("a dead relay must fail the send");

    assert!(error.to_string().contains("SMTP relay"), "got: {error}");
}

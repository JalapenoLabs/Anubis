//! A session store that fails spends nothing a person needs to get back in.
//!
//! Signing in with a recovery code, an emailed code, or a passkey spends the
//! code or the ceremony in the same transaction that writes the session, so a
//! store that cannot write it rolls everything back. A password reset ends sessions in
//! Postgres and only sweeps the store after the commit, so it completes while
//! the store is down. Each scenario runs two deployments over one database: a
//! healthy one on the Postgres store, and one whose Redis store points at a
//! port nothing listens on, which fails every command at once.
//!
//! Requires `DATABASE_URL`; without it each test logs a skip and passes. It
//! needs no Redis: the failing store fails without one.

mod support;

use anubis::config::AppConfig;
use anubis::mail::{EmailKind, TestOutbox};
use axum::Router;
use axum::http::StatusCode;
use chrono::Utc;
use serde_json::json;
use support::{PASSWORD, SoftAuthenticator, TestDatabase, register, send, session_token};
use uuid::Uuid;

/// A Redis store that can never be reached: nothing listens on the discard
/// port, so every connection is refused at once.
const UNREACHABLE_REDIS: &str = "redis://127.0.0.1:9";

/// Where the deployments say they are served from.
///
/// A passkey's relying party is `APP_URL`'s host, and a browser refuses a
/// ceremony at a bare IP address, which is what the default would be.
const ORIGIN: &str = "http://localhost:3000";

/// The password a reset through the failing deployment sets.
const RESET_PASSWORD: &str = "a passphrase set while the store was down";

/// A healthy deployment and a failing one, over the same database.
struct Deployments {
    healthy: Router,
    healthy_outbox: TestOutbox,
    failing: Router,
}

impl Deployments {
    async fn boot(database: &TestDatabase) -> Self {
        let pool = database.pool().await;
        let (healthy, healthy_outbox) = deployment(&pool, &[]);
        let (failing, _failing_outbox) = deployment(
            &pool,
            &[("SESSION_STORE", "redis"), ("REDIS_URL", UNREACHABLE_REDIS)],
        );
        Self {
            healthy,
            healthy_outbox,
            failing,
        }
    }

    /// The text of the most recent email of `kind` the healthy one sent `to`.
    fn latest(&self, to: &str, kind: EmailKind) -> String {
        self.healthy_outbox
            .emails()
            .into_iter()
            .rev()
            .find(|sent| sent.to == to && sent.kind == kind)
            .expect("the email must have been sent")
            .text_body
    }
}

/// The authentication routes, hardened as a served application's are, under
/// the configuration `variables` describe.
fn deployment(pool: &anubis::db::DbPool, variables: &[(&str, &str)]) -> (Router, TestOutbox) {
    let owned: Vec<(String, String)> = variables
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .chain([
            ("ANUBIS_ENV".to_owned(), "test".to_owned()),
            ("APP_URL".to_owned(), ORIGIN.to_owned()),
            ("RATE_LIMIT_DISABLED".to_owned(), "true".to_owned()),
        ])
        .collect();
    let config = AppConfig::from_lookup(move |name| {
        owned
            .iter()
            .find(|(variable, _value)| variable == name)
            .map(|(_variable, value)| value.clone())
    })
    .expect("the test configuration must parse");

    let (mailer, outbox) = anubis::mail::Mailer::test();
    let rate_limit = anubis::rate_limit::RateLimiter::new(&config.rate_limit);
    let routes = Router::new().nest(
        "/auth",
        anubis::auth::router(pool.clone(), mailer, &config, &rate_limit),
    );
    (
        anubis::server::harden(routes, pool.clone(), &config),
        outbox,
    )
}

/// The value after `marker` in an email's text, up to the next whitespace.
fn after(text: &str, marker: &str) -> String {
    let (_before, rest) = text
        .split_once(marker)
        .unwrap_or_else(|| panic!("the email must contain {marker:?}: {text}"));
    rest.split_whitespace()
        .next()
        .expect("the value ends at whitespace")
        .to_owned()
}

#[tokio::test]
async fn a_recovery_code_survives_a_session_store_that_fails() {
    let Some(database) = TestDatabase::create("session_store_outage_mfa").await else {
        return;
    };
    let deployments = Deployments::boot(&database).await;
    let healthy = &deployments.healthy;
    let email = format!("recovering-{}@example.com", Uuid::new_v4());
    let cookie = register(healthy, &email).await;

    // Turn on a second factor, and keep a recovery code.
    let (status, _headers, body) =
        send(healthy, "POST", "/auth/mfa/totp/setup", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let secret = body["secret"].as_str().expect("a secret").to_owned();
    let now = u64::try_from(Utc::now().timestamp()).expect("a sane clock");
    let code = anubis::auth::totp::code_at(&secret, now).expect("the secret decodes");
    let (status, _headers, body) = send(
        healthy,
        "POST",
        "/auth/mfa/totp/confirm",
        Some(&json!({ "code": code })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let recovery_code = body["recovery_codes"][0]
        .as_str()
        .expect("recovery codes are handed over")
        .to_owned();

    // A password alone now answers a challenge, which writes no session.
    let (status, _headers, body) = send(
        healthy,
        "POST",
        "/auth/login",
        Some(&json!({ "email": email, "password": PASSWORD })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let challenge = body["mfa_token"].as_str().expect("a challenge").to_owned();
    let answer = json!({ "mfa_token": challenge, "code": recovery_code });

    // The store fails to write the session...
    let (status, _headers, _body) = send(
        &deployments.failing,
        "POST",
        "/auth/mfa/verify",
        Some(&answer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    // ...and neither the recovery code nor the challenge was spent on it.
    let (status, headers, body) =
        send(healthy, "POST", "/auth/mfa/verify", Some(&answer), None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    session_token(&headers);
}

#[tokio::test]
async fn an_emailed_code_survives_a_session_store_that_fails() {
    let Some(database) = TestDatabase::create("session_store_outage_code").await else {
        return;
    };
    let deployments = Deployments::boot(&database).await;
    let healthy = &deployments.healthy;
    let email = format!("emailed-{}@example.com", Uuid::new_v4());
    register(healthy, &email).await;

    let (status, _headers, body) = send(
        healthy,
        "POST",
        "/auth/email-code/request",
        Some(&json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    let code = after(
        &deployments.latest(&email, EmailKind::SignInCode),
        "code is ",
    );
    let answer = json!({ "email": email, "code": code });

    let (status, _headers, _body) = send(
        &deployments.failing,
        "POST",
        "/auth/email-code/verify",
        Some(&answer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    let (status, headers, body) = send(
        healthy,
        "POST",
        "/auth/email-code/verify",
        Some(&answer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    session_token(&headers);
}

#[tokio::test]
async fn a_passkey_ceremony_survives_a_session_store_that_fails() {
    let Some(database) = TestDatabase::create("session_store_outage_passkey").await else {
        return;
    };
    let deployments = Deployments::boot(&database).await;
    let healthy = &deployments.healthy;
    let email = format!("passkey-{}@example.com", Uuid::new_v4());
    let cookie = register(healthy, &email).await;
    let mut device = SoftAuthenticator::new(ORIGIN);

    let (status, _headers, body) = send(
        healthy,
        "POST",
        "/auth/passkeys/register/start",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let registration = json!({
        "state_token": body["state_token"],
        "credential": device.create(&body["creation_options"]),
        "name": "Password manager",
    });
    let (status, _headers, body) = send(
        healthy,
        "POST",
        "/auth/passkeys/register/finish",
        Some(&registration),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let (status, _headers, body) =
        send(healthy, "POST", "/auth/passkeys/login/start", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let login = json!({
        "state_token": body["state_token"],
        "credential": device.get(&body["request_options"]),
    });

    let (status, _headers, _body) = send(
        &deployments.failing,
        "POST",
        "/auth/passkeys/login/finish",
        Some(&login),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    // The very same signed assertion finishes the very same ceremony, so
    // neither the ceremony nor the credential's counter moved for the failure.
    let (status, headers, body) = send(
        healthy,
        "POST",
        "/auth/passkeys/login/finish",
        Some(&login),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    session_token(&headers);
}

#[tokio::test]
async fn a_password_reset_completes_while_the_session_store_is_down() {
    let Some(database) = TestDatabase::create("session_store_outage_reset").await else {
        return;
    };
    let deployments = Deployments::boot(&database).await;
    let healthy = &deployments.healthy;
    let email = format!("resetting-{}@example.com", Uuid::new_v4());
    let old_session = register(healthy, &email).await;

    let (status, _headers, _body) = send(
        healthy,
        "POST",
        "/auth/password-reset/request",
        Some(&json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let token = after(
        &deployments.latest(&email, EmailKind::ResetPassword),
        "token=",
    );
    let reset = json!({ "token": token, "password": RESET_PASSWORD });

    // Ending the sessions is decided in Postgres, so a store that is down
    // cannot hold the reset up; it only misses the sweep afterwards.
    let (status, _headers, body) = send(
        &deployments.failing,
        "POST",
        "/auth/password-reset/confirm",
        Some(&reset),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, _headers, _body) =
        send(healthy, "GET", "/auth/me", None, Some(&old_session)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the reset ended it");

    // The link was spent by the reset that used it, and the password moved.
    let (status, _headers, _body) = send(
        healthy,
        "POST",
        "/auth/password-reset/confirm",
        Some(&reset),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _headers, body) = send(
        healthy,
        "POST",
        "/auth/login",
        Some(&json!({ "email": email, "password": RESET_PASSWORD })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

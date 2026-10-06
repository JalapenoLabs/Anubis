//! Every sign-in leaves a record, and the record says where it came from.
//!
//! A deployment has to be able to answer "when did this account last sign in,
//! and from where" without a request log that rotated away weeks ago. So every
//! path that issues a session writes one `session.created` audit event, in the
//! same transaction as the session, naming how the account proved itself.
//!
//! Two acts. The first drives each path this harness can reach without an
//! external party (registering, a password, an emailed code, and a password
//! followed by a second factor) and reads the method each one recorded. The
//! passkey and OpenID Connect paths are proved in their own ceremony suites,
//! which already stand up the authenticator and the provider they need. The
//! second act puts the origin layer in front of the router and proves the
//! address, the browser and the location reach the row, and that a location
//! header nobody configured is ignored.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

mod support;

use std::net::SocketAddr;

use anubis::auth::SignInMethod;
use anubis::schema::{audit_events, users};
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::header::{CONTENT_TYPE, USER_AGENT};
use axum::http::{Request, StatusCode};
use chrono::Utc;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::{Value, json};
use support::{Harness, PASSWORD, TestDatabase, register, send, session_token};
use tower::ServiceExt;
use uuid::Uuid;

/// One sign-in as the log recorded it.
#[derive(Debug, Queryable)]
struct SignIn {
    changes: Value,
    ip_address: Option<String>,
    user_agent: Option<String>,
    location: Option<String>,
}

impl SignIn {
    fn method(&self) -> &str {
        self.changes["method"]["new"]
            .as_str()
            .expect("a sign-in names its method")
    }
}

/// Every sign-in the account behind `email` has made, oldest first.
async fn sign_ins(harness: &Harness, email: &str) -> Vec<SignIn> {
    let mut connection = harness.pool.get().await.expect("a connection is available");
    let user_id: Uuid = users::table
        .filter(users::email.eq(email))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the account exists");

    audit_events::table
        .filter(audit_events::action.eq(anubis::audit::SESSION_CREATED))
        .filter(audit_events::user_id.eq(user_id))
        .filter(audit_events::subject_id.eq(user_id))
        .order(audit_events::created_at.asc())
        .select((
            audit_events::changes,
            audit_events::ip_address,
            audit_events::user_agent,
            audit_events::location,
        ))
        .load(&mut connection)
        .await
        .expect("the log is readable")
}

fn methods(records: &[SignIn]) -> Vec<&str> {
    records.iter().map(SignIn::method).collect()
}

#[tokio::test]
async fn every_path_that_issues_a_session_records_how() {
    let Some(database) = TestDatabase::create("sign_in_record_flow_paths").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let email = format!("sign-ins-{}@example.com", Uuid::new_v4());
    let credentials = json!({ "email": email, "password": PASSWORD });

    // Registering signs the new account in, so it is the first sign-in.
    let cookie = register(router, &email).await;
    assert_eq!(
        methods(&sign_ins(&harness, &email).await),
        [SignInMethod::Registration.as_str()],
    );

    // A password.
    let (status, _headers, body) =
        send(router, "POST", "/auth/login", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // An emailed code, read out of the outbox the way the inbox would.
    let (status, _headers, body) = send(
        router,
        "POST",
        "/auth/email-code/request",
        Some(&json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "body: {body}");
    let code = emailed_code(&harness, &email);
    let (status, _headers, body) = send(
        router,
        "POST",
        "/auth/email-code/verify",
        Some(&json!({ "email": email, "code": code })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // A password, then a second factor. The challenge in between issues no
    // session, so it must record no sign-in either.
    let secret = enroll_totp(router, &cookie).await;
    let (status, headers, body) =
        send(router, "POST", "/auth/login", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        headers
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .next()
            .is_none(),
        "a challenge is not a session",
    );
    assert_eq!(
        sign_ins(&harness, &email).await.len(),
        3,
        "the challenge recorded nothing",
    );
    let challenge = body["mfa_token"].as_str().expect("a challenge token");
    let (status, headers, body) = send(
        router,
        "POST",
        "/auth/mfa/verify",
        Some(&json!({ "mfa_token": challenge, "code": current_code(&secret) })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let _signed_in = session_token(&headers);

    let records = sign_ins(&harness, &email).await;
    assert_eq!(
        methods(&records),
        [
            SignInMethod::Registration.as_str(),
            SignInMethod::Password.as_str(),
            SignInMethod::EmailCode.as_str(),
            SignInMethod::SecondFactor.as_str(),
        ],
    );
    assert!(
        records
            .iter()
            .all(|record| record.ip_address.is_none() && record.location.is_none()),
        "a router nothing hardened measured no origin, and records none: {records:?}",
    );
}

#[tokio::test]
async fn a_sign_in_records_the_address_browser_and_location_it_came_from() {
    let Some(database) = TestDatabase::create("sign_in_record_flow_origin").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let trusting = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        "TRUSTED_PROXY_HEADER" => Some("x-forwarded-for".to_owned()),
        "TRUSTED_LOCATION_HEADER" => Some("x-client-geo-location".to_owned()),
        _ => None,
    })
    .expect("the test config must parse");
    let behind_balancer = anubis::server::origin::layer(harness.router.clone(), &trusting);

    let email = format!("origin-{}@example.com", Uuid::new_v4());
    let _cookie = register(&harness.router, &email).await;
    let (status, body) = sign_in_from(&behind_balancer, &email).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let records = sign_ins(&harness, &email).await;
    let latest = records.last().expect("the password sign-in was recorded");
    assert_eq!(latest.method(), SignInMethod::Password.as_str());
    assert_eq!(
        latest.ip_address.as_deref(),
        Some("203.0.113.5"),
        "the hop the trusted proxy appended, never the one the client wrote",
    );
    assert_eq!(latest.user_agent.as_deref(), Some(BROWSER));
    assert_eq!(latest.location.as_deref(), Some("US,mountain view"));

    // The same request through a deployment that trusts no location header:
    // the header still arrives, because a client can send anything, and the
    // row ignores it.
    let untrusting = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        _ => None,
    })
    .expect("the test config must parse");
    let bare = anubis::server::origin::layer(harness.router.clone(), &untrusting);
    let (status, body) = sign_in_from(&bare, &email).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let records = sign_ins(&harness, &email).await;
    let latest = records.last().expect("the second sign-in was recorded");
    assert_eq!(latest.location, None, "nobody vouched for that location");
    assert_eq!(
        latest.ip_address.as_deref(),
        Some("198.51.100.7"),
        "with no proxy trusted, the peer is the client",
    );
}

/// The browser every request in the origin act claims to be.
const BROWSER: &str = "Mozilla/5.0 (X11; Linux x86_64) Firefox/140.0";

/// Signs in with a password through `router`, as a client behind a balancer.
///
/// The connection is described the way the accept loop describes one, and the
/// forwarding header carries a first hop the client wrote itself, which is the
/// one a careless reader would trust.
async fn sign_in_from(router: &Router, email: &str) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/auth/login")
        .header(CONTENT_TYPE, "application/json")
        .header(USER_AGENT, BROWSER)
        .header("x-forwarded-for", "10.0.0.1, 203.0.113.5")
        .header("x-client-geo-location", "US,mountain view")
        .body(Body::from(
            serde_json::to_vec(&json!({ "email": email, "password": PASSWORD }))
                .expect("the body serializes"),
        ))
        .expect("the request builds");
    let peer: SocketAddr = "198.51.100.7:43210"
        .parse()
        .expect("a literal address parses");
    request.extensions_mut().insert(ConnectInfo(peer));

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = http_body_util::BodyExt::collect(response.into_body())
        .await
        .expect("the body collects")
        .to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Confirms a TOTP enrollment for the account behind `cookie`, returning its seed.
async fn enroll_totp(router: &Router, cookie: &str) -> String {
    let (status, _headers, body) =
        send(router, "POST", "/auth/mfa/totp/setup", None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let secret = body["secret"].as_str().expect("a seed").to_owned();

    let (status, _headers, body) = send(
        router,
        "POST",
        "/auth/mfa/totp/confirm",
        Some(&json!({ "code": current_code(&secret) })),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    secret
}

fn current_code(secret: &str) -> String {
    let now = u64::try_from(Utc::now().timestamp()).expect("a sane clock");
    anubis::auth::totp::code_at(secret, now).expect("the seed decodes")
}

/// The six digits in the newest sign-in code mailed to `email`.
fn emailed_code(harness: &Harness, email: &str) -> String {
    let sent = harness
        .outbox
        .emails()
        .into_iter()
        .rev()
        .find(|sent| sent.to == email && sent.subject.contains("sign-in code"))
        .expect("the code email is in the outbox");
    let (_before, rest) = sent
        .text_body
        .split_once("code is ")
        .expect("the email carries the code");
    rest.split_whitespace()
        .next()
        .expect("the code ends at whitespace")
        .to_owned()
}

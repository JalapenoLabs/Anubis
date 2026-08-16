//! Both passkey ceremonies, completed end to end by a software authenticator.
//!
//! [`support::SoftAuthenticator`] holds a real ES256 key and signs what a
//! browser would collect, so every byte the relying party verifies here is
//! genuine: the attestation, the assertion, the challenge, and the origin. A
//! session at the end of it means the cryptographic path works, which is the
//! one thing `passkey_flow` cannot say.
//!
//! The refusals matter as much as the round trip, because a ceremony that
//! accepts everything also accepts an attacker: a `clientDataJSON` that names
//! another origin, one account's credential offered as another's, a finish
//! call replayed after its state token was spent, and a registration state
//! claimed by an account that did not start it.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

mod support;

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use serde_json::{Value, json};
use support::{SoftAuthenticator, TestDatabase, register, send, session_token};
use uuid::Uuid;

/// The origin the browser reports and the relying party is derived from.
///
/// `anubis::auth::passkey` takes the relying party id out of `APP_URL`'s host,
/// so the application and the authenticator have to name the same origin or
/// nothing verifies. It has to be `localhost`: browsers refuse a ceremony at a
/// bare IP address, and the authenticator library refuses with them, which is
/// why this suite composes its own router rather than booting a `Harness`.
const ORIGIN: &str = "http://localhost:3000";

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear ceremony narrative, told once from register to refusal"
)]
async fn a_software_passkey_registers_signs_in_and_is_refused_when_forged() {
    let Some(database) = TestDatabase::create("passkey_ceremony_flow").await else {
        return;
    };
    let router = boot(&database).await;

    let email = format!("passkey-{}@example.com", Uuid::new_v4());
    let cookie = register(&router, &email).await;
    let mut device = SoftAuthenticator::new(ORIGIN);

    // ------------------------------------------------------------------
    // Registration: the authenticator attests, the API stores the key.
    // ------------------------------------------------------------------
    let (state_token, creation_options) = register_start(&router, &cookie).await;
    assert_eq!(
        creation_options["publicKey"]["rp"]["id"],
        json!("localhost"),
        "the relying party follows APP_URL: {creation_options}"
    );

    let (status, body) = register_finish(
        &router,
        &cookie,
        &json!({
            "state_token": state_token,
            "credential": device.create(&creation_options),
            "name": "Password manager",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["passkeys"][0]["name"], json!("Password manager"));

    let (status, _headers, body) =
        send(&router, "GET", "/auth/passkeys", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["passkeys"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        body["passkeys"][0]["last_used_at"],
        Value::Null,
        "the key has signed nothing yet: {body}"
    );

    // ------------------------------------------------------------------
    // Discoverable login: no username, no session, one signature.
    // ------------------------------------------------------------------
    let (state_token, request_options) = login_start(&router).await;
    let login = json!({
        "state_token": state_token,
        "credential": device.get(&request_options),
    });

    let (status, headers, body) = login_finish(&router, &login).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["user"]["email"], json!(email));
    let passkey_cookie = session_token(&headers);

    let (status, _headers, body) =
        send(&router, "GET", "/auth/me", None, Some(&passkey_cookie)).await;
    assert_eq!(status, StatusCode::OK, "the passkey issued a real session");
    assert_eq!(body["user"]["email"], json!(email), "body: {body}");
    let user_id: Uuid = body["user"]["id"]
        .as_str()
        .expect("the account has an id")
        .parse()
        .expect("a user id is a UUID");

    let (_status, _headers, body) = send(
        &router,
        "GET",
        "/auth/passkeys",
        None,
        Some(&passkey_cookie),
    )
    .await;
    assert!(
        body["passkeys"][0]["last_used_at"].is_string(),
        "the login stamps the credential it used: {body}"
    );

    // A spent state token is spent, even for the assertion that just worked.
    let (status, _headers, _body) = login_finish(&router, &login).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a login finish cannot be replayed"
    );

    // ------------------------------------------------------------------
    // A ceremony collected at another origin does not verify here.
    // ------------------------------------------------------------------
    let (state_token, request_options) = login_start(&router).await;
    let mut assertion = device.get(&request_options);
    assertion.response.client_data_json = with_origin(
        &assertion.response.client_data_json,
        "https://phish.example",
    );

    let (status, _headers, _body) = login_finish(
        &router,
        &json!({ "state_token": state_token, "credential": assertion }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the origin is signed over, and it is checked"
    );

    // ------------------------------------------------------------------
    // A second account, with a passkey on a device of its own.
    // ------------------------------------------------------------------
    let other_email = format!("passkey-{}@example.com", Uuid::new_v4());
    let other_cookie = register(&router, &other_email).await;
    let mut other_device = SoftAuthenticator::new(ORIGIN);

    let (state_token, creation_options) = register_start(&router, &other_cookie).await;
    let (status, body) = register_finish(
        &router,
        &other_cookie,
        &json!({
            "state_token": state_token,
            "credential": other_device.create(&creation_options),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    // That device signs its own owner in, which is what makes the refusal
    // below about the forged user handle and nothing else.
    let (state_token, request_options) = login_start(&router).await;
    let (status, _headers, body) = login_finish(
        &router,
        &json!({
            "state_token": state_token,
            "credential": other_device.get(&request_options),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["user"]["email"], json!(other_email));

    // The same credential, claiming to be the first account's. The user handle
    // says whose keys to look up; the signature has to hold against one of
    // them, and this credential is not among them.
    let (state_token, request_options) = login_start(&router).await;
    let mut assertion = other_device.get(&request_options);
    assertion.response.user_handle = Some(user_id.as_bytes().to_vec());

    let (status, _headers, _body) = login_finish(
        &router,
        &json!({ "state_token": state_token, "credential": assertion }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "one account's credential cannot sign another one in"
    );

    // A registration state belongs to the account that started it, so the
    // second account cannot finish the first account's ceremony.
    let (state_token, creation_options) = register_start(&router, &cookie).await;
    let (status, _body) = register_finish(
        &router,
        &other_cookie,
        &json!({
            "state_token": state_token,
            "credential": device.create(&creation_options),
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a registration state is not transferable"
    );
}

/// Composes the auth router over `database`, at [`ORIGIN`].
async fn boot(database: &TestDatabase) -> Router {
    let config = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        "APP_URL" => Some(ORIGIN.to_owned()),
        // Two registrations and four login attempts is a rate no person
        // reaches and the credential limiter is right to refuse; what the
        // limiter does is `rate_limit_flow`'s story rather than this one's.
        "RATE_LIMIT_DISABLED" => Some("true".to_owned()),
        _ => None,
    })
    .expect("the test config must parse");

    let (mailer, _outbox) = anubis::mail::Mailer::test();
    let rate_limit = anubis::rate_limit::RateLimiter::new(&config.rate_limit);

    Router::new().nest(
        "/auth",
        anubis::auth::router(database.pool().await, mailer, &config, &rate_limit),
    )
}

/// Starts a registration ceremony, returning its state token and challenge.
async fn register_start(router: &Router, cookie: &str) -> (String, Value) {
    let (status, _headers, body) = send(
        router,
        "POST",
        "/auth/passkeys/register/start",
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    (state_token(&body), body["creation_options"].clone())
}

/// Posts an attested credential to finish a registration ceremony.
async fn register_finish(router: &Router, cookie: &str, body: &Value) -> (StatusCode, Value) {
    let (status, _headers, answer) = send(
        router,
        "POST",
        "/auth/passkeys/register/finish",
        Some(body),
        Some(cookie),
    )
    .await;

    (status, answer)
}

/// Starts a discoverable login ceremony, which needs no session.
async fn login_start(router: &Router) -> (String, Value) {
    let (status, _headers, body) =
        send(router, "POST", "/auth/passkeys/login/start", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    (state_token(&body), body["request_options"].clone())
}

/// Posts a signed assertion to finish a login ceremony.
///
/// The headers come back with it, because a login that worked sets a session
/// cookie and that cookie is half of what this suite proves.
async fn login_finish(router: &Router, body: &Value) -> (StatusCode, HeaderMap, Value) {
    send(
        router,
        "POST",
        "/auth/passkeys/login/finish",
        Some(body),
        None,
    )
    .await
}

/// Reads the state token a started ceremony answered with.
fn state_token(body: &Value) -> String {
    body["state_token"]
        .as_str()
        .expect("a started ceremony has a state token")
        .to_owned()
}

/// Rewrites the origin a collected `clientDataJSON` names.
///
/// `clientDataJSON` is the browser's own record of the ceremony, and it is
/// hashed into what the authenticator signs, so a rewritten one is both the
/// wrong origin and the wrong signature. Either alone is enough to refuse it.
fn with_origin(client_data_json: &[u8], origin: &str) -> Vec<u8> {
    let mut client_data: Value =
        serde_json::from_slice(client_data_json).expect("collected client data is JSON");
    client_data["origin"] = json!(origin);

    serde_json::to_vec(&client_data).expect("collected client data must serialize")
}

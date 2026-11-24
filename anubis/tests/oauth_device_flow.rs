//! The device authorization grant, end to end: a headless program signs in.
//!
//! The program is played by the test, taking the steps a command-line tool on
//! a server with no browser takes: it serves a Client ID Metadata Document
//! declaring the device grant, asks for a device code and a user code, shows
//! the user code, and polls. The person is played by a signed-in session: it
//! enters the code, reads the consent screen, and decides. Every refusal the
//! grant defines is a step in one of these stories.
//!
//! Requires `DATABASE_URL`; without it each test logs a skip and passes. CI
//! always provides one.

mod support;

use std::net::SocketAddr;
use std::time::Instant;

use anubis::schema::{audit_events, oauth_clients, oauth_device_codes, oauth_grants, users};
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::header::{CONTENT_TYPE, COOKIE, RETRY_AFTER};
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use support::oauth::{
    DEVICE_CODE_GRANT, MetadataDocument, RESOURCE, authorize, authorize_params, decide,
    device_codes, enter_code, mcp, param, poll, post_form, request_for_code, start_device,
};
use support::{Harness, TestDatabase, register, send};

const WHOAMI: &str =
    r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"whoami","arguments":{}}}"#;

fn whoami() -> Value {
    serde_json::from_str(WHOAMI).expect("the call parses")
}

/// How many audit events of `action` were recorded.
async fn audited(harness: &Harness, action: &str) -> i64 {
    let mut connection = harness.pool.get().await.expect("a connection");
    audit_events::table
        .filter(audit_events::action.eq(action))
        .count()
        .get_result(&mut connection)
        .await
        .expect("the count runs")
}

/// Moves a device authorization's last poll `seconds` into the past, so the
/// next poll is held to the interval as if the program had waited.
async fn wait_out_the_interval(harness: &Harness, device_code: &str, seconds: i64) {
    let mut connection = harness.pool.get().await.expect("a connection");
    diesel::update(
        oauth_device_codes::table
            .filter(oauth_device_codes::device_code_hash.eq(device_code_hash(device_code))),
    )
    .set(oauth_device_codes::last_polled_at.eq(Some(Utc::now() - Duration::seconds(seconds))))
    .execute(&mut connection)
    .await
    .expect("the poll time moves");
}

/// Expires a device authorization and the request beside it, as ten minutes
/// passing would.
async fn expire(harness: &Harness, device_code: &str) {
    let mut connection = harness.pool.get().await.expect("a connection");
    let expired = Utc::now() - Duration::seconds(1);
    let device_code_id: Uuid = diesel::update(
        oauth_device_codes::table
            .filter(oauth_device_codes::device_code_hash.eq(device_code_hash(device_code))),
    )
    .set(oauth_device_codes::expires_at.eq(expired))
    .returning(oauth_device_codes::id)
    .get_result(&mut connection)
    .await
    .expect("the device code expires");
    diesel::update(
        anubis::schema::oauth_authorization_requests::table.filter(
            anubis::schema::oauth_authorization_requests::device_code_id.eq(device_code_id),
        ),
    )
    .set(anubis::schema::oauth_authorization_requests::expires_at.eq(expired))
    .execute(&mut connection)
    .await
    .expect("the request expires");
}

/// The stored form of a device code: its SHA-256, base64url, as every
/// credential in the authorization server is kept.
fn device_code_hash(device_code: &str) -> String {
    use base64::Engine;
    use sha2::Digest;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(device_code))
}

#[tokio::test]
async fn the_metadata_advertises_the_device_grant() {
    let Some(database) = TestDatabase::create("oauth_device_discovery").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let (status, _headers, metadata) = send(
        &harness.router,
        "GET",
        "/.well-known/oauth-authorization-server",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        metadata["device_authorization_endpoint"],
        "http://127.0.0.1:3000/oauth/device_authorization",
    );
    assert!(
        metadata["grant_types_supported"]
            .as_array()
            .expect("a list")
            .contains(&json!(DEVICE_CODE_GRANT)),
        "metadata: {metadata}",
    );
}

#[tokio::test]
async fn a_headless_program_signs_in_with_a_device_code() {
    let Some(database) = TestDatabase::create("oauth_device_sign_in").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let cookie = register(router, &format!("device-{}@example.com", Uuid::new_v4())).await;
    let program = MetadataDocument::serve_device("Server agent");

    // The program asks for codes, and is told where to send the person and
    // how often to poll.
    let codes = device_codes(router, &program.client_id, "notes:read").await;
    let user_code = codes.user_code.clone();
    assert_eq!(user_code.len(), 9, "two groups of four: {user_code}");
    assert!(
        user_code
            .chars()
            .all(|character| character == '-' || "BCDFGHJKLMNPQRSTVWXZ".contains(character)),
        "{user_code}",
    );
    assert_eq!(
        codes.response["verification_uri"],
        "http://127.0.0.1:3000/device"
    );
    assert_eq!(
        codes.response["verification_uri_complete"],
        format!("http://127.0.0.1:3000/device?user_code={user_code}"),
    );
    assert_eq!(codes.response["expires_in"], 600);
    assert_eq!(codes.response["interval"], 5);

    // Nobody has decided yet.
    let (status, pending) = poll(router, &program.client_id, &codes.device_code).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(pending["error"], "authorization_pending", "body: {pending}");

    // The person types the code as people do, in lowercase and without the
    // dash, and lands on the consent screen every client uses.
    let typed = user_code.replace('-', "").to_lowercase();
    let request_id = request_for_code(router, &cookie, &typed).await;
    let (status, _headers, request) = send(
        router,
        "GET",
        &format!("/oauth/requests/{request_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {request}");
    assert_eq!(request["client"]["name"], "Server agent");
    assert_eq!(request["client"]["verified_host"], "127.0.0.1");
    assert_eq!(request["delivery"], json!({ "kind": "device" }));
    assert_eq!(request["scopes"][0]["name"], "notes:read");

    // Approving sends the browser back to the code-entry page to say so.
    let landed = decide(router, &cookie, &request_id, true).await;
    assert_eq!(
        landed.as_str(),
        "http://127.0.0.1:3000/device?result=approved"
    );
    assert_eq!(audited(&harness, anubis::audit::OAUTH_GRANTED).await, 1);

    // The program's next poll is the sign-in.
    let (status, tokens) = poll(router, &program.client_id, &codes.device_code).await;
    assert_eq!(status, StatusCode::OK, "body: {tokens}");
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(tokens["scope"], "notes:read");
    let access_token = tokens["access_token"].as_str().expect("an access token");
    assert!(tokens["refresh_token"].is_string());

    let (status, _headers, answer) = mcp(router, Some(access_token), &whoami(), &[]).await;
    assert_eq!(status, StatusCode::OK, "body: {answer}");
    let identity = &answer["result"]["structuredContent"];
    assert!(
        identity["user"]["email"]
            .as_str()
            .is_some_and(|email| email.starts_with("device-")),
        "whoami names the person: {answer}",
    );
    assert_eq!(identity["client"], "Server agent");

    // The connection is listed with the others and ends the same way.
    let (_status, _headers, listed) =
        send(router, "GET", "/oauth/connections", None, Some(&cookie)).await;
    let connections = listed["connections"].as_array().expect("a list");
    assert_eq!(connections.len(), 1, "body: {listed}");
    assert_eq!(connections[0]["client"]["name"], "Server agent");
    let grant_id = connections[0]["id"].as_str().expect("an id");
    let (status, _headers, _body) = send(
        router,
        "DELETE",
        &format!("/oauth/connections/{grant_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _headers, _answer) = mcp(router, Some(access_token), &whoami(), &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "revocation is immediate");
}

#[tokio::test]
async fn a_program_polling_too_soon_is_slowed_down() {
    let Some(database) = TestDatabase::create("oauth_device_slow_down").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let program = MetadataDocument::serve_device("Server agent");
    let codes = device_codes(router, &program.client_id, "").await;

    // The first poll is never too soon. Polling again at once is, and every
    // too-soon poll grows the interval by five seconds.
    let (_status, pending) = poll(router, &program.client_id, &codes.device_code).await;
    assert_eq!(pending["error"], "authorization_pending", "body: {pending}");
    let (status, hurried) = poll(router, &program.client_id, &codes.device_code).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(hurried["error"], "slow_down", "body: {hurried}");
    wait_out_the_interval(&harness, &codes.device_code, 6).await;
    let (_status, still_hurried) = poll(router, &program.client_id, &codes.device_code).await;
    assert_eq!(
        still_hurried["error"], "slow_down",
        "six seconds is too soon once the interval is ten",
    );

    // Waiting out the grown interval is soon enough.
    wait_out_the_interval(&harness, &codes.device_code, 15).await;
    let (_status, waited) = poll(router, &program.client_id, &codes.device_code).await;
    assert_eq!(waited["error"], "authorization_pending", "body: {waited}");
}

#[tokio::test]
async fn a_device_code_is_exchanged_once_and_its_replay_ends_the_connection() {
    let Some(database) = TestDatabase::create("oauth_device_reuse").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let cookie = register(router, &format!("replay-{}@example.com", Uuid::new_v4())).await;
    let program = MetadataDocument::serve_device("Server agent");
    let codes = device_codes(router, &program.client_id, "").await;
    let request_id = request_for_code(router, &cookie, &codes.user_code).await;
    decide(router, &cookie, &request_id, true).await;

    // Another client naming the code gets nothing and spends nothing.
    let (status, refused) = poll(router, "someone-else", &codes.device_code).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["error"], "invalid_grant");

    let (status, tokens) = poll(router, &program.client_id, &codes.device_code).await;
    assert_eq!(status, StatusCode::OK, "body: {tokens}");
    let access_token = tokens["access_token"].as_str().expect("an access token");

    // Presented again, the device code is held twice: the grant ends.
    let (status, replayed) = poll(router, &program.client_id, &codes.device_code).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(replayed["error"], "invalid_grant", "body: {replayed}");
    let (status, _headers, _answer) = mcp(router, Some(access_token), &whoami(), &[]).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the replay revoked the grant"
    );
    assert_eq!(
        audited(&harness, anubis::audit::OAUTH_DEVICE_CODE_REUSED).await,
        1
    );

    // The person's user code named one request, decided once.
    let (status, _body) = enter_code(router, Some(&cookie), &codes.user_code).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_refusal_expiry_or_lost_grant_is_what_the_program_reads() {
    let Some(database) = TestDatabase::create("oauth_device_endings").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let email = format!("endings-{}@example.com", Uuid::new_v4());
    let cookie = register(router, &email).await;
    let program = MetadataDocument::serve_device("Server agent");

    // Denied: the browser returns to the code-entry page, the program reads
    // access_denied, and the code names nothing any more.
    let denied = device_codes(router, &program.client_id, "").await;
    let request_id = request_for_code(router, &cookie, &denied.user_code).await;
    let landed = decide(router, &cookie, &request_id, false).await;
    assert_eq!(
        landed.as_str(),
        "http://127.0.0.1:3000/device?result=denied"
    );
    let (status, body) = poll(router, &program.client_id, &denied.device_code).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "access_denied", "body: {body}");
    let (status, _body) = enter_code(router, Some(&cookie), &denied.user_code).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Expired before anybody decided: the program reads expired_token, the
    // person cannot enter the code, and a decision on a request already open
    // finds it gone.
    let lapsed = device_codes(router, &program.client_id, "").await;
    let request_id = request_for_code(router, &cookie, &lapsed.user_code).await;
    expire(&harness, &lapsed.device_code).await;
    let (_status, body) = poll(router, &program.client_id, &lapsed.device_code).await;
    assert_eq!(body["error"], "expired_token", "body: {body}");
    let (status, _body) = enter_code(router, Some(&cookie), &lapsed.user_code).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _headers, _body) = send(
        router,
        "POST",
        &format!("/oauth/requests/{request_id}"),
        Some(&json!({ "approve": true })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // An approval that expires before the program polls is expired too, and
    // the grant it left is never listed: the program never connected.
    let late = device_codes(router, &program.client_id, "").await;
    let request_id = request_for_code(router, &cookie, &late.user_code).await;
    decide(router, &cookie, &request_id, true).await;
    expire(&harness, &late.device_code).await;
    let (_status, body) = poll(router, &program.client_id, &late.device_code).await;
    assert_eq!(body["error"], "expired_token", "body: {body}");
    let (_status, _headers, listed) =
        send(router, "GET", "/oauth/connections", None, Some(&cookie)).await;
    assert_eq!(listed["connections"], json!([]));

    // Approved, and then the person reset their password before the program
    // polled: the reset ended the grant, so there is nothing to collect.
    let reset = device_codes(router, &program.client_id, "").await;
    let request_id = request_for_code(router, &cookie, &reset.user_code).await;
    decide(router, &cookie, &request_id, true).await;
    let mut connection = harness.pool.get().await.expect("a connection");
    let user_id: Uuid = users::table
        .filter(users::email.eq(&email))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the account exists");
    let live: Vec<Uuid> = oauth_grants::table
        .filter(oauth_grants::user_id.eq(user_id))
        .filter(oauth_grants::revoked_at.is_null())
        .select(oauth_grants::id)
        .load(&mut connection)
        .await
        .expect("the grants load");
    assert!(!live.is_empty());
    drop(connection);
    let (status, _headers, _body) = send(
        router,
        "POST",
        "/auth/password-reset/request",
        Some(&json!({ "email": email })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let reset_token = reset_link_token(&harness, &email);
    let (status, _headers, body) = send(
        router,
        "POST",
        "/auth/password-reset/confirm",
        Some(&json!({ "token": reset_token, "password": "a reset new passphrase" })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (_status, body) = poll(router, &program.client_id, &reset.device_code).await;
    assert_eq!(body["error"], "invalid_grant", "body: {body}");
}

/// The token in the most recent password reset email to `email`.
fn reset_link_token(harness: &Harness, email: &str) -> String {
    let message = harness
        .outbox
        .emails()
        .into_iter()
        .rev()
        .find(|sent| sent.to == email && sent.subject.contains("Reset"))
        .expect("the reset email must be in the outbox");
    let (_before, rest) = message
        .text_body
        .split_once("token=")
        .expect("the reset email must carry a link");
    rest.split_whitespace()
        .next()
        .expect("the token ends at whitespace")
        .to_owned()
}

#[tokio::test]
async fn only_a_client_that_declared_the_grant_may_start_one() {
    let Some(database) = TestDatabase::create("oauth_device_clients").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let cookie = register(router, &format!("clients-{}@example.com", Uuid::new_v4())).await;

    // A client of the authorization code flow alone, the way Claude Code's
    // document reads, is not a device client.
    let redirecting = MetadataDocument::serve("Claude Code");
    let (status, refused) = start_device(router, &redirecting.client_id, "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["error"], "unauthorized_client", "body: {refused}");

    // And a device client is not a redirecting one: it registered nowhere to
    // be sent, so the authorization endpoint refuses it before redirecting.
    let device = MetadataDocument::serve_device("Server agent");
    let landed = authorize(router, &authorize_params(&device.client_id, "")).await;
    assert_eq!(landed.path(), "/consent");
    assert_eq!(
        param(&landed, "error").as_deref(),
        Some("invalid_redirect_uri")
    );

    // A document that registers a redirect but declares only the device grant
    // is told, at its redirect, that the code flow is not its to use.
    let both_named = MetadataDocument::serve_with(
        "Device only",
        &[DEVICE_CODE_GRANT],
        &["http://localhost/callback"],
    );
    let landed = authorize(router, &authorize_params(&both_named.client_id, "")).await;
    assert_eq!(
        param(&landed, "error").as_deref(),
        Some("unauthorized_client"),
        "{landed}",
    );

    // Unknown clients, missing ids, and requests this server cannot serve.
    let (status, refused) = start_device(router, "no-such-client", "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(refused["error"], "invalid_client");
    let (status, refused) = post_form(router, "/oauth/device_authorization", &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["error"], "invalid_request");
    let (_status, refused) = start_device(router, &device.client_id, "notes:read admin").await;
    assert_eq!(refused["error"], "invalid_scope");
    let (_status, refused) = post_form(
        router,
        "/oauth/device_authorization",
        &[
            ("client_id", &device.client_id),
            ("resource", "https://evil.example/mcp"),
        ],
    )
    .await;
    assert_eq!(refused["error"], "invalid_target");
    // RFC 8628 section 3.1: a parameter with no value was not sent.
    let (status, codes) = post_form(
        router,
        "/oauth/device_authorization",
        &[
            ("client_id", &device.client_id),
            ("resource", ""),
            ("scope", ""),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {codes}");

    // A registered client declares the grant, needs no redirect URI, and is
    // shown as nobody's word but its own.
    let (status, _headers, registered) = send(
        router,
        "POST",
        "/oauth/register",
        Some(&json!({
            "client_name": "Old agent",
            "grant_types": [DEVICE_CODE_GRANT],
            "token_endpoint_auth_method": "none",
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {registered}");
    assert_eq!(
        registered["grant_types"],
        json!([DEVICE_CODE_GRANT, "refresh_token"])
    );
    assert_eq!(registered["response_types"], json!([]));
    assert_eq!(registered["redirect_uris"], json!([]));
    let client_id = registered["client_id"].as_str().expect("a client id");
    let codes = device_codes(router, client_id, "").await;
    let request_id = request_for_code(router, &cookie, &codes.user_code).await;
    let (_status, _headers, request) = send(
        router,
        "GET",
        &format!("/oauth/requests/{request_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(request["client"]["kind"], "dynamic");
    assert_eq!(request["client"]["verified_host"], Value::Null);
}

#[tokio::test]
async fn code_entry_is_signed_in_and_says_the_same_about_every_unusable_code() {
    let Some(database) = TestDatabase::create("oauth_device_entry").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let email = format!("entry-{}@example.com", Uuid::new_v4());
    let cookie = register(router, &email).await;
    let program = MetadataDocument::serve_device("Server agent");
    let codes = device_codes(router, &program.client_id, "").await;

    // Signed out, nothing is looked up.
    let (status, _body) = enter_code(router, None, &codes.user_code).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A form post is all another site can send without a preflight, so code
    // entry takes JSON alone, and a page elsewhere cannot enter a code in a
    // signed-in visitor's name.
    let forged = router
        .clone()
        .oneshot(
            Request::post("/oauth/device_verification")
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(COOKIE, format!("anubis_session={cookie}"))
                .body(Body::from(format!("user_code={}", codes.user_code)))
                .expect("the request builds"),
        )
        .await
        .expect("the router answers");
    assert_eq!(forged.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // Malformed, never issued, and already decided read identically.
    let decided = device_codes(router, &program.client_id, "").await;
    let request_id = request_for_code(router, &cookie, &decided.user_code).await;
    decide(router, &cookie, &request_id, false).await;
    let mut refusals = Vec::new();
    for entered in ["not a code", "BBBB-BBBB", decided.user_code.as_str()] {
        let (status, body) = enter_code(router, Some(&cookie), entered).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{entered}: {body}");
        refusals.push(body);
    }
    assert!(
        refusals.windows(2).all(|pair| pair[0] == pair[1]),
        "every unusable code reads the same: {refusals:?}",
    );

    // Two browsers entering one code see one request, decided once.
    let second_browser = register(router, &format!("other-{}@example.com", Uuid::new_v4())).await;
    let first = request_for_code(router, &cookie, &codes.user_code).await;
    let second = request_for_code(router, &second_browser, &codes.user_code).await;
    assert_eq!(first, second);
    decide(router, &second_browser, &second, true).await;
    let (status, _headers, _body) = send(
        router,
        "POST",
        &format!("/oauth/requests/{first}"),
        Some(&json!({ "approve": false })),
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the first decision consumed it"
    );
    // The approval bound to the account that made it.
    let (status, tokens) = poll(router, &program.client_id, &codes.device_code).await;
    assert_eq!(status, StatusCode::OK, "body: {tokens}");
    let access_token = tokens["access_token"].as_str().expect("an access token");
    let (_status, _headers, answer) = mcp(router, Some(access_token), &whoami(), &[]).await;
    assert!(
        answer["result"]["structuredContent"]["user"]["email"]
            .as_str()
            .is_some_and(|signed_in| signed_in.starts_with("other-")),
        "body: {answer}",
    );

    // An account on an operator's temporary password cannot enter a code.
    let mut connection = harness.pool.get().await.expect("a connection");
    diesel::update(users::table.filter(users::email.eq(&email)))
        .set(users::password_change_required.eq(true))
        .execute(&mut connection)
        .await
        .expect("the flag sets");
    let fresh = device_codes(router, &program.client_id, "").await;
    let (status, body) = enter_code(router, Some(&cookie), &fresh.user_code).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["code"], anubis::auth::PASSWORD_CHANGE_REQUIRED);
}

#[tokio::test]
async fn a_denied_device_outlives_the_sweep_of_stale_documents() {
    let Some(database) = TestDatabase::create("oauth_device_sweep").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let cookie = register(router, &format!("sweep-{}@example.com", Uuid::new_v4())).await;
    let program = MetadataDocument::serve_device("Server agent");
    let codes = device_codes(router, &program.client_id, "").await;
    let request_id = request_for_code(router, &cookie, &codes.user_code).await;
    decide(router, &cookie, &request_id, false).await;

    // The refusal left neither a request nor a grant holding the client, and
    // its cached document goes stale while the program still polls.
    let mut connection = harness.pool.get().await.expect("a connection");
    diesel::update(oauth_clients::table.filter(oauth_clients::client_id.eq(&program.client_id)))
        .set(oauth_clients::refresh_after.eq(Some(Utc::now() - Duration::hours(1))))
        .execute(&mut connection)
        .await
        .expect("the cache entry goes stale");
    drop(connection);
    let next = MetadataDocument::serve("Next");
    authorize(router, &authorize_params(&next.client_id, "")).await;

    let (_status, body) = poll(router, &program.client_id, &codes.device_code).await;
    assert_eq!(body["error"], "access_denied", "body: {body}");
}

#[tokio::test]
async fn waiting_device_authorizations_are_bounded() {
    let Some(database) = TestDatabase::create("oauth_device_ceiling").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let program = MetadataDocument::serve_device("Server agent");
    device_codes(router, &program.client_id, "").await;

    // Fill the table to the ceiling with authorizations nobody will decide.
    let mut connection = harness.pool.get().await.expect("a connection");
    diesel::sql_query(
        "INSERT INTO oauth_device_codes \
         (client_id, device_code_hash, user_code_hash, interval_seconds, expires_at) \
         SELECT client_id, 'filler-device-' || n, 'filler-user-' || n, 5, now() + interval '10 minutes' \
         FROM oauth_device_codes, generate_series(1, 4999) AS n",
    )
    .execute(&mut connection)
    .await
    .expect("the filler rows insert");
    drop(connection);

    let (status, refused) = start_device(router, &program.client_id, "").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {refused}");
    assert_eq!(refused["error"], "temporarily_unavailable");
}

/// One code entry as `cookie`, arriving from `client`, as a browser would.
async fn enter_from(router: &Router, cookie: &str, user_code: &str, client: &str) -> StatusCode {
    let mut request = Request::post("/oauth/device_verification")
        .header(CONTENT_TYPE, "application/json")
        .header(COOKIE, format!("anubis_session={cookie}"))
        .body(Body::from(
            serde_json::to_vec(&json!({ "user_code": user_code })).expect("the body serializes"),
        ))
        .expect("the request builds");
    let peer: SocketAddr = client.parse().expect("the client address parses");
    request.extensions_mut().insert(ConnectInfo(peer));

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router answers");
    let status = response.status();
    if status == StatusCode::TOO_MANY_REQUESTS {
        assert!(response.headers().contains_key(RETRY_AFTER));
    }
    let _body = response.into_body().collect().await;
    status
}

#[tokio::test]
async fn guessing_user_codes_runs_out_per_account_and_per_address() {
    let Some(database) = TestDatabase::create("oauth_device_guessing").await else {
        return;
    };
    let pool = database.pool().await;
    // Limits on, whatever the environment says: this is the test of them.
    let config = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        _ => None,
    })
    .expect("the test config parses");
    let (mailer, _outbox) = anubis::mail::Mailer::test();
    let rate_limit = anubis::rate_limit::RateLimiter::new(&config.rate_limit);
    let authorization = anubis::oauth_server::Server::new(pool.clone(), &config, Harness::scopes());
    let router = Router::new()
        .nest(
            "/auth",
            anubis::auth::router(pool.clone(), mailer, &config, &rate_limit),
        )
        .merge(anubis::oauth_server::router(&authorization, &rate_limit));
    let quota = anubis::rate_limit::Budget::UserCodePerAccount.quota();
    let period = anubis::rate_limit::Budget::UserCodePerAccount.period();
    let start = Instant::now();

    // One account rotating addresses meets its own budget everywhere. The
    // budget leaks one entry back every `period / quota`, so what it admitted
    // is held to the quota plus what leaked while the test ran.
    let guesser = register(&router, &format!("guesser-{}@example.com", Uuid::new_v4())).await;
    let mut admitted = 0;
    for attempt in 0..quota * 4 {
        let client = format!("198.51.100.{}:41000", attempt % 200 + 1);
        if enter_from(&router, &guesser, "BBBB-BBBB", &client).await
            == StatusCode::TOO_MANY_REQUESTS
        {
            break;
        }
        admitted += 1;
    }
    let leaked = start.elapsed().as_nanos() / (period / quota).as_nanos();
    assert!(
        u128::from(admitted) <= u128::from(quota) + leaked,
        "admitted {admitted} entries from rotating addresses",
    );
    assert!(
        admitted >= quota,
        "a person's own budget is the whole quota"
    );

    // Its refusal does not depend on whether the code was real.
    let program = MetadataDocument::serve_device("Server agent");
    let (status, codes) = start_device(&router, &program.client_id, "").await;
    assert_eq!(status, StatusCode::OK, "body: {codes}");
    let real = codes["user_code"].as_str().expect("a user code");
    assert_eq!(
        enter_from(&router, &guesser, real, "203.0.113.9:41000").await,
        StatusCode::TOO_MANY_REQUESTS,
    );

    // Many accounts behind one address meet the address's budget, though
    // none of them spent its own.
    let mut accounts = Vec::new();
    for _account in 0..4 {
        accounts.push(register(&router, &format!("crowd-{}@example.com", Uuid::new_v4())).await);
    }
    let shared = "203.0.113.50:41000";
    let crowd_size = u32::try_from(accounts.len()).expect("a handful of accounts");
    let mut crowd_admitted = 0_u32;
    let mut refused = false;
    let attempts = usize::try_from(quota * 2).expect("the quota is small");
    for cookie in accounts.iter().cycle().take(attempts) {
        if enter_from(&router, cookie, "BBBB-BBBB", shared).await == StatusCode::TOO_MANY_REQUESTS {
            refused = true;
            break;
        }
        crowd_admitted += 1;
    }
    assert!(refused, "the address budget never ran out");
    let leaked = start.elapsed().as_nanos() / (period / quota).as_nanos();
    assert!(
        u128::from(crowd_admitted) <= u128::from(quota) + leaked,
        "admitted {crowd_admitted} entries from one address",
    );
    assert!(
        crowd_admitted.div_ceil(crowd_size) < quota,
        "no single account spent its own budget",
    );
    // The same accounts are still heard from another address.
    assert_eq!(
        enter_from(&router, &accounts[0], "BBBB-BBBB", "203.0.113.51:41000").await,
        StatusCode::BAD_REQUEST,
    );
}

#[tokio::test]
async fn an_approval_after_expiry_creates_nothing() {
    let Some(database) = TestDatabase::create("oauth_device_late_approval").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let cookie = register(router, &format!("late-{}@example.com", Uuid::new_v4())).await;
    let program = MetadataDocument::serve_device("Server agent");
    let codes = device_codes(router, &program.client_id, "").await;
    let request_id = request_for_code(router, &cookie, &codes.user_code).await;

    // The device authorization lapses while the request is still open: the
    // request has seconds left, the authorization none.
    let mut connection = harness.pool.get().await.expect("a connection");
    diesel::update(
        oauth_device_codes::table
            .filter(oauth_device_codes::device_code_hash.eq(device_code_hash(&codes.device_code))),
    )
    .set(oauth_device_codes::expires_at.eq(Utc::now() - Duration::seconds(1)))
    .execute(&mut connection)
    .await
    .expect("the device code expires");

    let (status, _headers, _body) = send(
        router,
        "POST",
        &format!("/oauth/requests/{request_id}"),
        Some(&json!({ "approve": true })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let grants: i64 = oauth_grants::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("the count runs");
    assert_eq!(grants, 0, "the refused approval rolled its grant back");
    assert_eq!(audited(&harness, anubis::audit::OAUTH_GRANTED).await, 0);
}

/// The RFC 8628 parameter a device client sends is all the token endpoint
/// needs: no redirect URI and no verifier.
#[tokio::test]
async fn a_device_poll_needs_its_device_code() {
    let Some(database) = TestDatabase::create("oauth_device_missing_code").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let program = MetadataDocument::serve_device("Server agent");
    let (status, refused) = post_form(
        &harness.router,
        "/oauth/token",
        &[
            ("grant_type", DEVICE_CODE_GRANT),
            ("client_id", &program.client_id),
            ("resource", RESOURCE),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["error"], "invalid_request");
}

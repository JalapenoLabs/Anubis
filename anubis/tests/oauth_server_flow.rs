//! The authorization server, end to end: discovery, consent, tokens, and endings.
//!
//! The client is played by the test, taking the steps Claude Code takes: it
//! serves a Client ID Metadata Document on a loopback port (which the server
//! fetches, allowed outside production), sends the person's browser to
//! `/oauth/authorize`, and exchanges the code it is handed with its PKCE
//! verifier. Every refusal a stolen or mangled credential should meet is a
//! step in one of these stories.
//!
//! Requires `DATABASE_URL`; without it each test logs a skip and passes. CI
//! always provides one.

mod support;

use anubis::schema::{audit_events, oauth_grants, users};
use axum::http::StatusCode;
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::{Value, json};
use uuid::Uuid;

use support::oauth::{
    MetadataDocument, REDIRECT, RESOURCE, authorize, authorize_params, connect, decide, exchange,
    mcp, param, post_form, refresh,
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

#[tokio::test]
async fn discovery_advertises_what_claude_and_codex_look_for() {
    let Some(database) = TestDatabase::create("oauth_server_discovery").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let (status, _headers, server) = send(
        &harness.router,
        "GET",
        "/.well-known/oauth-authorization-server",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(server["issuer"], "http://127.0.0.1:3000");
    // All four, or both clients fall back to registration.
    assert_eq!(server["client_id_metadata_document_supported"], true);
    assert_eq!(
        server["token_endpoint_auth_methods_supported"],
        json!(["none"])
    );
    assert_eq!(server["code_challenge_methods_supported"], json!(["S256"]));
    assert_eq!(
        server["authorization_response_iss_parameter_supported"],
        true
    );
    assert_eq!(
        server["scopes_supported"],
        json!(["notes:read", "notes:write"])
    );

    for path in [
        "/.well-known/oauth-protected-resource/mcp",
        "/.well-known/oauth-protected-resource",
    ] {
        let (status, _headers, resource) = send(&harness.router, "GET", path, None, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(resource["resource"], RESOURCE, "{path}");
        assert_eq!(
            resource["authorization_servers"],
            json!(["http://127.0.0.1:3000"]),
            "{path}",
        );
    }
}

#[tokio::test]
async fn a_client_connects_calls_a_tool_and_is_audited() {
    let Some(database) = TestDatabase::create("oauth_server_connect").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let cookie = register(router, &format!("connect-{}@example.com", Uuid::new_v4())).await;
    let document = MetadataDocument::serve("Claude Code");

    // The browser arrives at the authorization endpoint and is sent on to the
    // consent screen, carrying nothing but a request id.
    let consent = authorize(router, &authorize_params(&document.client_id, "notes:read")).await;
    assert_eq!(consent.path(), "/consent");
    let request_id = param(&consent, "request").expect("a request id");

    // The screen names the client, the host that vouches for it, where the
    // code will go, and the scope with its description.
    let (status, _headers, request) = send(
        router,
        "GET",
        &format!("/oauth/requests/{request_id}"),
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {request}");
    assert_eq!(request["client"]["name"], "Claude Code");
    assert_eq!(request["client"]["kind"], "metadata_document");
    assert_eq!(request["client"]["verified_host"], "127.0.0.1");
    assert_eq!(request["redirect_host"], "localhost");
    assert_eq!(request["redirect_is_loopback"], true);
    assert_eq!(request["scopes"][0]["name"], "notes:read");
    assert_eq!(request["scopes"][0]["description"], "Read your notes");

    // Signed out, the screen learns nothing.
    let (status, _headers, _body) = send(
        router,
        "GET",
        &format!("/oauth/requests/{request_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Approving delivers a code to the port the client bound, with its state
    // and this server's issuer.
    let callback = decide(router, &cookie, &request_id, true).await;
    assert_eq!(callback.as_str().split('?').next(), Some(REDIRECT));
    assert_eq!(param(&callback, "state").as_deref(), Some("client-state"));
    assert_eq!(
        param(&callback, "iss").as_deref(),
        Some("http://127.0.0.1:3000")
    );
    let code = param(&callback, "code").expect("a code");

    // The request was consumed by the decision.
    let (status, _headers, _body) = send(
        router,
        "POST",
        &format!("/oauth/requests/{request_id}"),
        Some(&json!({ "approve": true })),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, tokens) = exchange(router, &document.client_id, &code).await;
    assert_eq!(status, StatusCode::OK, "body: {tokens}");
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(tokens["expires_in"], 3600);
    assert_eq!(tokens["scope"], "notes:read");
    let access_token = tokens["access_token"].as_str().expect("an access token");

    let (status, _headers, answer) = mcp(router, Some(access_token), &whoami(), &[]).await;
    assert_eq!(status, StatusCode::OK, "body: {answer}");
    let identity = &answer["result"]["structuredContent"];
    assert!(
        identity["user"]["email"]
            .as_str()
            .is_some_and(|email| email.starts_with("connect-")),
        "whoami names the person: {answer}",
    );
    assert_eq!(identity["client"], "Claude Code");
    assert_eq!(identity["scopes"], json!(["notes:read"]));

    assert_eq!(audited(&harness, anubis::audit::OAUTH_GRANTED).await, 1);
}

#[tokio::test]
async fn a_stolen_or_mangled_code_is_refused() {
    let Some(database) = TestDatabase::create("oauth_server_codes").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let cookie = register(router, &format!("codes-{}@example.com", Uuid::new_v4())).await;
    let document = MetadataDocument::serve("Claude Code");

    // A wrong verifier spends the code and gets nothing for it.
    let consent = authorize(router, &authorize_params(&document.client_id, "")).await;
    let callback = decide(
        router,
        &cookie,
        &param(&consent, "request").expect("a request id"),
        true,
    )
    .await;
    let code = param(&callback, "code").expect("a code");
    let (status, refused) = post_form(
        router,
        "/oauth/token",
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &document.client_id),
            ("code", &code),
            ("redirect_uri", REDIRECT),
            ("code_verifier", &"x".repeat(43)),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["error"], "invalid_grant");

    // Another client cannot exchange a code issued to this one.
    let consent = authorize(router, &authorize_params(&document.client_id, "")).await;
    let callback = decide(
        router,
        &cookie,
        &param(&consent, "request").expect("a request id"),
        true,
    )
    .await;
    let code = param(&callback, "code").expect("a code");
    let (status, refused) = exchange(router, "someone-else", &code).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["error"], "invalid_grant");

    // A code exchanged once and replayed revokes everything it produced.
    let consent = authorize(router, &authorize_params(&document.client_id, "")).await;
    let callback = decide(
        router,
        &cookie,
        &param(&consent, "request").expect("a request id"),
        true,
    )
    .await;
    let code = param(&callback, "code").expect("a code");
    let (status, tokens) = exchange(router, &document.client_id, &code).await;
    assert_eq!(status, StatusCode::OK, "body: {tokens}");
    let access_token = tokens["access_token"].as_str().expect("an access token");
    let (status, _headers, _answer) = mcp(router, Some(access_token), &whoami(), &[]).await;
    assert_eq!(status, StatusCode::OK);

    let (status, replayed) = exchange(router, &document.client_id, &code).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(replayed["error"], "invalid_grant");
    let (status, headers, _answer) = mcp(router, Some(access_token), &whoami(), &[]).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the replay revoked the grant"
    );
    assert!(
        headers["www-authenticate"]
            .to_str()
            .expect("a visible header")
            .contains("error=\"invalid_token\""),
    );
    assert_eq!(audited(&harness, anubis::audit::OAUTH_CODE_REUSED).await, 1);
}

#[tokio::test]
async fn a_reused_refresh_token_revokes_the_family() {
    let Some(database) = TestDatabase::create("oauth_server_refresh").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let cookie = register(router, &format!("refresh-{}@example.com", Uuid::new_v4())).await;
    let document = MetadataDocument::serve("Codex");
    let first = connect(
        router,
        &cookie,
        &document.client_id,
        "notes:read notes:write",
    )
    .await;

    // A refresh may narrow, never widen.
    let (status, widened) = post_form(
        router,
        "/oauth/token",
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &document.client_id),
            ("refresh_token", &first.refresh_token),
            ("scope", "notes:read admin"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(widened["error"], "invalid_scope");

    // Asking for too much did not spend the token. Asking for less narrows
    // the access token and rotates the refresh token.
    let (status, rotated) = post_form(
        router,
        "/oauth/token",
        &[
            ("grant_type", "refresh_token"),
            ("client_id", &document.client_id),
            ("refresh_token", &first.refresh_token),
            ("scope", "notes:read"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {rotated}");
    assert_eq!(rotated["scope"], "notes:read");
    assert_ne!(rotated["refresh_token"], first.refresh_token.as_str());
    let second_access = rotated["access_token"].as_str().expect("an access token");
    let (status, _headers, _answer) = mcp(router, Some(second_access), &whoami(), &[]).await;
    assert_eq!(status, StatusCode::OK);

    // The rotated-away token, presented again, ends the whole connection.
    let (status, reused) = refresh(router, &document.client_id, &first.refresh_token).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(reused["error"], "invalid_grant");
    let (status, _headers, _answer) = mcp(router, Some(second_access), &whoami(), &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let newest = rotated["refresh_token"].as_str().expect("a refresh token");
    let (status, _body) = refresh(router, &document.client_id, newest).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "the newest token died too");
    assert_eq!(
        audited(&harness, anubis::audit::OAUTH_REFRESH_REUSED).await,
        1
    );
}

#[tokio::test]
async fn requests_this_server_will_not_serve_are_refused_where_they_should_be() {
    let Some(database) = TestDatabase::create("oauth_server_refusals").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let cookie = register(router, &format!("refusals-{}@example.com", Uuid::new_v4())).await;
    let document = MetadataDocument::serve("Claude Code");

    // Before the redirect URI is trusted, the browser stays here.
    let mut params = authorize_params(&document.client_id, "");
    params[2].1 = "http://localhost:53682/elsewhere".to_owned();
    let landed = authorize(router, &params).await;
    assert_eq!(landed.path(), "/consent");
    assert_eq!(
        param(&landed, "error").as_deref(),
        Some("invalid_redirect_uri")
    );

    let unknown = authorize(router, &authorize_params("no-such-client", "")).await;
    assert_eq!(param(&unknown, "error").as_deref(), Some("invalid_client"));

    // After it is trusted, the client hears why, with its state and our iss.
    // Each case replaces one parameter of a valid request, by position:
    // resource, scope, challenge method, response type.
    let cases: [(usize, &str, &str); 4] = [
        (7, "https://evil.example/mcp", "invalid_target"),
        (3, "notes:read admin", "invalid_scope"),
        (6, "plain", "invalid_request"),
        (0, "token", "unsupported_response_type"),
    ];
    for (index, value, expected) in cases {
        let mut params = authorize_params(&document.client_id, "notes:read");
        params[index].1 = value.to_owned();
        let landed = authorize(router, &params).await;
        assert!(
            landed.as_str().starts_with(REDIRECT),
            "{expected}: {landed}"
        );
        assert_eq!(param(&landed, "error").as_deref(), Some(expected));
        assert_eq!(param(&landed, "state").as_deref(), Some("client-state"));
        assert_eq!(
            param(&landed, "iss").as_deref(),
            Some("http://127.0.0.1:3000")
        );
    }

    // A token request naming another resource is refused too.
    let consent = authorize(router, &authorize_params(&document.client_id, "")).await;
    let callback = decide(
        router,
        &cookie,
        &param(&consent, "request").expect("a request id"),
        true,
    )
    .await;
    let code = param(&callback, "code").expect("a code");
    let (status, refused) = post_form(
        router,
        "/oauth/token",
        &[
            ("grant_type", "authorization_code"),
            ("client_id", &document.client_id),
            ("code", &code),
            ("redirect_uri", REDIRECT),
            ("code_verifier", support::oauth::VERIFIER),
            ("resource", "https://evil.example/mcp"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["error"], "invalid_target");

    // Denying sends the client away with access_denied and no code.
    let consent = authorize(router, &authorize_params(&document.client_id, "")).await;
    let denied = decide(
        router,
        &cookie,
        &param(&consent, "request").expect("a request id"),
        false,
    )
    .await;
    assert_eq!(param(&denied, "error").as_deref(), Some("access_denied"));
    assert_eq!(param(&denied, "code"), None);
}

#[tokio::test]
async fn dynamic_registration_is_bounded_and_works() {
    let Some(database) = TestDatabase::create("oauth_server_registration").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let cookie = register(router, &format!("dcr-{}@example.com", Uuid::new_v4())).await;

    let (status, _headers, refused) = send(
        router,
        "POST",
        "/oauth/register",
        Some(&json!({ "redirect_uris": ["http://evil.example/callback"] })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["error"], "invalid_redirect_uri");

    let (status, _headers, refused) = send(
        router,
        "POST",
        "/oauth/register",
        Some(&json!({
            "redirect_uris": ["http://localhost/callback"],
            "token_endpoint_auth_method": "client_secret_post",
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["error"], "invalid_client_metadata");

    let (status, _headers, registered) = send(
        router,
        "POST",
        "/oauth/register",
        Some(&json!({
            "client_name": "Old client",
            "redirect_uris": ["http://localhost/callback"],
            "grant_types": ["authorization_code", "refresh_token"],
            "token_endpoint_auth_method": "none",
            "application_type": "native",
        })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {registered}");
    assert_eq!(registered["token_endpoint_auth_method"], "none");
    let client_id = registered["client_id"].as_str().expect("a client id");
    assert!(!client_id.starts_with("https://"));

    // A registered client consents and connects like any other, and the
    // consent screen does not claim anybody vouched for its name.
    let consent = authorize(router, &authorize_params(client_id, "")).await;
    let request_id = param(&consent, "request").expect("a request id");
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
    let callback = decide(router, &cookie, &request_id, true).await;
    let (status, _tokens) = exchange(
        router,
        client_id,
        &param(&callback, "code").expect("a code"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_person_sees_and_revokes_their_connections() {
    let Some(database) = TestDatabase::create("oauth_server_connections").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let owner = register(router, &format!("owner-{}@example.com", Uuid::new_v4())).await;
    let stranger = register(router, &format!("stranger-{}@example.com", Uuid::new_v4())).await;
    let document = MetadataDocument::serve("Claude Code");

    // Approved, but the program never came back for its tokens: not a
    // connection, so not listed.
    let abandoned = authorize(router, &authorize_params(&document.client_id, "")).await;
    let _callback = decide(
        router,
        &owner,
        &param(&abandoned, "request").expect("a request id"),
        true,
    )
    .await;

    let connection = connect(router, &owner, &document.client_id, "notes:read").await;

    let (status, _headers, listed) =
        send(router, "GET", "/oauth/connections", None, Some(&owner)).await;
    assert_eq!(status, StatusCode::OK);
    let connections = listed["connections"].as_array().expect("a list");
    assert_eq!(connections.len(), 1);
    assert_eq!(connections[0]["client"]["name"], "Claude Code");
    assert_eq!(connections[0]["scopes"][0]["name"], "notes:read");
    let grant_id = connections[0]["id"].as_str().expect("an id").to_owned();

    // Nobody else sees it or can end it.
    let (_status, _headers, theirs) =
        send(router, "GET", "/oauth/connections", None, Some(&stranger)).await;
    assert_eq!(theirs["connections"], json!([]));
    let (status, _headers, _body) = send(
        router,
        "DELETE",
        &format!("/oauth/connections/{grant_id}"),
        None,
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _headers, _body) = send(
        router,
        "DELETE",
        &format!("/oauth/connections/{grant_id}"),
        None,
        Some(&owner),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _headers, _answer) =
        mcp(router, Some(&connection.access_token), &whoami(), &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "revocation is immediate");
    let (status, _body) = refresh(router, &document.client_id, &connection.refresh_token).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_status, _headers, listed) =
        send(router, "GET", "/oauth/connections", None, Some(&owner)).await;
    assert_eq!(listed["connections"], json!([]));
    assert_eq!(audited(&harness, anubis::audit::OAUTH_REVOKED).await, 1);

    // The row stays, marked, so the account and the log agree on when.
    let mut db = harness.pool.get().await.expect("a connection");
    let revoked_at: Option<chrono::DateTime<chrono::Utc>> = oauth_grants::table
        .filter(oauth_grants::id.eq(grant_id.parse::<Uuid>().expect("a uuid")))
        .select(oauth_grants::revoked_at)
        .first(&mut db)
        .await
        .expect("the grant row remains");
    assert!(revoked_at.is_some());
}

#[tokio::test]
async fn a_client_can_revoke_its_own_connection() {
    let Some(database) = TestDatabase::create("oauth_server_revocation").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let cookie = register(router, &format!("revoke-{}@example.com", Uuid::new_v4())).await;
    let document = MetadataDocument::serve("Codex");
    let connection = connect(router, &cookie, &document.client_id, "").await;

    // Another client naming the token does nothing, and hears the same 200.
    let (status, _body) = post_form(
        router,
        "/oauth/revoke",
        &[
            ("token", &connection.refresh_token),
            ("client_id", "someone-else"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _headers, _answer) =
        mcp(router, Some(&connection.access_token), &whoami(), &[]).await;
    assert_eq!(status, StatusCode::OK);

    // The owner revoking its refresh token ends the connection.
    let (status, _body) = post_form(
        router,
        "/oauth/revoke",
        &[
            ("token", &connection.refresh_token),
            ("client_id", &document.client_id),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _headers, _answer) =
        mcp(router, Some(&connection.access_token), &whoami(), &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(audited(&harness, anubis::audit::OAUTH_REVOKED).await, 1);
}

/// Sets or clears the flag an operator's temporary password leaves on `email`.
async fn flag_temporary_password(harness: &Harness, email: &str, required: bool) {
    let mut connection = harness.pool.get().await.expect("a connection");
    let updated = diesel::update(users::table.filter(users::email.eq(email)))
        .set(users::password_change_required.eq(required))
        .execute(&mut connection)
        .await
        .expect("the flag updates");
    assert_eq!(updated, 1, "exactly one account carries {email}");
}

#[tokio::test]
async fn an_account_on_a_temporary_password_can_neither_use_nor_approve_a_client() {
    let Some(database) = TestDatabase::create("oauth_server_temporary_password").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let email = format!("rescued-{}@example.com", Uuid::new_v4());
    let cookie = register(router, &email).await;
    let document = MetadataDocument::serve("Claude Code");
    let connection = connect(router, &cookie, &document.client_id, "notes:read").await;

    let (status, _headers, answer) =
        mcp(router, Some(&connection.access_token), &whoami(), &[]).await;
    assert_eq!(status, StatusCode::OK, "body: {answer}");

    // An operator rescues the account. The connected client is refused with
    // the same 403 and code a session gets, and no challenge, because the
    // token is valid and signing in again would not help.
    flag_temporary_password(&harness, &email, true).await;
    let (status, headers, body) = mcp(router, Some(&connection.access_token), &whoami(), &[]).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert_eq!(body["code"], anubis::auth::PASSWORD_CHANGE_REQUIRED);
    assert!(headers.get("www-authenticate").is_none());

    // An unknown token still reads 401 and says nothing about any account.
    let (status, _headers, _body) = mcp(router, Some("not-a-token"), &whoami(), &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The consent screen cannot approve a new client for the account either.
    let second = MetadataDocument::serve("Codex");
    let consent = authorize(router, &authorize_params(&second.client_id, "notes:read")).await;
    let request_id = param(&consent, "request").expect("a request id");
    for (method, body) in [("GET", None), ("POST", Some(json!({ "approve": true })))] {
        let (status, _headers, answer) = send(
            router,
            method,
            &format!("/oauth/requests/{request_id}"),
            body.as_ref(),
            Some(&cookie),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} body: {answer}");
        assert_eq!(answer["code"], anubis::auth::PASSWORD_CHANGE_REQUIRED);
    }
    let mut db = harness.pool.get().await.expect("a connection");
    let grants: i64 = oauth_grants::table
        .count()
        .get_result(&mut db)
        .await
        .expect("the count runs");
    assert_eq!(grants, 1, "the refused approval created no grant");

    // The grant survived the rescue: once the person chooses a password, the
    // same token works again, which proves the refusal was the flag.
    flag_temporary_password(&harness, &email, false).await;
    let (status, _headers, answer) =
        mcp(router, Some(&connection.access_token), &whoami(), &[]).await;
    assert_eq!(status, StatusCode::OK, "body: {answer}");
}

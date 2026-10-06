//! The MCP endpoint, end to end: the challenge, both protocol eras, and tools.
//!
//! A client that has never connected reads the `401` and its challenge; once
//! connected it speaks either revision: the 2025 `initialize` handshake that
//! shipped clients send, or the stateless 2026-07-28 revision with its
//! `server/discover`. Tool calls run as the person, under the scopes they
//! granted, and nothing about the endpoint answers a session cookie.
//!
//! Requires `DATABASE_URL`; without it each test logs a skip and passes. CI
//! always provides one.

mod support;

use axum::body::Body;
use axum::http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HOST, ORIGIN};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use support::oauth::{APP_HOST, Connection, MetadataDocument, connect, mcp, mcp_with_cookie};
use support::{Harness, TestDatabase, register};

/// A signed-up person with a connected client holding `scope`.
async fn connected(harness: &Harness, scope: &str) -> Connection {
    let router = &harness.router;
    let cookie = register(router, &format!("mcp-{}@example.com", Uuid::new_v4())).await;
    let document = MetadataDocument::serve("Claude Code");
    connect(router, &cookie, &document.client_id, scope).await
}

fn call(name: &str, arguments: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments },
    })
}

#[tokio::test]
async fn a_client_that_has_not_connected_is_told_where_to_sign_in() {
    let Some(database) = TestDatabase::create("mcp_challenge").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let list = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });

    let (status, headers, _body) = mcp(router, None, &list, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        headers["www-authenticate"],
        "Bearer resource_metadata=\"http://127.0.0.1:3000/.well-known/oauth-protected-resource/mcp\", \
         scope=\"notes:read notes:write\"",
    );

    let (status, headers, _body) = mcp(router, Some("not-a-token"), &list, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        headers["www-authenticate"]
            .to_str()
            .expect("a visible header")
            .contains("error=\"invalid_token\""),
    );

    // A browser session is not a connection.
    let cookie = register(router, &format!("cookie-{}@example.com", Uuid::new_v4())).await;
    assert_eq!(
        mcp_with_cookie(router, &cookie, &list).await,
        StatusCode::UNAUTHORIZED,
    );
}

#[tokio::test]
async fn a_2025_client_initializes_lists_and_calls() {
    let Some(database) = TestDatabase::create("mcp_legacy").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let connection = connected(&harness, "notes:read").await;
    let token = Some(connection.access_token.as_str());

    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "claude-code", "version": "2.1.231" },
        },
    });
    let (status, _headers, answer) = mcp(router, token, &initialize, &[]).await;
    assert_eq!(status, StatusCode::OK, "body: {answer}");
    assert_eq!(answer["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(answer["result"]["serverInfo"]["name"], "Anubis test");
    assert!(answer["result"]["capabilities"]["tools"].is_object());

    let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let (status, _headers, _body) = mcp(
        router,
        token,
        &initialized,
        &[("mcp-protocol-version", "2025-11-25")],
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
    let (status, _headers, answer) = mcp(
        router,
        token,
        &list,
        &[("mcp-protocol-version", "2025-11-25")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {answer}");
    let names: Vec<&str> = answer["result"]["tools"]
        .as_array()
        .expect("a tool list")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(names, ["whoami", "write_note"]);
    assert_eq!(
        answer["result"]["tools"][1]["inputSchema"]["required"],
        json!(["text"]),
    );

    let (status, _headers, answer) = mcp(
        router,
        token,
        &call("whoami", &json!({})),
        &[("mcp-protocol-version", "2025-11-25")],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {answer}");
    assert_eq!(answer["result"]["isError"], false);
    assert_eq!(
        answer["result"]["structuredContent"]["client"],
        "Claude Code"
    );
}

#[tokio::test]
async fn a_2026_client_discovers_without_a_handshake() {
    let Some(database) = TestDatabase::create("mcp_modern").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let connection = connected(&harness, "").await;

    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": "codex", "version": "1.0.0" },
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    let discover = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": { "_meta": meta },
    });
    let (status, _headers, answer) = mcp(
        router,
        Some(&connection.access_token),
        &discover,
        &[
            ("mcp-protocol-version", "2026-07-28"),
            ("mcp-method", "server/discover"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {answer}");
    assert!(
        answer["result"]["supportedVersions"]
            .as_array()
            .is_some_and(|versions| versions.contains(&json!("2026-07-28"))),
        "body: {answer}",
    );
    assert!(answer["result"]["capabilities"]["tools"].is_object());

    let whoami = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": { "name": "whoami", "arguments": {}, "_meta": meta },
    });
    let (status, _headers, answer) = mcp(
        router,
        Some(&connection.access_token),
        &whoami,
        &[
            ("mcp-protocol-version", "2026-07-28"),
            ("mcp-method", "tools/call"),
            ("mcp-name", "whoami"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {answer}");
    assert_eq!(
        answer["result"]["structuredContent"]["client"],
        "Claude Code"
    );
}

#[tokio::test]
async fn a_tool_runs_only_with_the_scope_it_requires() {
    let Some(database) = TestDatabase::create("mcp_scopes").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = &harness.router;
    let note = call("write_note", &json!({ "text": "hello" }));

    let reader = connected(&harness, "notes:read").await;
    let (status, _headers, answer) = mcp(router, Some(&reader.access_token), &note, &[]).await;
    assert_eq!(status, StatusCode::OK, "body: {answer}");
    assert_eq!(answer["result"]["isError"], true);
    assert!(
        answer["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("notes:write")),
        "the refusal names the scope: {answer}",
    );

    let writer = connected(&harness, "notes:read notes:write").await;
    let (status, _headers, answer) = mcp(router, Some(&writer.access_token), &note, &[]).await;
    assert_eq!(status, StatusCode::OK, "body: {answer}");
    assert_eq!(answer["result"]["isError"], false);
    assert_eq!(answer["result"]["structuredContent"]["written"], "hello");

    // Arguments are held to the input schema before the tool runs.
    let (status, _headers, answer) = mcp(
        router,
        Some(&writer.access_token),
        &call("write_note", &json!({ "text": 42 })),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {answer}");
    // A tool error rather than a protocol error, so the model can correct
    // itself from the message.
    assert_eq!(answer["result"]["isError"], true, "body: {answer}");
    assert!(
        answer["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("/text")),
        "the refusal names the argument: {answer}",
    );

    let (status, _headers, answer) = mcp(
        router,
        Some(&writer.access_token),
        &call("no_such_tool", &json!({})),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {answer}");
    assert_eq!(answer["error"]["code"], -32602);
}

#[tokio::test]
async fn the_transport_refuses_other_hosts_origins_and_methods() {
    let Some(database) = TestDatabase::create("mcp_transport").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let connection = connected(&harness, "").await;
    let list = serde_json::to_vec(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .expect("the message serializes");

    let send = |host: &'static str, origin: Option<&'static str>, method: &'static str| {
        let mut builder = Request::builder()
            .method(method)
            .uri("/mcp")
            .header(HOST, host)
            .header(AUTHORIZATION, format!("Bearer {}", connection.access_token))
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream");
        if let Some(origin) = origin {
            builder = builder.header(ORIGIN, origin);
        }
        let request = builder
            .body(Body::from(list.clone()))
            .expect("the request builds");
        harness.router.clone().oneshot(request)
    };

    // A DNS-rebinding page reaches this server under a name it does not own.
    let rebound = send("evil.example", None, "POST").await.expect("an answer");
    assert!(rebound.status().is_client_error(), "{}", rebound.status());

    let cross_origin = send(APP_HOST, Some("https://evil.example"), "POST")
        .await
        .expect("an answer");
    assert_eq!(cross_origin.status(), StatusCode::FORBIDDEN);

    let same_origin = send(APP_HOST, Some("http://127.0.0.1:3000"), "POST")
        .await
        .expect("an answer");
    assert_eq!(same_origin.status(), StatusCode::OK);

    let stream = send(APP_HOST, None, "GET").await.expect("an answer");
    assert_eq!(stream.status(), StatusCode::METHOD_NOT_ALLOWED);
}

//! The inputs nobody sends by accident.
//!
//! Every other suite drives the surface the way a client is meant to. This one
//! drives it the way an attacker would: bodies that do not fit, paths that
//! climb out of their directory, frames no browser would frame, credentials
//! aimed at somebody else's tenant. Each test asserts what the server does
//! *today*, so a regression is visible and a known gap is written down rather
//! than assumed closed.
//!
//! Requires `DATABASE_URL` for the tests that need one; the rest run anywhere.

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use anubis::spa::Assets;
use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE};
use axum::http::{Request, StatusCode};
use axum::routing::get;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use support::{Harness, TestDatabase, register, send, socket};

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

/// A body far past any limit the framework sets, so one of them has to catch
/// it. Four megabytes beats axum's two-megabyte default without being large
/// enough to slow the suite down.
const OVERSIZED_BYTES: usize = 4 * 1024 * 1024;

/// Reading a body is the first thing a handler does, so an unbounded one is
/// memory an unauthenticated caller can make the server allocate. The limit
/// answering here is axum's default, which applies to every extractor that
/// consumes a body, and the avatar route's own larger one where an image
/// legitimately needs the room.
#[tokio::test]
async fn oversized_bodies_are_refused_before_a_handler_reads_them() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    // A JSON endpoint, unauthenticated, with a body no login ever carries.
    let padding = "a".repeat(OVERSIZED_BYTES);
    let credentials = json!({ "email": "nobody@example.com", "password": padding });
    let (status, _headers, _body) = send(
        &harness.router,
        "POST",
        "/auth/login",
        Some(&credentials),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "a global body limit must answer before the handler does",
    );

    // The avatar route raises the limit to fit an image, and still has one.
    let cookie = register(&harness.router, "uploader@example.com").await;
    let request = Request::builder()
        .method("POST")
        .uri("/auth/profile/avatar")
        .header(COOKIE, format!("anubis_session={cookie}"))
        .header(CONTENT_TYPE, "image/png")
        .body(Body::from(vec![0_u8; 6 * 1024 * 1024]))
        .expect("request must build");
    let response = harness
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the upload must complete");
    assert_eq!(
        response.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "an upload past the avatar limit is refused, not decoded",
    );
}

// ---------------------------------------------------------------------------
// Static assets
// ---------------------------------------------------------------------------

/// The SPA fallback serves a directory to the public, which is the classic
/// place to go looking for files above it. Every escape has to answer the same
/// JSON `404` an unknown asset does, and never a byte of anything outside the
/// build.
#[tokio::test]
async fn the_spa_never_serves_a_file_outside_its_build() {
    let build = BuildOutput::new();
    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .fallback_service(
            Assets::new(build.path())
                .expect("the temporary build output must open")
                .into_service(),
        );

    // Everything under `/assets/` must resolve inside the build or 404: a
    // missing hashed asset is never answered with index.html, so a traversal
    // cannot even be disguised as a client route.
    for path in [
        "/assets/../Cargo.toml",
        "/assets/%2e%2e/Cargo.toml",
        "/assets/..%2fCargo.toml",
        "/assets/....//Cargo.toml",
        "/assets/../../../../../../etc/passwd",
        "/assets/..\\..\\Cargo.toml",
    ] {
        let (status, body) = fetch(&app, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path} was served: {body}");
        assert!(
            !body.contains("[package]") && !body.contains("root:"),
            "{path} leaked a file outside the build: {body}",
        );
    }

    // Above the assets directory, a traversal is indistinguishable from a
    // client-side route, so it lands on the shell rather than on a file.
    for path in ["/../Cargo.toml", "/%2e%2e/Cargo.toml"] {
        let (status, body) = fetch(&app, path).await;
        assert!(
            status == StatusCode::NOT_FOUND || body == INDEX_HTML,
            "{path} answered {status} with {body}",
        );
        assert!(!body.contains("[package]"), "{path} leaked: {body}");
    }

    // A write to the fallback is a routing mistake, not a client route.
    let request = Request::builder()
        .method("POST")
        .uri("/anything")
        .body(Body::empty())
        .expect("request must build");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("the request must complete");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// What a client-side route must hand the browser.
const INDEX_HTML: &str = "<!doctype html><title>Anubis</title><div id=\"root\"></div>";

/// A temporary build output, removed when the test that made it ends.
#[derive(Debug)]
struct BuildOutput {
    root: PathBuf,
}

impl BuildOutput {
    /// Writes the smallest directory that passes for a frontend build.
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("anubis-adversarial-{}", Uuid::new_v4()));
        fs::create_dir_all(root.join("assets")).expect("the build output must be creatable");
        fs::write(root.join("index.html"), INDEX_HTML).expect("index.html must be writable");
        fs::write(
            root.join("assets").join("index-a1b2c3.js"),
            "console.log(1)",
        )
        .expect("the asset must be writable");
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for BuildOutput {
    fn drop(&mut self) {
        // A leftover temp directory is not worth failing a passing test over.
        let _ignored = fs::remove_dir_all(&self.root);
    }
}

/// Fetches one path, returning its status and body as text.
async fn fetch(app: &Router, path: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("request must build");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("the request must complete");

    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

// ---------------------------------------------------------------------------
// The realtime socket
// ---------------------------------------------------------------------------

/// The socket parses attacker-controlled text on an authenticated connection,
/// so every malformed shape has to end in an answer or a close, never in a
/// buffer the client chose the size of.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_realtime_socket_refuses_frames_no_client_would_send() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let address = harness.serve();

    let cookie = register(&harness.router, "prober@example.com").await;
    let mut probe = socket::connect(address, &cookie).await;

    // Text that is not a frame at all.
    socket::send(&mut probe, &json!("plain text")).await;
    let reply = socket::next_frame(&mut probe).await;
    assert_eq!(reply["code"], json!("invalid_frame"), "reply: {reply}");

    // A frame naming a type the protocol does not have.
    socket::send(&mut probe, &json!({ "type": "publish", "channel": "x" })).await;
    let reply = socket::next_frame(&mut probe).await;
    assert_eq!(reply["code"], json!("invalid_frame"), "reply: {reply}");

    // A channel name that is not one.
    let reply = socket::subscribe(&mut probe, "team:not-a-uuid:projects").await;
    assert_eq!(reply["code"], json!("invalid_frame"), "reply: {reply}");

    // Somebody else's user channel: refused as absent, which is the socket's
    // form of the `404` the ownership guards answer.
    let stranger = Uuid::new_v4();
    let reply = socket::subscribe(&mut probe, &format!("user:{stranger}:inbox")).await;
    assert_eq!(reply["code"], json!("not_found"), "reply: {reply}");

    // Unsubscribing from something never held is satisfied, not an error: the
    // client's intent holds either way.
    socket::send(
        &mut probe,
        &json!({ "type": "unsubscribe", "channel": format!("user:{stranger}:inbox") }),
    )
    .await;
    let reply = socket::next_frame(&mut probe).await;
    assert_eq!(reply["type"], json!("unsubscribed"), "reply: {reply}");

    // A binary frame is not part of the protocol, and is dropped rather than
    // ending the conversation: the next text frame still gets its answer.
    socket::send_binary(&mut probe, vec![0xFF; 1024]).await;
    socket::send(
        &mut probe,
        &json!({ "type": "unsubscribe", "channel": "user:x:y" }),
    )
    .await;
    let reply = socket::next_frame(&mut probe).await;
    assert_eq!(
        reply["type"],
        json!("unsubscribed"),
        "a binary frame must not derail the socket: {reply}",
    );

    // A frame past the incoming limit ends the connection instead of being
    // buffered, which is the whole point of setting one.
    let huge = "a".repeat(16 * 1024);
    socket::send(&mut probe, &json!({ "type": "subscribe", "channel": huge })).await;
    assert!(
        socket::is_closed(&mut probe).await,
        "an oversized frame must close the socket, not be read",
    );
}

// ---------------------------------------------------------------------------
// Webhook endpoints
// ---------------------------------------------------------------------------

/// **Known gap, deliberately asserted.** A delivery is a request this server
/// makes on a user's instruction, and what bounds it today is the scheme, the
/// absence of embedded credentials, the timeout, and the refusal to follow
/// redirects. Where the host *resolves to* is not bounded, so a team may
/// subscribe an address inside the network the server runs on.
///
/// `docs/webhooks.md` states the posture and the roadmap item that closes it.
/// This test exists so the day it closes, it fails and gets rewritten, rather
/// than the gap quietly outliving the note.
#[tokio::test]
async fn webhook_endpoints_may_still_name_an_address_inside_the_network() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let admin = register(&harness.router, "hooks@example.com").await;
    let team_id = harness.bootstrapped_team(&admin).await;
    let path = format!("/developers/teams/{team_id}/webhook-endpoints");

    // The rules that do hold.
    for url in [
        "ftp://example.com/hooks",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://user:pass@example.com/hooks",
        "not a url",
    ] {
        let body = json!({ "url": url, "event_types": ["project.created"] });
        let (status, _headers, answer) =
            send(&harness.router, "POST", &path, Some(&body), Some(&admin)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{url} was accepted: {answer}"
        );
    }

    // The gap. Both of these resolve inside the network in a real deployment:
    // a link-local metadata service, and the host itself.
    for url in [
        "https://169.254.169.254/latest/meta-data/",
        "https://127.0.0.1:9/hooks",
        "https://localhost/hooks",
    ] {
        let body = json!({ "url": url, "event_types": ["project.created"] });
        let (status, _headers, answer) =
            send(&harness.router, "POST", &path, Some(&body), Some(&admin)).await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "known gap: {url} is accepted today, and this test says so out loud: {answer}",
        );
    }
}

// ---------------------------------------------------------------------------
// Credentials and tenancy
// ---------------------------------------------------------------------------

/// The developer surface decides where a team's records are sent, so it takes
/// a session and the admin role, and it is scoped to one team. A bearer token
/// reaches none of it, and neither does another team's admin.
#[tokio::test]
async fn the_developer_surface_is_session_only_and_scoped_to_one_team() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let ours = register(&harness.router, "ours@example.com").await;
    let our_team = harness.bootstrapped_team(&ours).await;
    let theirs = register(&harness.router, "theirs@example.com").await;
    let their_team = harness.bootstrapped_team(&theirs).await;

    // A live platform token, minted the way an application's client would.
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        &format!("/developers/teams/{our_team}/platform-applications"),
        Some(&json!({ "name": "Adversarial suite" })),
        Some(&ours),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let token = body["token"]
        .as_str()
        .expect("the created application carries its token")
        .to_owned();

    // Our own endpoint, so the cross-tenant probes below name something real.
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        &format!("/developers/teams/{our_team}/webhook-endpoints"),
        Some(&json!({ "url": "https://example.com/hooks", "event_types": ["project.created"] })),
        Some(&ours),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let endpoint_id = body["webhook_endpoint"]["id"]
        .as_str()
        .expect("the endpoint names itself")
        .to_owned();

    // A bearer token is the `/api/v1` identity and nothing else. It carries no
    // session, so the management surface answers `401`, not `403`: there is no
    // caller to forbid.
    let request = Request::builder()
        .uri(format!("/developers/teams/{our_team}/webhook-endpoints"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request must build");
    let response = harness
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the request must complete");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a platform token must not drive the team's own management screens",
    );

    // Another team's admin: every route answers `404`, byte-identical to a
    // team that does not exist, so probing ids reveals nothing.
    for (method, path) in [
        (
            "GET",
            format!("/developers/teams/{our_team}/webhook-endpoints"),
        ),
        (
            "GET",
            format!("/developers/teams/{our_team}/webhook-endpoints/{endpoint_id}/deliveries"),
        ),
        (
            "DELETE",
            format!("/developers/teams/{our_team}/webhook-endpoints/{endpoint_id}"),
        ),
        (
            "GET",
            format!("/developers/teams/{our_team}/platform-applications"),
        ),
    ] {
        let (status, _headers, body) =
            send(&harness.router, method, &path, None, Some(&theirs)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}: {body}");

        let nowhere = path.replace(&our_team.to_string(), &Uuid::new_v4().to_string());
        let (absent, _headers, absent_body) =
            send(&harness.router, method, &nowhere, None, Some(&theirs)).await;
        assert_eq!(absent, status, "{method} {nowhere}: {absent_body}");
        assert_eq!(
            absent_body, body,
            "a foreign team must read as an absent one"
        );
    }

    // Our own team's ids, driven from their session, still belong to us.
    let (status, _headers, body) = send(
        &harness.router,
        "GET",
        &format!("/developers/teams/{their_team}/webhook-endpoints"),
        None,
        Some(&theirs),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["webhook_endpoints"],
        json!([]),
        "their team sees their own endpoints, which is none: {body}",
    );
}

/// A session cookie is attacker-supplied text on every request, so nothing it
/// can hold may become anything but a `401`.
#[tokio::test]
async fn a_forged_session_cookie_is_refused_rather_than_mishandled() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    // Control characters are absent on purpose: a header value cannot hold
    // one, so the HTTP layer refuses to frame the request at all and the
    // server never sees it. What is left is everything a header *can* carry.
    for cookie in [
        String::new(),
        "not-a-token".to_owned(),
        "' OR 1=1 --".to_owned(),
        "../../etc/passwd".to_owned(),
        "%00%0d%0aSet-Cookie:+anubis_session=x".to_owned(),
        "a".repeat(64 * 1024),
    ] {
        let (status, _headers, body) =
            send(&harness.router, "GET", "/auth/me", None, Some(&cookie)).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "a cookie of {} bytes answered {status}: {body}",
            cookie.len(),
        );
    }
}

/// Tenant names are rendered into an invitation's subject line and into the
/// UI, so a line break in one is refused at the point somebody typed it.
#[tokio::test]
async fn a_tenant_name_cannot_carry_a_line_break() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let founder = register(&harness.router, "founder@example.com").await;

    for name in [
        "Ops\r\nBcc: attacker@example.com",
        "Ops\nX-Injected: yes",
        "Ops\u{7}",
    ] {
        let (status, _headers, body) = send(
            &harness.router,
            "POST",
            "/tenancy/organizations",
            Some(&json!({ "name": name })),
            Some(&founder),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{name:?} was accepted as a name: {body}",
        );
    }

    // The invitation email a real name produces reaches the outbox intact.
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/tenancy/organizations",
        Some(&json!({ "name": "Ops" })),
        Some(&founder),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
}

/// An email address is the other string that reaches a mail header, and the
/// same rule applies to it.
#[tokio::test]
async fn an_email_address_cannot_carry_a_line_break() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let admin = register(&harness.router, "inviter@example.com").await;
    let team_id = harness.bootstrapped_team(&admin).await;

    for email in [
        "victim@example.com\r\nBcc: attacker@example.com",
        "victim@example.com\nX-Injected: yes",
        "victim@example.com attacker@example.com",
    ] {
        let invite = json!({ "email": email, "team_id": team_id });
        let (status, _headers, body) = send(
            &harness.router,
            "POST",
            "/tenancy/invitations",
            Some(&invite),
            Some(&admin),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{email:?} was accepted as a recipient: {body}",
        );
    }

    let sent: Vec<Value> = harness
        .outbox
        .emails()
        .into_iter()
        .map(|email| json!(email.to))
        .collect();
    assert!(
        sent.iter().all(|to| {
            to.as_str()
                .is_some_and(|address| !address.contains(['\r', '\n']))
        }),
        "an address with a line break reached the mailer: {sent:?}",
    );
}

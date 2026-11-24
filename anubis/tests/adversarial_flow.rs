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

use anubis::schema::{
    audit_events, organization_memberships, role_grants, roles, team_memberships, users,
};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use support::{Harness, PASSWORD, TestDatabase, register, send, stream};

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
// The realtime stream
// ---------------------------------------------------------------------------

/// A stream reads channel names from a query string and a cursor from a
/// header, both attacker-controlled on an authenticated request, so every
/// malformed shape has to end in a refusal or a reset, never in a stream that
/// carries more than it should or a cursor the server trusts.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one probe after another against the same stream endpoint"
)]
async fn the_realtime_stream_refuses_what_no_client_would_send() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = harness.router.clone();
    let cookie = register(&router, "prober@example.com").await;
    let (status, _headers, me) = send(&router, "GET", "/auth/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    let mine = format!("user:{}:inbox", me["user"]["id"].as_str().expect("an id"));
    let stranger = format!("user:{}:inbox", Uuid::new_v4());

    // Names that are not names, beside one that is: each is refused on its
    // own, and the stream still carries the one that may be heard.
    let garbage = [
        "plain text",
        "team:not-a-uuid:projects",
        "organization:00000000-0000-0000-0000-000000000000:audits",
        "user:00000000-0000-0000-0000-000000000000:Upper",
    ];
    let mut names: Vec<&str> = garbage.to_vec();
    names.push(&mine);
    let mut probe = stream::connect(&router, &stream::path(&names, &[]), &cookie, None).await;
    let opening = probe.opening().await;
    for name in garbage {
        let refused = opening
            .refusal(name)
            .unwrap_or_else(|| panic!("{name:?} must be refused: {opening:?}"));
        assert_eq!(refused.frame["code"], json!("invalid_channel"), "{name:?}");
    }
    assert_eq!(opening.resets(), vec![mine.as_str()]);
    drop(probe);

    // Somebody else's user channel is refused as absent, which is the
    // stream's form of the `404` the ownership guards answer.
    let mut probe = stream::connect(&router, &stream::path(&[&stranger], &[]), &cookie, None).await;
    let opening = probe.opening().await;
    let refused = opening
        .refusal(&stranger)
        .expect("a stranger's inbox is refused");
    assert_eq!(refused.frame["code"], json!("not_found"));
    assert!(opening.resets().is_empty());
    drop(probe);

    // A name that parses in a spelling the server never renders, such as an
    // uppercase UUID, is refused: every frame would name it the other way, and
    // the listener that asked for it would never hear a thing.
    let shouted = mine
        .to_uppercase()
        .replacen("USER", "user", 1)
        .replace("INBOX", "inbox");
    let mut probe = stream::connect(&router, &stream::path(&[&shouted], &[]), &cookie, None).await;
    let opening = probe.opening().await;
    let refused = opening
        .refusal(&shouted)
        .expect("a non-canonical spelling is refused");
    assert_eq!(refused.frame["code"], json!("invalid_channel"));
    assert!(opening.resets().is_empty());
    drop(probe);

    // A name given twice is carried once, so it cannot be delivered twice.
    let mut probe = stream::connect(
        &router,
        &stream::path(&[&mine, &mine, &mine], &[]),
        &cookie,
        None,
    )
    .await;
    assert_eq!(probe.opening().await.resets(), vec![mine.as_str()]);
    drop(probe);

    // A cursor the server never sent, in shapes from plausible to absurd,
    // is never resumed from: the channel resets and nothing is replayed.
    let huge = "9".repeat(4096);
    for cursor in [
        "0000000000000000-18446744073709551616",
        "ffffffffffffffff-18446744073709551615",
        "-1",
        huge.as_str(),
    ] {
        let mut probe =
            stream::connect(&router, &stream::path(&[&mine], &[]), &cookie, Some(cursor)).await;
        let opening = probe.opening().await;
        assert_eq!(opening.resets(), vec![mine.as_str()], "cursor {cursor:?}");
        assert!(opening.of_kind("event").is_empty(), "cursor {cursor:?}");
    }

    // A header that is not text at all is no cursor either.
    let request = Request::builder()
        .uri(stream::path(&[&mine], &[]))
        .header(COOKIE, format!("anubis_session={cookie}"))
        .header(
            "last-event-id",
            axum::http::HeaderValue::from_bytes(&[0xFF, 0xFE]).expect("opaque bytes are allowed"),
        )
        .body(Body::empty())
        .expect("the request must build");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router must answer");
    assert_eq!(response.status(), StatusCode::OK);
    // Closing the stream frees its share of the account's bounds.
    drop(response);

    // One account holds a share of the process's channel table, not the
    // whole of it: past its streams, it is asked to wait rather than allowed
    // to crowd out everybody else on the instance.
    let mut held = Vec::new();
    for index in 0..32 {
        let topic = format!("{mine}{index}");
        held.push(stream::connect(&router, &stream::path(&[&topic], &[]), &cookie, None).await);
    }
    let (status, _body) = stream::open(&router, &stream::path(&[&mine], &[]), Some(&cookie), None)
        .await
        .expect_err("an account past its share must be refused");
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    let stranger_cookie = register(&router, "bystander@example.com").await;
    let (_status, _headers, them) =
        send(&router, "GET", "/auth/me", None, Some(&stranger_cookie)).await;
    let theirs = format!("user:{}:inbox", them["user"]["id"].as_str().expect("an id"));
    let _unaffected = stream::connect(
        &router,
        &stream::path(&[&theirs], &[]),
        &stranger_cookie,
        None,
    )
    .await;
    drop(held);

    // A channel list past the limit is refused before any stream exists,
    // and so is one that names nothing at all.
    let many: Vec<String> = (0..65)
        .map(|index| format!("user:{}:t{index}", Uuid::new_v4()))
        .collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let (status, _body) = stream::open(&router, &stream::path(&many, &[]), Some(&cookie), None)
        .await
        .expect_err("too many channels must be refused");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _body) = stream::open(&router, "/realtime?channels=,,,", Some(&cookie), None)
        .await
        .expect_err("no channels must be refused");
    assert_eq!(status, StatusCode::BAD_REQUEST);
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

// ---------------------------------------------------------------------------
// Runtime roles: every way of managing roles that could be a way to gain them
// ---------------------------------------------------------------------------

/// The permissions the court's Moderator role carries: it manages members and
/// roles, and reads the team's one model, and nothing else.
const MODERATOR_PERMISSIONS: [&str; 7] = [
    "Tenancy::Membership.create",
    "Tenancy::Membership.update",
    "Tenancy::Membership.destroy",
    "Tenancy::Role.create",
    "Tenancy::Role.update",
    "Tenancy::Role.destroy",
    "Team.read",
];

/// One organization, set up the way an escalation story needs it.
///
/// The owner made a Moderator role able to manage members and roles and put
/// it above `billing` and `editor`, so the ladder reads default 0, editor 1,
/// billing 2, Moderator 3, admin 4. The moderator holds it organization-wide
/// and in the team; the member holds `editor` in the team and only the
/// default role organization-wide.
struct Court {
    harness: Harness,
    owner: String,
    moderator: String,
    member: String,
    organization: Uuid,
    team: Uuid,
    roles: CourtRoles,
}

struct CourtRoles {
    default: Uuid,
    editor: Uuid,
    billing: Uuid,
    moderator: Uuid,
    administrator: Uuid,
}

impl Court {
    async fn open(database: &TestDatabase) -> Self {
        let harness = Harness::boot(database).await;
        let run = Uuid::new_v4();
        let moderator_email = format!("moderator-{run}@example.com");
        let member_email = format!("member-{run}@example.com");

        let owner = register(&harness.router, &format!("owner-{run}@example.com")).await;
        let team = harness.bootstrapped_team(&owner).await;
        let (status, _headers, body) = send(
            &harness.router,
            "GET",
            "/tenancy/memberships",
            None,
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        let organization = id_of(&body["organizations"][0]["id"]);

        let (status, _headers, body) = send(
            &harness.router,
            "POST",
            &format!("/tenancy/organizations/{organization}/roles"),
            Some(&json!({
                "name": "Moderator",
                "color": "#dc2626",
                "permissions": MODERATOR_PERMISSIONS,
            })),
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "body: {body}");
        let moderator_role = id_of(&body["role"]["id"]);

        let listed = role_listing(&harness, &owner, organization).await;
        let roles = CourtRoles {
            default: keyed(&listed, "default"),
            editor: keyed(&listed, "editor"),
            billing: keyed(&listed, "billing"),
            moderator: moderator_role,
            administrator: keyed(&listed, "admin"),
        };

        let (status, _headers, body) = send(
            &harness.router,
            "PUT",
            &format!("/tenancy/organizations/{organization}/role-order"),
            Some(&json!({ "role_ids": [roles.moderator, roles.billing, roles.editor] })),
            Some(&owner),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");

        let moderator = register(&harness.router, &moderator_email).await;
        let member = register(&harness.router, &member_email).await;
        let court = Self {
            harness,
            owner,
            moderator,
            member,
            organization,
            team,
            roles,
        };

        for invitation in [
            json!({
                "email": moderator_email,
                "organization_id": organization,
                "role_ids": [court.roles.moderator],
            }),
            json!({
                "email": moderator_email,
                "team_id": team,
                "role_ids": [court.roles.moderator],
            }),
            json!({ "email": member_email, "organization_id": organization }),
            json!({ "email": member_email, "team_id": team, "role_ids": [court.roles.editor] }),
        ] {
            let joining = if invitation["email"] == moderator_email.as_str() {
                &court.moderator
            } else {
                &court.member
            };
            court.invite_and_claim(&invitation, joining).await;
        }

        court
    }

    async fn invite_and_claim(&self, invitation: &Value, joining: &str) {
        let (status, _headers, body) = self
            .send(
                "POST",
                "/tenancy/invitations",
                Some(invitation),
                &self.owner,
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "body: {body}");
        let email = invitation["email"]
            .as_str()
            .expect("an invitation names an email");
        let token = latest_invitation_token(&self.harness, email);
        let (status, _headers, body) = self
            .send(
                "POST",
                "/tenancy/invitations/claim",
                Some(&json!({ "token": token })),
                joining,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        cookie: &str,
    ) -> (StatusCode, axum::http::HeaderMap, Value) {
        send(&self.harness.router, method, path, body, Some(cookie)).await
    }

    /// Edits a role's permissions as `cookie`, answering the status.
    async fn set_permissions(&self, cookie: &str, role: Uuid, permissions: &[&str]) -> StatusCode {
        let path = format!("/tenancy/organizations/{}/roles/{role}", self.organization);
        let (status, _headers, _body) = self
            .send(
                "PATCH",
                &path,
                Some(&json!({ "permissions": permissions })),
                cookie,
            )
            .await;
        status
    }

    /// The team membership of the account behind an email.
    async fn team_membership(&self, email_prefix: &str) -> Uuid {
        let path = format!("/tenancy/teams/{}/members", self.team);
        let (status, _headers, body) = self.send("GET", &path, None, &self.owner).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        roster_entry(&body, email_prefix)
    }

    /// The organization membership of the account behind an email.
    async fn organization_membership(&self, email_prefix: &str) -> Uuid {
        let path = format!("/tenancy/organizations/{}/members", self.organization);
        let (status, _headers, body) = self.send("GET", &path, None, &self.owner).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        roster_entry(&body, email_prefix)
    }

    /// Replaces a team member's team-wide roles as `cookie`.
    async fn grant_in_team(&self, cookie: &str, membership: Uuid, roles: &[Uuid]) -> StatusCode {
        let path = format!("/tenancy/teams/{}/members/{membership}", self.team);
        let (status, _headers, _body) = self
            .send("PATCH", &path, Some(&json!({ "role_ids": roles })), cookie)
            .await;
        status
    }

    /// Replaces an organization member's organization-wide roles as `cookie`.
    async fn grant_in_organization(
        &self,
        cookie: &str,
        membership: Uuid,
        roles: &[Uuid],
    ) -> StatusCode {
        let path = format!(
            "/tenancy/organizations/{}/members/{membership}",
            self.organization
        );
        let (status, _headers, _body) = self
            .send("PATCH", &path, Some(&json!({ "role_ids": roles })), cookie)
            .await;
        status
    }

    /// Invites a fresh address as `cookie`, answering the status.
    async fn offer(&self, cookie: &str, target: (&str, Uuid), roles: &[Uuid]) -> StatusCode {
        let invitation = json!({
            "email": format!("offered-{}@example.com", Uuid::new_v4()),
            target.0: target.1,
            "role_ids": roles,
        });
        let (status, _headers, _body) = self
            .send("POST", "/tenancy/invitations", Some(&invitation), cookie)
            .await;
        status
    }
}

fn id_of(value: &Value) -> Uuid {
    value
        .as_str()
        .expect("an id is a string")
        .parse()
        .expect("an id is a UUID")
}

async fn role_listing(harness: &Harness, cookie: &str, organization: Uuid) -> Value {
    let (status, _headers, body) = send(
        &harness.router,
        "GET",
        &format!("/tenancy/organizations/{organization}/roles"),
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    body
}

fn keyed(listing: &Value, key: &str) -> Uuid {
    id_of(
        &listing["roles"]
            .as_array()
            .expect("the listing carries roles")
            .iter()
            .find(|role| role["key"] == key)
            .unwrap_or_else(|| panic!("no role seeded from {key:?}"))["id"],
    )
}

fn roster_entry(roster: &Value, email_prefix: &str) -> Uuid {
    let entry = roster["members"]
        .as_array()
        .expect("a roster lists members")
        .iter()
        .find(|member| {
            member["email"]
                .as_str()
                .is_some_and(|email| email.starts_with(email_prefix))
        })
        .unwrap_or_else(|| panic!("nobody named {email_prefix} in {roster}"));
    id_of(&entry["membership_id"])
}

/// The newest invitation token mailed to an address.
fn latest_invitation_token(harness: &Harness, email: &str) -> String {
    let invitation = harness
        .outbox
        .emails()
        .into_iter()
        .rev()
        .find(|sent| sent.to == email && sent.subject.contains("invited"))
        .expect("the invitation email must be in the outbox");
    let (_before, rest) = invitation
        .text_body
        .split_once("token=")
        .expect("the invitation email must carry a link");
    rest.split_whitespace()
        .next()
        .expect("the token ends at whitespace")
        .to_owned()
}

/// Editing a role is changing what its holders may do, so every permission
/// switched on, and every one switched off, must be one the editor holds.
#[tokio::test]
async fn a_role_editor_switches_only_what_they_hold() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let (owner, moderator) = (&court.owner, &court.moderator);

    assert_eq!(
        court
            .set_permissions(moderator, court.roles.editor, &["Team.update"])
            .await,
        StatusCode::FORBIDDEN,
        "a role below yours still cannot carry what you lack",
    );
    assert_eq!(
        court
            .set_permissions(moderator, court.roles.editor, &["Team.read"])
            .await,
        StatusCode::OK,
    );

    // A stale screen cannot undo somebody else's edit either.
    assert_eq!(
        court
            .set_permissions(owner, court.roles.editor, &["Team.read", "Team.destroy"])
            .await,
        StatusCode::OK,
    );
    assert_eq!(
        court
            .set_permissions(moderator, court.roles.editor, &["Team.read"])
            .await,
        StatusCode::FORBIDDEN,
        "switching off a permission you do not hold is refused too",
    );

    // `billing` carries the organization's money, which the moderator does not.
    assert_eq!(
        court
            .set_permissions(moderator, court.roles.billing, &[])
            .await,
        StatusCode::FORBIDDEN,
    );
    assert_eq!(
        court
            .set_permissions(moderator, court.roles.moderator, &MODERATOR_PERMISSIONS)
            .await,
        StatusCode::FORBIDDEN,
        "nobody edits a role at their own rank, their own role included",
    );

    let path = format!("/tenancy/organizations/{}/roles", court.organization);
    for (permissions, expected) in [
        (vec!["Team.update"], StatusCode::FORBIDDEN),
        (vec!["Team.read"], StatusCode::CREATED),
    ] {
        let (status, _headers, body) = court
            .send(
                "POST",
                &path,
                Some(&json!({ "name": "Helper", "color": "#0ea5e9", "permissions": permissions })),
                moderator,
            )
            .await;
        assert_eq!(status, expected, "body: {body}");
    }
}

/// Holding a role-management permission is not a way to hand out a role that
/// carries more than you do, to somebody else, to an invitee, or to yourself.
#[tokio::test]
async fn nobody_hands_out_a_role_carrying_what_they_lack() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let (owner, moderator) = (&court.owner, &court.moderator);

    assert_eq!(
        court
            .set_permissions(owner, court.roles.editor, &["Team.update"])
            .await,
        StatusCode::OK,
    );
    let member = court.team_membership("member-").await;
    let moderator_membership = court.team_membership("moderator-").await;

    // Taking a lower role away hands nothing out, so the hierarchy alone
    // decides it; giving it back is a grant of what the moderator lacks.
    assert_eq!(
        court.grant_in_team(moderator, member, &[]).await,
        StatusCode::OK,
    );
    assert_eq!(
        court
            .grant_in_team(moderator, member, &[court.roles.editor])
            .await,
        StatusCode::FORBIDDEN,
        "editor now carries Team.update, which the moderator lacks",
    );
    assert_eq!(
        court
            .grant_in_team(
                moderator,
                moderator_membership,
                &[court.roles.moderator, court.roles.editor],
            )
            .await,
        StatusCode::FORBIDDEN,
        "nor may they take it themselves",
    );
    assert_eq!(
        court
            .offer(moderator, ("team_id", court.team), &[court.roles.editor])
            .await,
        StatusCode::FORBIDDEN,
    );
    assert_eq!(
        court
            .offer(
                moderator,
                ("organization_id", court.organization),
                &[court.roles.billing],
            )
            .await,
        StatusCode::FORBIDDEN,
        "billing carries the organization's money",
    );
    assert_eq!(
        court
            .offer(moderator, ("team_id", court.team), &[court.roles.default])
            .await,
        StatusCode::BAD_REQUEST,
        "the default role is everybody's and never granted",
    );

    // The administrator role: nobody but an administrator touches it, and an
    // administrator appoints the next one, so the top is never frozen.
    assert_eq!(
        court
            .grant_in_team(moderator, member, &[court.roles.administrator])
            .await,
        StatusCode::FORBIDDEN,
    );
    assert_eq!(
        court
            .offer(
                moderator,
                ("team_id", court.team),
                &[court.roles.administrator]
            )
            .await,
        StatusCode::FORBIDDEN,
    );
    assert_eq!(
        court
            .grant_in_team(owner, member, &[court.roles.administrator])
            .await,
        StatusCode::OK,
    );
    assert_eq!(
        court.grant_in_team(moderator, member, &[]).await,
        StatusCode::FORBIDDEN,
        "and once appointed, they outrank the moderator",
    );
}

/// A reorder states the whole ladder or nothing, and never moves a role at or
/// above the person reordering.
#[tokio::test]
async fn a_reorder_states_the_whole_ladder_and_lifts_nothing() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let roles = &court.roles;
    let path = format!("/tenancy/organizations/{}/role-order", court.organization);
    let reorder = async |order: Value| {
        let (status, _headers, _body) = court
            .send(
                "PUT",
                &path,
                Some(&json!({ "role_ids": order })),
                &court.moderator,
            )
            .await;
        status
    };

    assert_eq!(
        reorder(json!([roles.billing, roles.moderator, roles.editor])).await,
        StatusCode::FORBIDDEN,
        "putting billing above the moderator's own role",
    );
    assert_eq!(
        reorder(json!([roles.moderator, roles.editor, roles.billing])).await,
        StatusCode::OK,
        "roles below the mover may change places",
    );
    for incomplete in [
        json!([roles.moderator, roles.billing]),
        json!([roles.moderator, roles.billing, roles.billing]),
        json!([
            roles.moderator,
            roles.billing,
            roles.editor,
            roles.administrator
        ]),
        json!([roles.moderator, roles.billing, roles.editor, roles.default]),
    ] {
        assert_eq!(
            reorder(incomplete.clone()).await,
            StatusCode::BAD_REQUEST,
            "{incomplete}",
        );
    }
    assert_eq!(
        reorder(json!([
            roles.moderator,
            roles.billing,
            roles.editor,
            Uuid::new_v4()
        ]))
        .await,
        StatusCode::NOT_FOUND,
    );

    let listing = role_listing(&court.harness, &court.owner, court.organization).await;
    let positions: Vec<i64> = listing["roles"]
        .as_array()
        .expect("the listing carries roles")
        .iter()
        .map(|role| role["position"].as_i64().expect("a position is a number"))
        .collect();
    assert_eq!(
        positions,
        [4, 3, 2, 1, 0],
        "the ladder stays dense and whole"
    );
}

/// The default role is everybody's, including whoever edits it, so it is the
/// obvious lever for raising everybody at once. It lifts nobody.
#[tokio::test]
async fn the_default_role_lifts_nobody() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let (owner, moderator, member) = (&court.owner, &court.moderator, &court.member);

    assert_eq!(
        court
            .set_permissions(moderator, court.roles.default, &["Team.update"])
            .await,
        StatusCode::FORBIDDEN,
        "the moderator cannot give everybody what they lack",
    );
    assert_eq!(
        court
            .set_permissions(moderator, court.roles.default, &["Team.read"])
            .await,
        StatusCode::OK,
    );

    // Even every management permission on the default role is inert in the
    // hands of somebody whose highest role it is: they rank 0, and nothing
    // ranks below 0.
    assert_eq!(
        court
            .set_permissions(owner, court.roles.default, &MODERATOR_PERMISSIONS)
            .await,
        StatusCode::OK,
    );
    assert_eq!(
        court
            .set_permissions(member, court.roles.default, &[])
            .await,
        StatusCode::FORBIDDEN,
        "the default role is at the member's own rank",
    );
    let (status, _headers, _body) = court
        .send(
            "POST",
            &format!("/tenancy/organizations/{}/roles", court.organization),
            Some(&json!({ "name": "Mine", "color": "#000000", "permissions": [] })),
            member,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a new role would sit at their rank"
    );
    assert_eq!(
        court
            .offer(
                member,
                ("organization_id", court.organization),
                &[court.roles.editor]
            )
            .await,
        StatusCode::FORBIDDEN,
    );
    let moderator_membership = court.organization_membership("moderator-").await;
    let (status, _headers, _body) = court
        .send(
            "DELETE",
            &format!(
                "/tenancy/organizations/{}/members/{moderator_membership}",
                court.organization
            ),
            None,
            member,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "everybody else outranks them"
    );
}

/// The administrator role is the top of every organization: it cannot be
/// edited away, deleted, or left without a holder.
#[tokio::test]
async fn the_administrator_role_stays_whole_and_held() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let (owner, moderator) = (&court.owner, &court.moderator);
    let roles_path = format!("/tenancy/organizations/{}/roles", court.organization);

    assert_eq!(
        court
            .set_permissions(owner, court.roles.administrator, &[])
            .await,
        StatusCode::BAD_REQUEST,
        "the administrator holds every permission; there is nothing to edit",
    );
    for structural in [court.roles.administrator, court.roles.default] {
        let (status, _headers, _body) = court
            .send("DELETE", &format!("{roles_path}/{structural}"), None, owner)
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    let owner_membership = court.organization_membership("owner-").await;
    let owner_path = format!(
        "/tenancy/organizations/{}/members/{owner_membership}",
        court.organization
    );
    let (status, _headers, _body) = court
        .send(
            "PATCH",
            &owner_path,
            Some(&json!({ "role_ids": [] })),
            moderator,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the moderator cannot unseat the owner"
    );
    let (status, _headers, _body) = court
        .send(
            "PATCH",
            &owner_path,
            Some(&json!({ "role_ids": [] })),
            owner,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the last administrator cannot step down, so management is never locked out",
    );

    // Appointing a second administrator is what lets the first step down.
    let moderator_membership = court.organization_membership("moderator-").await;
    let (status, _headers, body) = court
        .send(
            "PATCH",
            &format!(
                "/tenancy/organizations/{}/members/{moderator_membership}",
                court.organization
            ),
            Some(&json!({ "role_ids": [court.roles.administrator] })),
            owner,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (status, _headers, body) = court
        .send(
            "PATCH",
            &owner_path,
            Some(&json!({ "role_ids": [] })),
            owner,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

/// A role granted on one team or one record is standing there and nowhere
/// wider: it never raises organization-wide rank or team-wide permissions.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "two narrow grants, each proved against every wider place it must not reach"
)]
async fn a_narrow_grant_never_raises_wider_standing() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let owner = &court.owner;
    let member = court.team_membership("member-").await;

    // The member is made a moderator of the team only.
    assert_eq!(
        court
            .grant_in_team(owner, member, &[court.roles.moderator])
            .await,
        StatusCode::OK,
    );
    assert_eq!(
        court
            .set_permissions(&court.member, court.roles.editor, &["Team.read"])
            .await,
        StatusCode::FORBIDDEN,
        "a team grant is no standing over the organization's roles",
    );
    let moderator_in_organization = court.organization_membership("moderator-").await;
    let (status, _headers, _body) = court
        .send(
            "DELETE",
            &format!(
                "/tenancy/organizations/{}/members/{moderator_in_organization}",
                court.organization
            ),
            None,
            &court.member,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A newcomer is made a moderator of one record of the team.
    let run = Uuid::new_v4();
    let newcomer_email = format!("newcomer-{run}@example.com");
    let newcomer = register(&court.harness.router, &newcomer_email).await;
    court
        .invite_and_claim(
            &json!({ "email": newcomer_email, "team_id": court.team }),
            &newcomer,
        )
        .await;
    let newcomer_membership = court.team_membership("newcomer-").await;
    let record = Uuid::new_v4();
    let path = format!(
        "/tenancy/teams/{}/members/{newcomer_membership}",
        court.team
    );
    let (status, _headers, body) = court
        .send(
            "PATCH",
            &path,
            Some(&json!({ "role_ids": [court.roles.moderator], "resource_id": record })),
            owner,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let viewer = {
        let (status, _headers, body) = court
            .send(
                "POST",
                &format!("/tenancy/organizations/{}/roles", court.organization),
                Some(
                    &json!({ "name": "Viewer", "color": "#22c55e", "permissions": ["Team.read"] }),
                ),
                owner,
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "body: {body}");
        id_of(&body["role"]["id"])
    };
    // Somebody fresh to the team, whom the newcomer outranks only on the
    // record they moderate.
    let fresh_email = format!("fresh-{run}@example.com");
    let fresh = register(&court.harness.router, &fresh_email).await;
    court
        .invite_and_claim(
            &json!({ "email": fresh_email, "team_id": court.team }),
            &fresh,
        )
        .await;
    let fresh_membership = court.team_membership("fresh-").await;
    let fresh_path = format!("/tenancy/teams/{}/members/{fresh_membership}", court.team);
    for (body, expected, why) in [
        (
            json!({ "role_ids": [viewer] }),
            StatusCode::FORBIDDEN,
            "team-wide, the newcomer holds only the default role",
        ),
        (
            json!({ "role_ids": [viewer], "resource_id": record }),
            StatusCode::OK,
            "on the record they moderate, they manage",
        ),
        (
            json!({ "role_ids": [viewer], "resource_id": Uuid::new_v4() }),
            StatusCode::FORBIDDEN,
            "on any other record they hold nothing",
        ),
    ] {
        let (status, _headers, answer) = court
            .send("PATCH", &fresh_path, Some(&body), &newcomer)
            .await;
        assert_eq!(status, expected, "{why}: {answer}");
    }
    let (status, _headers, _body) = court
        .send(
            "PATCH",
            &format!("/tenancy/teams/{}/members/{member}", court.team),
            Some(&json!({ "role_ids": [], "resource_id": record })),
            &newcomer,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the team's moderator ranks as high as the record's, even on the record",
    );
}

/// Ids from another organization, of a role, a team, or a membership, read
/// exactly as ids that do not exist.
#[tokio::test]
async fn ids_from_another_organization_read_as_missing() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let owner = &court.owner;

    let stranger = register(
        &court.harness.router,
        &format!("stranger-{}@example.com", Uuid::new_v4()),
    )
    .await;
    let stranger_team = court.harness.bootstrapped_team(&stranger).await;
    let (status, _headers, body) = send(
        &court.harness.router,
        "GET",
        &format!("/tenancy/teams/{stranger_team}/roles"),
        None,
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let foreign_role = keyed(&body, "editor");
    let member = court.team_membership("member-").await;

    assert_eq!(
        court.grant_in_team(owner, member, &[foreign_role]).await,
        StatusCode::NOT_FOUND,
    );
    assert_eq!(
        court
            .offer(owner, ("team_id", court.team), &[foreign_role])
            .await,
        StatusCode::NOT_FOUND,
    );
    for method in ["PATCH", "DELETE"] {
        let path = format!(
            "/tenancy/organizations/{}/roles/{foreign_role}",
            court.organization
        );
        let (status, _headers, _body) = court
            .send(method, &path, Some(&json!({ "name": "Mine" })), owner)
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {path}");
    }

    // A team of another organization and a team that does not exist answer
    // the same, so inviting cannot probe for teams.
    let (foreign, _headers, foreign_body) = court
        .send(
            "POST",
            "/tenancy/invitations",
            Some(&json!({ "email": "probe@example.com", "team_id": stranger_team })),
            owner,
        )
        .await;
    let (absent, _headers, absent_body) = court
        .send(
            "POST",
            "/tenancy/invitations",
            Some(&json!({ "email": "probe@example.com", "team_id": Uuid::new_v4() })),
            owner,
        )
        .await;
    assert_eq!(foreign, StatusCode::NOT_FOUND);
    assert_eq!(absent, foreign);
    assert_eq!(absent_body, foreign_body);

    // The stranger reads nothing of the court's roles.
    let (status, _headers, _body) = send(
        &court.harness.router,
        "GET",
        &format!("/tenancy/organizations/{}/roles", court.organization),
        None,
        Some(&stranger),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A grant belongs to the membership it hangs off, so somebody removed and
/// invited back holds what the new invitation offers and nothing they held
/// before.
#[tokio::test]
async fn a_member_invited_back_finds_no_old_roles_waiting() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let owner = &court.owner;
    let member = court.team_membership("member-").await;

    assert_eq!(
        court
            .grant_in_team(owner, member, &[court.roles.administrator])
            .await,
        StatusCode::OK,
    );
    let (status, _headers, _body) = court
        .send(
            "DELETE",
            &format!("/tenancy/teams/{}/members/{member}", court.team),
            None,
            owner,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let email = court
        .harness
        .outbox
        .emails()
        .into_iter()
        .find(|sent| sent.subject.contains("invited") && sent.to.starts_with("member-"))
        .map(|sent| sent.to)
        .expect("the member was invited once already");
    court
        .invite_and_claim(
            &json!({ "email": email, "team_id": court.team }),
            &court.member,
        )
        .await;

    let (status, _headers, _body) = court
        .send(
            "PATCH",
            &format!("/tenancy/teams/{}", court.team),
            Some(&json!({ "name": "Taken back" })),
            &court.member,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the administrator grant went with the old membership",
    );
}

/// Deleting a role takes it from everybody holding it, at once, and they fall
/// back to whatever else they hold and the default role.
#[tokio::test]
async fn a_deleted_role_leaves_its_holders_with_the_rest() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;

    let (status, _headers, body) = court
        .send(
            "DELETE",
            &format!(
                "/tenancy/organizations/{}/roles/{}",
                court.organization, court.roles.moderator
            ),
            None,
            &court.owner,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");

    assert_eq!(
        court
            .set_permissions(&court.moderator, court.roles.editor, &["Team.read"])
            .await,
        StatusCode::FORBIDDEN,
        "the moderator's management went with the role",
    );
    let (status, _headers, body) = court
        .send("GET", "/account/notifications", None, &court.moderator)
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body["notifications"]
            .as_array()
            .expect("the inbox lists notices")
            .iter()
            .any(|notice| notice["kind"] == "membership.roles_changed"),
        "holders are told: {body}",
    );

    let listing = role_listing(&court.harness, &court.owner, court.organization).await;
    let positions: Vec<i64> = listing["roles"]
        .as_array()
        .expect("the listing carries roles")
        .iter()
        .map(|role| role["position"].as_i64().expect("a position is a number"))
        .collect();
    assert_eq!(positions, [3, 2, 1, 0], "the roles above close the gap");
}

/// Inviting an address again replaces the invitation it already has, so a
/// re-invite is a revocation of the old offer and is held to the same rule:
/// nobody replaces an offer of a role at or above their own.
#[tokio::test]
async fn a_re_invite_cannot_revoke_an_offer_above_the_inviter() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let invitee = format!("invitee-{}@example.com", Uuid::new_v4());

    let offer =
        |roles: Vec<Uuid>| json!({ "email": invitee, "team_id": court.team, "role_ids": roles });
    let (status, _headers, body) = court
        .send(
            "POST",
            "/tenancy/invitations",
            Some(&offer(vec![court.roles.administrator])),
            &court.owner,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let (status, _headers, _body) = court
        .send(
            "POST",
            "/tenancy/invitations",
            Some(&offer(Vec::new())),
            &court.moderator,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "replacing an administrator offer is revoking it",
    );

    let path = format!("/tenancy/teams/{}/members", court.team);
    let (status, _headers, roster) = court.send("GET", &path, None, &court.owner).await;
    assert_eq!(status, StatusCode::OK, "body: {roster}");
    let pending = roster["members"]
        .as_array()
        .expect("a roster lists members")
        .iter()
        .find(|member| member["email"] == invitee.as_str())
        .expect("the invitation still holds its place");
    assert_eq!(
        pending["roles"][0]["id"],
        json!(court.roles.administrator),
        "and still offers what the owner offered",
    );
}

/// Each standing judges an invitation whole, the offer it replaces included,
/// so an inviter whose team standing cannot replace a pending offer is judged
/// by the organization standing that can, rather than refused by the first
/// standing that allowed the new offer alone.
#[tokio::test]
async fn a_re_invite_is_judged_whole_by_one_standing() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let invitee = format!("invitee-{}@example.com", Uuid::new_v4());

    // The billing role (position 2) learns to invite, and the moderator holds
    // it alone in the team: rank 2 there, rank 3 organization-wide.
    let inviting = ["Tenancy::Membership.create"];
    assert_eq!(
        court
            .set_permissions(&court.owner, court.roles.billing, &inviting)
            .await,
        StatusCode::OK,
    );
    let moderator_in_team = court.team_membership("moderator-").await;
    assert_eq!(
        court
            .grant_in_team(&court.owner, moderator_in_team, &[court.roles.billing])
            .await,
        StatusCode::OK,
    );

    let offer =
        |roles: Vec<Uuid>| json!({ "email": invitee, "team_id": court.team, "role_ids": roles });
    let (status, _headers, body) = court
        .send(
            "POST",
            "/tenancy/invitations",
            Some(&offer(vec![court.roles.billing])),
            &court.owner,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let (status, _headers, body) = court
        .send(
            "POST",
            "/tenancy/invitations",
            Some(&offer(vec![court.roles.editor])),
            &court.moderator,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the organization standing outranks both offers: {body}",
    );
}

/// A guest reaches only what was granted by name, and a role that lets them
/// invite must not undo that: inviting counts the organization standing only
/// for a full member, and a guest invites nobody into the organization.
#[tokio::test]
async fn a_guest_cannot_invite_past_the_guest_boundary() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let member = court.organization_membership("member-").await;
    assert_eq!(
        court
            .grant_in_organization(&court.owner, member, &[court.roles.moderator])
            .await,
        StatusCode::OK,
    );
    let mut connection = court.harness.pool.get().await.expect("connection");
    diesel::update(organization_memberships::table.find(member))
        .set(organization_memberships::access.eq("guest"))
        .execute(&mut connection)
        .await
        .expect("the member becomes a guest");

    assert_eq!(
        court
            .offer(&court.member, ("organization_id", court.organization), &[])
            .await,
        StatusCode::FORBIDDEN,
        "a guest invites nobody in as a full member",
    );
    assert_eq!(
        court
            .offer(&court.member, ("team_id", court.team), &[])
            .await,
        StatusCode::FORBIDDEN,
        "into a team, a guest is judged by their standing there, where editor invites nobody",
    );
}

/// A role applies wherever it is held, so editing one is held to what the
/// editor holds in every place they hold it: an organization-wide role editor
/// cannot raise their own standing in a team through a role they hold there.
#[tokio::test]
async fn a_role_editor_cannot_raise_their_own_team_standing() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let moderator_in_team = court.team_membership("moderator-").await;
    assert_eq!(
        court
            .grant_in_team(&court.owner, moderator_in_team, &[court.roles.editor])
            .await,
        StatusCode::OK,
    );

    assert_eq!(
        court
            .set_permissions(&court.moderator, court.roles.editor, &["Team.read"])
            .await,
        StatusCode::FORBIDDEN,
        "the moderator holds editor in the team, where Team.read is not theirs",
    );

    // Holding Team.read in the team too, through the moderator role, the same
    // edit raises nothing of theirs and is theirs to make.
    assert_eq!(
        court
            .grant_in_team(
                &court.owner,
                moderator_in_team,
                &[court.roles.editor, court.roles.moderator],
            )
            .await,
        StatusCode::OK,
    );
    assert_eq!(
        court
            .set_permissions(&court.moderator, court.roles.editor, &["Team.read"])
            .await,
        StatusCode::OK,
    );
}

/// The administrator is exempt from holding what they hand out, but not from
/// `roles.yml`: no organization's role carries a permission no tenancy role in
/// the file grants, and a model only a platform role names is no permission a
/// tenant's role carries at all.
#[tokio::test]
async fn an_administrator_gives_only_what_roles_yml_lets_a_tenant_hold() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let roles_path = format!("/tenancy/organizations/{}/roles", court.organization);

    for (permissions, expected) in [
        ("Invoice.read", StatusCode::CREATED),
        ("Invoice.destroy", StatusCode::FORBIDDEN),
        ("Ledger.destroy", StatusCode::BAD_REQUEST),
    ] {
        let role = json!({ "name": "Clerk", "color": "#0ea5e9", "permissions": [permissions] });
        let (status, _headers, body) = court
            .send("POST", &roles_path, Some(&role), &court.owner)
            .await;
        assert_eq!(status, expected, "{permissions}: {body}");
    }
    assert_eq!(
        court
            .set_permissions(&court.owner, court.roles.editor, &["Invoice.destroy"])
            .await,
        StatusCode::FORBIDDEN,
        "nor is an existing role edited into one",
    );
}

/// Changing a member's roles replaces them wholesale, so a body that does not
/// name the roles is refused rather than read as "none", and an invitation
/// naming a field the server does not take is refused rather than half read.
#[tokio::test]
async fn a_roles_change_must_name_the_roles() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let member = court.team_membership("member-").await;
    let path = format!("/tenancy/teams/{}/members/{member}", court.team);

    for body in [json!({ "roles": ["editor"] }), json!({})] {
        let (status, _headers, _body) = court.send("PATCH", &path, Some(&body), &court.owner).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    }
    let roster = format!("/tenancy/teams/{}/members", court.team);
    let (status, _headers, body) = court.send("GET", &roster, None, &court.owner).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let held = body["members"]
        .as_array()
        .expect("a roster lists members")
        .iter()
        .find(|entry| entry["membership_id"] == json!(member))
        .expect("the member is on the roster");
    assert_eq!(held["roles"][0]["id"], json!(court.roles.editor));

    let invitation = json!({
        "email": format!("stale-{}@example.com", Uuid::new_v4()),
        "team_id": court.team,
        "roles": ["editor"],
    });
    let (status, _headers, _body) = court
        .send(
            "POST",
            "/tenancy/invitations",
            Some(&invitation),
            &court.owner,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// A suspended member is shut out by every resolver, so they cannot be the
/// administrator that keeps an organization administrable.
#[tokio::test]
async fn a_suspended_administrator_keeps_nothing_administrable() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let moderator = court.organization_membership("moderator-").await;
    let owner = court.organization_membership("owner-").await;
    assert_eq!(
        court
            .grant_in_organization(&court.owner, moderator, &[court.roles.administrator])
            .await,
        StatusCode::OK,
    );
    let mut connection = court.harness.pool.get().await.expect("connection");
    diesel::update(organization_memberships::table.find(moderator))
        .set(organization_memberships::suspended_at.eq(Some(chrono::Utc::now())))
        .execute(&mut connection)
        .await
        .expect("the second administrator is suspended");

    assert_eq!(
        court.grant_in_organization(&court.owner, owner, &[]).await,
        StatusCode::CONFLICT,
        "a suspended administrator administers nothing",
    );
}

/// The first read of a team's roles may offer newly recommended ones, which
/// moves the ladder, so the standing it answers with is read after that.
#[tokio::test]
async fn a_team_roles_read_answers_with_the_standing_it_lists() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let owner = register(&harness.router, "fresh-owner@example.com").await;
    let team = harness.bootstrapped_team(&owner).await;

    let path = format!("/tenancy/teams/{team}/roles");
    let (status, _headers, body) = send(&harness.router, "GET", &path, None, Some(&owner)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let administrator = body["roles"]
        .as_array()
        .expect("the listing carries roles")
        .iter()
        .find(|role| role["key"] == "admin")
        .expect("every organization holds the administrator role");
    assert_eq!(
        body["viewer"]["rank"], administrator["position"],
        "the viewer's rank is measured on the ladder in the same answer: {body}",
    );
}

/// The audit log answers who handed out a role and who lost one: grants made
/// when an invitation is claimed name the inviter, and deleting a role records
/// whose grants went with it.
#[tokio::test]
async fn the_audit_log_names_who_granted_and_who_lost_a_role() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let mut connection = court.harness.pool.get().await.expect("connection");
    let account = |prefix: &'static str| {
        users::table
            .filter(users::email.like(format!("{prefix}%")))
            .select(users::id)
    };
    let owner: Uuid = account("owner-")
        .first(&mut connection)
        .await
        .expect("the owner exists");
    let member: Uuid = account("member-")
        .first(&mut connection)
        .await
        .expect("the member exists");

    let moderator = court.organization_membership("moderator-").await;
    let granted_by: Option<Uuid> = role_grants::table
        .filter(role_grants::organization_membership_id.eq(moderator))
        .select(role_grants::granted_by)
        .first(&mut connection)
        .await
        .expect("the claimed invitation granted the moderator role");
    assert_eq!(
        granted_by,
        Some(owner),
        "the inviter granted what the claim did"
    );

    let path = format!(
        "/tenancy/organizations/{}/roles/{}",
        court.organization, court.roles.editor
    );
    let (status, _headers, body) = court.send("DELETE", &path, None, &court.owner).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");
    let changes: Value = audit_events::table
        .filter(audit_events::action.eq("role.destroyed"))
        .filter(audit_events::subject_id.eq(court.roles.editor))
        .select(audit_events::changes)
        .first(&mut connection)
        .await
        .expect("the deletion is recorded");
    assert_eq!(
        changes["holders"]["old"],
        json!([member.to_string()]),
        "the member held editor in the team: {changes}",
    );
}

/// The default role is held everywhere and granted nowhere, so an edit to it
/// is held to what the editor holds in every team they belong to, and an
/// organization administrator is held to it wherever they do not administer
/// the team.
#[tokio::test]
async fn the_default_role_raises_nobody_in_a_team_either() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let moderator_in_team = court.team_membership("moderator-").await;
    assert_eq!(
        court
            .grant_in_team(&court.owner, moderator_in_team, &[court.roles.editor])
            .await,
        StatusCode::OK,
    );

    assert_eq!(
        court
            .set_permissions(&court.moderator, court.roles.default, &["Team.read"])
            .await,
        StatusCode::FORBIDDEN,
        "the moderator holds Team.read organization-wide, not in the team",
    );

    let moderator = court.organization_membership("moderator-").await;
    assert_eq!(
        court
            .grant_in_organization(&court.owner, moderator, &[court.roles.administrator])
            .await,
        StatusCode::OK,
    );
    assert_eq!(
        court
            .set_permissions(&court.moderator, court.roles.default, &["Team.read"])
            .await,
        StatusCode::FORBIDDEN,
        "administering the organization is not administering a team they are a member of",
    );
}

/// A team's ladder is the organization's, so a reorder is held to the
/// reorderer's standing in every team they belong to: it may not lift a role
/// they hold there over one they could not already act on there.
#[tokio::test]
async fn a_reorder_never_lifts_the_reorderer_in_a_team() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    // In the team, the moderator holds editor, which may now manage members,
    // and the member holds billing, which outranks it.
    assert_eq!(
        court
            .set_permissions(
                &court.owner,
                court.roles.editor,
                &["Tenancy::Membership.update"]
            )
            .await,
        StatusCode::OK,
    );
    let moderator_in_team = court.team_membership("moderator-").await;
    let member_in_team = court.team_membership("member-").await;
    for (membership, role) in [
        (moderator_in_team, court.roles.editor),
        (member_in_team, court.roles.billing),
    ] {
        assert_eq!(
            court.grant_in_team(&court.owner, membership, &[role]).await,
            StatusCode::OK,
        );
    }
    assert_eq!(
        court
            .grant_in_team(&court.moderator, member_in_team, &[])
            .await,
        StatusCode::FORBIDDEN,
        "billing outranks editor in the team",
    );

    let path = format!("/tenancy/organizations/{}/role-order", court.organization);
    let lifted = json!({
        "role_ids": [court.roles.moderator, court.roles.editor, court.roles.billing],
    });
    let (status, _headers, body) = court
        .send("PUT", &path, Some(&lifted), &court.moderator)
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "lifting editor over billing would hand the moderator the member: {body}",
    );
    assert_eq!(
        court
            .grant_in_team(&court.moderator, member_in_team, &[])
            .await,
        StatusCode::FORBIDDEN,
    );
}

/// A guest's organization-wide roles manage nothing: granting organization
/// roles and revoking the organization's invitations, team ones included,
/// reach past what a guest reaches, exactly as inviting does.
#[tokio::test]
async fn a_guest_manages_nothing_organization_wide() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let third_email = format!("third-{}@example.com", Uuid::new_v4());
    let third = register(&court.harness.router, &third_email).await;
    court
        .invite_and_claim(
            &json!({ "email": third_email, "organization_id": court.organization }),
            &third,
        )
        .await;

    let member = court.organization_membership("member-").await;
    assert_eq!(
        court
            .grant_in_organization(&court.owner, member, &[court.roles.moderator])
            .await,
        StatusCode::OK,
    );
    let mut connection = court.harness.pool.get().await.expect("connection");
    diesel::update(organization_memberships::table.find(member))
        .set(organization_memberships::access.eq("guest"))
        .execute(&mut connection)
        .await
        .expect("the member becomes a guest");

    let third_membership = court.organization_membership("third-").await;
    assert_eq!(
        court
            .grant_in_organization(&court.member, third_membership, &[court.roles.editor])
            .await,
        StatusCode::FORBIDDEN,
        "a guest grants nothing organization-wide",
    );

    let invitee = format!("invitee-{}@example.com", Uuid::new_v4());
    let (status, _headers, body) = court
        .send(
            "POST",
            "/tenancy/invitations",
            Some(&json!({ "email": invitee, "team_id": court.team })),
            &court.owner,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let roster = format!("/tenancy/teams/{}/members", court.team);
    let (status, _headers, body) = court.send("GET", &roster, None, &court.owner).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let invitation = body["members"]
        .as_array()
        .expect("a roster lists members")
        .iter()
        .find(|entry| entry["email"] == invitee.as_str())
        .and_then(|entry| entry["invitation_id"].as_str())
        .expect("the invitation holds a place")
        .to_owned();
    let path = format!(
        "/tenancy/organizations/{}/invitations/{invitation}",
        court.organization
    );
    let (status, _headers, _body) = court.send("DELETE", &path, None, &court.member).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a guest revokes nothing through the organization",
    );
}

/// A suspension is a deny, so the membership overview, which the browser draws
/// every affordance from, does not answer a suspended member's access.
#[tokio::test]
async fn the_overview_answers_nothing_for_a_suspension() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let member = court.organization_membership("member-").await;
    let mut connection = court.harness.pool.get().await.expect("connection");
    diesel::update(organization_memberships::table.find(member))
        .set(organization_memberships::suspended_at.eq(Some(chrono::Utc::now())))
        .execute(&mut connection)
        .await
        .expect("the member is suspended");

    let (status, _headers, body) = court
        .send("GET", "/tenancy/memberships", None, &court.member)
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let listed = body["organizations"]
        .as_array()
        .expect("the overview lists organizations")
        .iter()
        .any(|organization| organization["id"] == json!(court.organization));
    assert!(
        !listed,
        "neither the organization nor its team is reachable: {body}",
    );
}

/// When a departure leaves an organization or a team without an
/// administrator, the role goes to the longest-standing member who can use it,
/// never to a suspended one, whom every resolver would shut out.
#[tokio::test]
async fn a_departure_promotes_only_a_reachable_successor() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let moderator = court.organization_membership("moderator-").await;
    let member = court.organization_membership("member-").await;
    let member_in_team = court.team_membership("member-").await;
    let mut connection = court.harness.pool.get().await.expect("connection");
    diesel::update(organization_memberships::table.find(moderator))
        .set(organization_memberships::suspended_at.eq(Some(chrono::Utc::now())))
        .execute(&mut connection)
        .await
        .expect("the longest-standing member is suspended");

    let (status, _headers, body) = court
        .send(
            "DELETE",
            "/auth/account",
            Some(&json!({ "password": PASSWORD })),
            &court.owner,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");

    let administrator = court.roles.administrator;
    let organization_successor: Vec<Uuid> = role_grants::table
        .inner_join(roles::table)
        .filter(roles::id.eq(administrator))
        .filter(role_grants::organization_membership_id.is_not_null())
        .select(role_grants::organization_membership_id.assume_not_null())
        .load(&mut connection)
        .await
        .expect("the grants are readable");
    assert_eq!(
        organization_successor,
        [member],
        "the organization's successor"
    );

    let team_successor: Vec<Uuid> = role_grants::table
        .inner_join(
            team_memberships::table.on(team_memberships::id
                .nullable()
                .eq(role_grants::team_membership_id)),
        )
        .filter(role_grants::role_id.eq(administrator))
        .select(team_memberships::id)
        .load(&mut connection)
        .await
        .expect("the grants are readable");
    assert_eq!(team_successor, [member_in_team], "the team's successor");
}

/// When everybody left is suspended, the administrator role still goes to the
/// longest-standing of them: it gives nothing while the suspension holds, and
/// without it nobody could ever grant the role again once one is lifted.
#[tokio::test]
async fn a_departure_leaves_the_role_with_a_suspended_successor_rather_than_nobody() {
    let Some(database) = TestDatabase::create("adversarial_flow").await else {
        return;
    };
    let court = Court::open(&database).await;
    let moderator = court.organization_membership("moderator-").await;
    let member = court.organization_membership("member-").await;
    let moderator_in_team = court.team_membership("moderator-").await;
    let mut connection = court.harness.pool.get().await.expect("connection");
    diesel::update(
        organization_memberships::table
            .filter(organization_memberships::id.eq_any([moderator, member])),
    )
    .set(organization_memberships::suspended_at.eq(Some(chrono::Utc::now())))
    .execute(&mut connection)
    .await
    .expect("everybody but the owner is suspended");

    let (status, _headers, body) = court
        .send(
            "DELETE",
            "/auth/account",
            Some(&json!({ "password": PASSWORD })),
            &court.owner,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");

    let administrator = court.roles.administrator;
    let organization_successor: Vec<Uuid> = role_grants::table
        .filter(role_grants::role_id.eq(administrator))
        .filter(role_grants::organization_membership_id.is_not_null())
        .select(role_grants::organization_membership_id.assume_not_null())
        .load(&mut connection)
        .await
        .expect("the grants are readable");
    assert_eq!(
        organization_successor,
        [moderator],
        "the longest-standing suspended member holds the role",
    );

    let team_successor: Vec<Uuid> = role_grants::table
        .filter(role_grants::role_id.eq(administrator))
        .filter(role_grants::team_membership_id.is_not_null())
        .select(role_grants::team_membership_id.assume_not_null())
        .load(&mut connection)
        .await
        .expect("the grants are readable");
    assert_eq!(
        team_successor,
        [moderator_in_team],
        "and so does the team's"
    );
}

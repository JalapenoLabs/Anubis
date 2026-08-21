//! In-app notifications end to end, against a real Postgres database.
//!
//! Two narratives. The first follows a notice from the write that caused it to
//! the recipient's inbox: an invitation, a claim, a role change, then the
//! reads and the two ways of marking one read, including what another user's
//! id answers. The second connects a real websocket and runs a real worker, so
//! the path from `notify` to a bell that moves is covered end to end rather
//! than asserted about.
//!
//! Requires `DATABASE_URL`; without it the tests log a skip and pass. CI
//! always provides one.

mod support;

use std::time::Duration;

use anubis::jobs::Worker;
use anubis::notifications::{self, PingRecipient, Pinger};
use axum::http::StatusCode;
use serde_json::{Value, json};
use support::{Harness, TestDatabase, invitation_token, register, send, socket};

/// The address the person doing the inviting registers with.
const OWNER: &str = "owner@notifications.test";

/// The address that is invited, and that already has an account.
const MEMBER: &str = "member@notifications.test";

/// How long the realtime narrative gives the worker to publish one ping.
///
/// Generous, because the path is real: a job row, a worker claim, a publish,
/// and a websocket frame. The test waits on the frame rather than the clock,
/// so a fast machine never waits this long.
const PING_TIMEOUT: Duration = Duration::from_secs(20);

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative, from the write to the inbox"
)]
async fn a_notice_travels_from_the_write_that_caused_it_to_its_inbox() {
    let Some(database) = TestDatabase::create("notifications_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = harness.router.clone();

    let owner_cookie = register(&router, OWNER).await;
    let member_cookie = register(&router, MEMBER).await;
    let team_id = harness.bootstrapped_team(&owner_cookie).await;

    // ------------------------------------------------------------------
    // Inviting an address that already has an account writes it a notice.
    // ------------------------------------------------------------------
    let invitation = json!({ "email": MEMBER, "team_id": team_id, "roles": [ "editor" ] });
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/tenancy/invitations",
        Some(&invitation),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let inbox = list(&router, &member_cookie).await;
    assert_eq!(inbox["unread"], json!(1), "inbox: {inbox}");
    assert_eq!(
        inbox["notifications"][0]["kind"],
        json!(notifications::INVITATION_RECEIVED),
    );
    assert_eq!(inbox["notifications"][0]["team_id"], json!(team_id));
    assert!(
        inbox["notifications"][0]["title"]
            .as_str()
            .is_some_and(|title| title.contains(OWNER)),
        "the notice names who invited them: {inbox}",
    );
    assert_eq!(
        inbox["notifications"][0]["href"],
        Value::Null,
        "the claim token stays in the email, so the notice links nowhere",
    );
    assert!(
        inbox["notifications"][0]["read_at"].is_null(),
        "a fresh notice is unread: {inbox}",
    );

    // The person who sent it hears nothing about their own action.
    let owner_inbox = list(&router, &owner_cookie).await;
    assert_eq!(owner_inbox["unread"], json!(0), "inbox: {owner_inbox}");

    // ------------------------------------------------------------------
    // Claiming it tells the admins of what was joined.
    // ------------------------------------------------------------------
    let token = invitation_token(&harness.outbox, MEMBER);
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/tenancy/invitations/claim",
        Some(&json!({ "token": token })),
        Some(&member_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let owner_inbox = list(&router, &owner_cookie).await;
    assert_eq!(owner_inbox["unread"], json!(1), "inbox: {owner_inbox}");
    assert_eq!(
        owner_inbox["notifications"][0]["kind"],
        json!(notifications::INVITATION_CLAIMED),
    );
    assert_eq!(
        owner_inbox["notifications"][0]["href"],
        json!(format!("/teams/{team_id}/settings")),
        "the notice points at the roster it is about",
    );

    // ------------------------------------------------------------------
    // Changing somebody's roles tells that person, and nobody else.
    // ------------------------------------------------------------------
    let membership_id = membership_of(&router, &owner_cookie, team_id, MEMBER).await;
    let (status, _headers, body) = send(
        &router,
        "PATCH",
        &format!("/tenancy/teams/{team_id}/members/{membership_id}"),
        Some(&json!({ "roles": [ "admin" ] })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let inbox = list(&router, &member_cookie).await;
    assert_eq!(inbox["unread"], json!(2), "inbox: {inbox}");
    assert_eq!(
        inbox["notifications"][0]["kind"],
        json!(notifications::MEMBERSHIP_ROLES_CHANGED),
        "the newest unread notice comes first: {inbox}",
    );
    assert_eq!(
        inbox["notifications"][1]["kind"],
        json!(notifications::INVITATION_RECEIVED),
    );

    // ------------------------------------------------------------------
    // Reading: one page at a time, unread first, then in reverse order.
    // ------------------------------------------------------------------
    let (status, _headers, page) = send(
        &router,
        "GET",
        "/account/notifications?limit=1",
        None,
        Some(&member_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {page}");
    assert_eq!(page["notifications"].as_array().map(Vec::len), Some(1));
    assert_eq!(page["pagination"]["total_items"], json!(2));
    assert_eq!(page["pagination"]["total_pages"], json!(2));
    assert_eq!(
        page["unread"],
        json!(2),
        "the badge counts the inbox, not the page: {page}",
    );

    // ------------------------------------------------------------------
    // Marking one read moves it down the list rather than out of it.
    // ------------------------------------------------------------------
    let oldest = notification_id(&inbox, 1);
    let (status, _headers, body) = send(
        &router,
        "POST",
        &format!("/account/notifications/{oldest}/read"),
        None,
        Some(&member_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        !body["notification"]["read_at"].is_null(),
        "the answer carries the row as it now stands: {body}",
    );

    let inbox = list(&router, &member_cookie).await;
    assert_eq!(inbox["unread"], json!(1), "inbox: {inbox}");
    assert_eq!(
        notification_id(&inbox, 1),
        oldest,
        "a read notice keeps its place in time: {inbox}",
    );

    // Pressing it again is harmless and keeps the first reading's timestamp.
    let read_at = inbox["notifications"][1]["read_at"].clone();
    let (status, _headers, body) = send(
        &router,
        "POST",
        &format!("/account/notifications/{oldest}/read"),
        None,
        Some(&member_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["notification"]["read_at"], read_at);

    // ------------------------------------------------------------------
    // An inbox is nobody else's to read or to touch.
    // ------------------------------------------------------------------
    let owner_inbox = list(&router, &owner_cookie).await;
    let owners_own = notification_id(&owner_inbox, 0);
    let (status, _headers, body) = send(
        &router,
        "POST",
        &format!("/account/notifications/{owners_own}/read"),
        None,
        Some(&member_cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "another user's notice is not found, not forbidden: {body}",
    );
    assert_eq!(
        list(&router, &owner_cookie).await["unread"],
        json!(1),
        "and it stays unread",
    );

    let (status, _headers, body) = send(&router, "GET", "/account/notifications", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");

    // ------------------------------------------------------------------
    // Marking everything read empties the badge and stays idempotent.
    // ------------------------------------------------------------------
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/account/notifications/read-all",
        None,
        Some(&member_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["marked"], json!(1), "only the unread one was marked");

    let inbox = list(&router, &member_cookie).await;
    assert_eq!(inbox["unread"], json!(0), "inbox: {inbox}");
    assert_eq!(
        inbox["notifications"].as_array().map(Vec::len),
        Some(2),
        "reading an inbox never empties it: {inbox}",
    );

    let (_status, _headers, body) = send(
        &router,
        "POST",
        "/account/notifications/read-all",
        None,
        Some(&member_cookie),
    )
    .await;
    assert_eq!(body["marked"], json!(0), "nothing was left to mark");
}

#[tokio::test]
async fn a_written_notice_reaches_the_recipients_open_socket() {
    let Some(database) = TestDatabase::create("notifications_flow realtime").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let router = harness.router.clone();
    let address = harness.serve();

    let owner_cookie = register(&router, OWNER).await;
    let member_cookie = register(&router, MEMBER).await;
    let team_id = harness.bootstrapped_team(&owner_cookie).await;
    let member_id = user_id(&router, &member_cookie).await;

    // The bell's own subscription: the recipient's user channel, whose name is
    // the authorization rule, so nobody else can listen in on this inbox.
    let mut listening = socket::connect(address, &member_cookie).await;
    let channel = format!("user:{member_id}:notifications");
    let subscribed = socket::subscribe(&mut listening, &channel).await;
    assert_eq!(subscribed["type"], json!("subscribed"), "{subscribed}");

    // The worker is what publishes, once the transaction that wrote the row
    // has committed. Without one the notice is still delivered; the bell just
    // waits for its next fetch.
    let pinger = Pinger::new(harness.channels.clone());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let worker = Worker::builder(harness.pool.clone())
        .register(move |job: PingRecipient| {
            let pinger = pinger.clone();
            async move { pinger.ping(job).await }
        })
        // Fast enough that the test tracks the worker rather than the clock.
        .poll_interval(Duration::from_millis(25))
        .build();
    let running = tokio::spawn(worker.run(async move {
        let _stopped = stopped.await;
    }));

    let invitation = json!({ "email": MEMBER, "team_id": team_id });
    let (status, _headers, body) = send(
        &router,
        "POST",
        "/tenancy/invitations",
        Some(&invitation),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let frame = tokio::time::timeout(PING_TIMEOUT, socket::next_frame(&mut listening))
        .await
        .expect("the worker must publish the ping within the timeout");
    assert_eq!(frame["type"], json!("event"), "{frame}");
    assert_eq!(frame["channel"], json!(channel), "{frame}");
    assert_eq!(frame["event"], json!("created"), "{frame}");
    assert_eq!(
        frame["payload"],
        json!({}),
        "nothing durable rides the socket; the client refetches: {frame}",
    );

    // And what it refetches is the row the ping was about.
    let inbox = list(&router, &member_cookie).await;
    assert_eq!(inbox["unread"], json!(1), "inbox: {inbox}");

    let _stopping = stop.send(());
    running.await.expect("the worker must shut down cleanly");
}

/// The recipient's whole first page.
async fn list(router: &axum::Router, cookie: &str) -> Value {
    let (status, _headers, body) =
        send(router, "GET", "/account/notifications", None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    body
}

/// The id of the notification at `index` of a listing.
fn notification_id(inbox: &Value, index: usize) -> String {
    inbox["notifications"][index]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("notification {index} must exist: {inbox}"))
        .to_owned()
}

/// The signed-in user's own id.
async fn user_id(router: &axum::Router, cookie: &str) -> String {
    let (status, _headers, body) = send(router, "GET", "/auth/me", None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    body["user"]["id"]
        .as_str()
        .expect("a signed-in user has an id")
        .to_owned()
}

/// The membership id `email` holds on `team_id`, read from the roster.
async fn membership_of(
    router: &axum::Router,
    cookie: &str,
    team_id: uuid::Uuid,
    email: &str,
) -> String {
    let (status, _headers, body) = send(
        router,
        "GET",
        &format!("/tenancy/teams/{team_id}/members"),
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    body["members"]
        .as_array()
        .expect("a roster is a list")
        .iter()
        .find(|member| member["email"] == json!(email))
        .and_then(|member| member["membership_id"].as_str())
        .unwrap_or_else(|| panic!("{email} must hold a membership: {body}"))
        .to_owned()
}

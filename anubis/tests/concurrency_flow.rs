//! What the framework promises when two requests arrive at the same instant.
//!
//! Every other suite in this directory is a linear narrative: one caller, one
//! step after another. This one runs the steps *together*, on a multi-threaded
//! runtime with real tasks against a real database, and asserts the property
//! that has to survive it. Each test takes a database of its own (see
//! `support::TestDatabase`), because a race is only observable in totals and a
//! total is only meaningful when nothing else writes to the table.
//!
//! Requires `DATABASE_URL`; without it each test logs a skip and passes. CI
//! always provides one.

mod support;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anubis::jobs::{self, BoxError, Job, Worker};
use anubis::realtime::ChannelName;
use anubis::schema::{
    dead_jobs, jobs as jobs_table, organization_memberships, organizations, sessions,
    team_memberships, teams, user_avatars, users, webhook_deliveries, webhook_endpoints,
};
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE};
use axum::http::{Request, StatusCode};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use http_body_util::BodyExt;
use image::codecs::png::PngEncoder;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tower::ServiceExt;
use uuid::Uuid;

use support::{
    Harness, PASSWORD, TestDatabase, invitation_token, register, send, session_token, socket,
};

/// How many racers a test fires at once.
///
/// Small enough to stay inside the connection pool and finish in well under a
/// second, large enough that an unserialized write is overwhelmingly likely to
/// interleave rather than happening to line up.
const RACERS: usize = 8;

/// How long racers get to queue behind a lock a test is holding open.
///
/// Only used where a test blocks the handlers deliberately, to make an
/// otherwise scheduler-dependent race arrive together. It bounds nothing and
/// overshooting only costs the test its own time.
const QUEUING_WINDOW: Duration = Duration::from_millis(250);

/// How long [`wait_until_queued`] gives racers to reach a lock a test holds.
///
/// An upper bound and nothing else: the wait returns the moment they arrive,
/// and a test that hits this deadline fails rather than quietly proving
/// nothing. It is measured in seconds because a signup computes an argon2 hash
/// before it opens a transaction, and several of those share the machine.
const QUEUING_DEADLINE: Duration = Duration::from_secs(30);

/// How often that wait re-asks.
const QUEUING_POLL: Duration = Duration::from_millis(10);

/// How many first signups race to create the one shared organization.
///
/// Two is what the property is about, and two is what the connection pool can
/// always spare: each racer holds a pooled connection for as long as it queues
/// behind the lock, and the pool is sized from the machine's core count.
const FIRST_SIGNUPS: usize = 2;

/// The administrator two instances booting together both try to seed.
const BOOTSTRAP_ADMIN_EMAIL: &str = "founder@example.com";

// ---------------------------------------------------------------------------
// Migrations
// ---------------------------------------------------------------------------

/// Two processes booting together both apply migrations, and CI proved that
/// racing: concurrent `CREATE TABLE` of the same table fails one of them with a
/// unique violation on Postgres' own `pg_type` index. The advisory lock in
/// `anubis::db` orders them; this is the test that would have caught it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_migration_passes_serialize() {
    let Some(database) = TestDatabase::create_bare("concurrency_flow").await else {
        return;
    };

    let (first, second, third) = tokio::join!(
        anubis::db::run_pending_migrations(database.url()),
        anubis::db::run_pending_migrations(database.url()),
        anubis::db::run_pending_migrations(database.url()),
    );

    first.expect("the first migration pass must apply");
    second.expect("a concurrent migration pass must wait, not collide");
    third.expect("a concurrent migration pass must wait, not collide");
}

// ---------------------------------------------------------------------------
// Invitations
// ---------------------------------------------------------------------------

/// An invitation is a single-use credential, so two accounts presenting it at
/// the same instant must produce one member and one clean refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_invitation_admits_exactly_one_claimant() {
    let Some(database) = TestDatabase::create("concurrency_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let admin = register(&harness.router, "admin@example.com").await;
    let team_id = harness.bootstrapped_team(&admin).await;

    let invite = json!({ "email": "invitee@example.com", "team_id": team_id, "roles": ["editor"] });
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/tenancy/invitations",
        Some(&invite),
        Some(&admin),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let token = invitation_token(&harness.outbox, "invitee@example.com");

    let alpha = register(&harness.router, "alpha@example.com").await;
    let beta = register(&harness.router, "beta@example.com").await;

    let claim = json!({ "token": token });
    let (first, second) = tokio::join!(
        claim_invitation(&harness, &claim, alpha),
        claim_invitation(&harness, &claim, beta),
    );

    let mut outcomes = [first, second];
    outcomes.sort_unstable();
    assert_eq!(
        outcomes,
        [StatusCode::OK, StatusCode::BAD_REQUEST],
        "one claimant wins and the other is refused, not served or crashed",
    );

    let mut connection = harness.pool.get().await.expect("connection");
    let claimed: i64 = team_memberships::table
        .filter(team_memberships::team_id.eq(team_id))
        .filter(team_memberships::user_id.is_not_null())
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(claimed, 2, "the admin and exactly one claimant");

    let unclaimed: i64 = team_memberships::table
        .filter(team_memberships::team_id.eq(team_id))
        .filter(team_memberships::user_id.is_null())
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(
        unclaimed, 0,
        "the pre-created membership was adopted, not left"
    );
}

/// Sends one claim on its own task, so both are genuinely in flight together.
async fn claim_invitation(harness: &Harness, claim: &Value, cookie: String) -> StatusCode {
    let router = harness.router.clone();
    let claim = claim.clone();
    tokio::spawn(async move {
        let (status, _headers, _body) = send(
            &router,
            "POST",
            "/tenancy/invitations/claim",
            Some(&claim),
            Some(&cookie),
        )
        .await;
        status
    })
    .await
    .expect("the claim task must not panic")
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// Sessions are additive by design: signing in on a second device must not
/// disturb the first. Simultaneous sign-ins are the same property under load,
/// and each one has to come back with a token of its own.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_logins_each_get_their_own_session() {
    let Some(database) = TestDatabase::create("concurrency_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let _registered = register(&harness.router, "many-devices@example.com").await;

    let credentials = json!({ "email": "many-devices@example.com", "password": PASSWORD });
    let mut logins = Vec::with_capacity(RACERS);
    for _device in 0..RACERS {
        let router = harness.router.clone();
        let credentials = credentials.clone();
        logins.push(tokio::spawn(async move {
            let (status, headers, body) =
                send(&router, "POST", "/auth/login", Some(&credentials), None).await;
            assert_eq!(status, StatusCode::OK, "body: {body}");
            session_token(&headers)
        }));
    }

    let mut tokens = BTreeSet::new();
    for login in logins {
        tokens.insert(login.await.expect("a login task must not panic"));
    }
    assert_eq!(tokens.len(), RACERS, "every sign-in gets its own token");

    let mut connection = harness.pool.get().await.expect("connection");
    let stored: i64 = sessions::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(
        stored,
        i64::try_from(RACERS).expect("the racer count fits") + 1,
        "one row per sign-in, plus the one registration created",
    );

    // Every token still opens its own session, so the sweep of expired rows
    // that runs on each sign-in never touched a live sibling.
    for token in &tokens {
        let (status, _headers, body) =
            send(&harness.router, "GET", "/auth/me", None, Some(token)).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
    }
}

// ---------------------------------------------------------------------------
// Avatars
// ---------------------------------------------------------------------------

/// The avatar table holds one row per user, written through an upsert. Two
/// uploads landing together must leave one row whose bytes and `ETag` came
/// from the same upload, and neither may fail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_avatar_uploads_leave_one_consistent_row() {
    let Some(database) = TestDatabase::create("concurrency_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let cookie = register(&harness.router, "poser@example.com").await;

    // Two distinguishable sources, so the stored bytes name their upload.
    let square = png_bytes(&image::DynamicImage::new_rgb8(300, 300));
    let landscape = png_bytes(&image::DynamicImage::new_rgb8(640, 200));

    let (first, second) = tokio::join!(
        upload_avatar(&harness, &cookie, square),
        upload_avatar(&harness, &cookie, landscape),
    );
    assert_eq!(
        first,
        StatusCode::OK,
        "an upsert race is not a server error"
    );
    assert_eq!(
        second,
        StatusCode::OK,
        "an upsert race is not a server error"
    );

    let mut connection = harness.pool.get().await.expect("connection");
    let rows: Vec<(Vec<u8>, String)> = user_avatars::table
        .select((user_avatars::image, user_avatars::etag))
        .load(&mut connection)
        .await
        .expect("the avatar query must run");
    assert_eq!(rows.len(), 1, "one user, one avatar row");

    let (stored, etag) = rows.into_iter().next().expect("one row");
    let decoded = image::load_from_memory(&stored).expect("the stored avatar must decode");
    assert!(
        decoded.width() == 300 || decoded.width() == 200,
        "the stored image is one upload's output, not a blend: {}x{}",
        decoded.width(),
        decoded.height(),
    );

    // The one property an interleaved upsert would break: the `ETag` a browser
    // caches against has to be the digest of the bytes stored beside it.
    let served = serve_avatar(&harness, &cookie).await;
    assert_eq!(served, stored, "the served bytes are the stored bytes");
    assert!(!etag.is_empty(), "a stored avatar always carries its ETag");
}

/// Uploads one image on its own task and answers with the status.
async fn upload_avatar(harness: &Harness, cookie: &str, image: Vec<u8>) -> StatusCode {
    let router = harness.router.clone();
    let cookie = cookie.to_owned();
    tokio::spawn(async move {
        let request = Request::builder()
            .method("POST")
            .uri("/auth/profile/avatar")
            .header(COOKIE, format!("anubis_session={cookie}"))
            .header(CONTENT_TYPE, "image/png")
            .body(Body::from(image))
            .expect("request must build");
        router
            .oneshot(request)
            .await
            .expect("the upload must complete")
            .status()
    })
    .await
    .expect("the upload task must not panic")
}

/// Reads the avatar back through its public URL.
async fn serve_avatar(harness: &Harness, cookie: &str) -> Vec<u8> {
    let (status, _headers, body) =
        send(&harness.router, "GET", "/auth/me", None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let user_id = body["user"]["id"]
        .as_str()
        .expect("the profile names the user");

    let request = Request::builder()
        .uri(format!("/users/{user_id}/avatar"))
        .body(Body::empty())
        .expect("request must build");
    let response = harness
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("the avatar request must complete");
    assert_eq!(response.status(), StatusCode::OK);

    response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes()
        .to_vec()
}

fn png_bytes(image: &image::DynamicImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    image
        .write_with_encoder(PngEncoder::new(&mut bytes))
        .expect("encoding the fixture must succeed");
    bytes
}

// ---------------------------------------------------------------------------
// The job queue
// ---------------------------------------------------------------------------

/// The queue's whole claim story, and the reason it selects `FOR UPDATE SKIP
/// LOCKED`: any number of workers draw from one table without two of them
/// taking the same row.
#[derive(Serialize, Deserialize)]
struct Count {
    ticket: usize,
}

impl Job for Count {
    const KIND: &'static str = "concurrency_flow_count";
    const QUEUE: &'static str = "concurrency_flow";
}

/// Enough work that both workers get some of it, few enough to drain fast.
const TICKETS: usize = 24;

/// Two workers on one queue: delivery is at-least-once, so the assertion is
/// that nothing is *lost* and nothing is buried, and that the observed
/// executions do not exceed what at-least-once explains.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_workers_lose_no_job_and_bury_none() {
    let Some(database) = TestDatabase::create("concurrency_flow").await else {
        return;
    };
    let pool = database.pool().await;

    let mut connection = pool.get().await.expect("connection");
    for ticket in 0..TICKETS {
        jobs::enqueue(&mut connection, &Count { ticket })
            .await
            .expect("enqueue must succeed");
    }
    drop(connection);

    let ran: Arc<Mutex<Vec<usize>>> = Arc::new(Mutex::new(Vec::new()));
    let (stop, stopped) = tokio::sync::watch::channel(());

    let mut workers = Vec::new();
    for _worker in 0..2 {
        let ran = Arc::clone(&ran);
        let worker = Worker::builder(pool.clone())
            .poll_interval(Duration::from_millis(20))
            .register(move |job: Count| {
                let ran = Arc::clone(&ran);
                async move {
                    ran.lock()
                        .expect("the ledger lock is never poisoned")
                        .push(job.ticket);
                    Ok::<(), BoxError>(())
                }
            })
            .build();

        let mut signal = stopped.clone();
        workers.push(tokio::spawn(worker.run(async move {
            let _stopped = signal.changed().await;
        })));
    }

    // Both workers drain the queue; the deadline only bounds a failure.
    let drained = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if ran.lock().expect("the ledger lock is never poisoned").len() >= TICKETS {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let _stopping = stop.send(());
    for worker in workers {
        worker.await.expect("a worker must not panic");
    }
    drained.expect("two workers must drain the queue");

    let observed = ran
        .lock()
        .expect("the ledger lock is never poisoned")
        .clone();
    let distinct: BTreeSet<usize> = observed.iter().copied().collect();
    assert_eq!(distinct.len(), TICKETS, "no job was lost: every ticket ran");
    assert_eq!(
        observed.len(),
        TICKETS,
        "no job ran twice at this scale; at-least-once allows it only when a \
         lease expires under a running handler, which none of these do",
    );

    let mut connection = pool.get().await.expect("connection");
    let left: i64 = jobs_table::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(left, 0, "a completed job is deleted, not left claimed");

    let buried: i64 = dead_jobs::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(buried, 0, "nothing failed, so nothing is dead-lettered");
}

// ---------------------------------------------------------------------------
// The last-admin invariant
// ---------------------------------------------------------------------------

/// The race the `409` exists for. Two admins demoting each other at the same
/// instant each read the other as the admin that remains; without ordering,
/// both writes commit and the team is left with nobody who can administer it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_demotions_keep_a_team_administrable() {
    let Some(database) = TestDatabase::create("concurrency_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let first = register(&harness.router, "first-admin@example.com").await;
    let team_id = harness.bootstrapped_team(&first).await;
    let second = register(&harness.router, "second-admin@example.com").await;

    let invite =
        json!({ "email": "second-admin@example.com", "team_id": team_id, "roles": ["admin"] });
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/tenancy/invitations",
        Some(&invite),
        Some(&first),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let token = invitation_token(&harness.outbox, "second-admin@example.com");
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/tenancy/invitations/claim",
        Some(&json!({ "token": token })),
        Some(&second),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let memberships = admin_memberships(&harness, &first, team_id).await;
    assert_eq!(memberships.len(), 2, "two claimed admins before the race");
    let (first_membership, second_membership) = (memberships[0], memberships[1]);

    // Each demotes the other, and both are made to arrive together: a third
    // connection holds the very lock the handlers take, so both get past their
    // guards, both queue behind it, and neither can win by being scheduled
    // first. Without that, the scheduler usually runs one request to
    // completion, the other's guard finds it is no longer an admin, and the
    // race the invariant exists for is never actually run.
    let demote = json!({ "roles": ["editor"] });
    let mut blocker = harness.pool.get().await.expect("connection");
    let racing = blocker
        .transaction::<_, diesel::result::Error, _>(async |transaction| {
            teams::table
                .find(team_id)
                .select(teams::id)
                .for_update()
                .first::<Uuid>(transaction)
                .await?;

            let one = demote_member(&harness, &first, team_id, second_membership, &demote);
            let other = demote_member(&harness, &second, team_id, first_membership, &demote);
            // Long enough for both requests to authenticate, resolve their
            // membership, and reach the lock. Overshooting only costs time.
            tokio::time::sleep(QUEUING_WINDOW).await;
            Ok((one, other))
        })
        .await
        .expect("holding the team lock must succeed");
    drop(blocker);

    let (one, other) = racing;
    let outcomes = [
        one.await.expect("the demotion task must not panic"),
        other.await.expect("the demotion task must not panic"),
    ];
    let accepted = outcomes.iter().filter(|status| status.is_success()).count();
    assert_eq!(accepted, 1, "exactly one demotion lands: {outcomes:?}");

    // The loser is refused either way. `409` is what the invariant answers
    // when both requests were already past their guards, which is what the
    // lock above arranges; `403` is what the guard itself answers if the
    // winner still managed to commit first, on a machine slow enough that the
    // queuing window expired early. Either is a refusal; being served is not.
    let refused = outcomes
        .iter()
        .find(|status| !status.is_success())
        .expect("one demotion is refused");
    assert!(
        matches!(*refused, StatusCode::CONFLICT | StatusCode::FORBIDDEN),
        "the losing demotion is refused, not served: {refused}",
    );

    let mut connection = harness.pool.get().await.expect("connection");
    let admins: i64 = team_memberships::table
        .filter(team_memberships::team_id.eq(team_id))
        .filter(team_memberships::user_id.is_not_null())
        .filter(team_memberships::roles.contains(vec![anubis::tenancy::ADMIN_ROLE]))
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(admins, 1, "the team is never left without an admin");
}

/// Disabling is refused against oneself, so no single request can take the
/// last admin who is able to sign in. Two requests can: each administrator
/// disables the other, each reads the other as the one still standing. The
/// organization lock orders them and the count catches whichever arrives
/// second, exactly as it does for demotion.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_disables_keep_an_organization_administrable() {
    let Some(database) = TestDatabase::create("concurrency_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let first = register(&harness.router, "first-owner@example.com").await;
    let organization_id = bootstrapped_organization(&harness, &first).await;
    let second = register(&harness.router, "second-owner@example.com").await;

    let invite = json!({
        "email": "second-owner@example.com",
        "organization_id": organization_id,
        "roles": ["admin"],
    });
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/tenancy/invitations",
        Some(&invite),
        Some(&first),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let token = invitation_token(&harness.outbox, "second-owner@example.com");
    let (status, _headers, body) = send(
        &harness.router,
        "POST",
        "/tenancy/invitations/claim",
        Some(&json!({ "token": token })),
        Some(&second),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // Addressed by email rather than by position: each request has to name the
    // *other* administrator, and one aimed at its own account is refused for a
    // reason that has nothing to do with the race.
    let first_membership =
        organization_membership(&harness, &first, organization_id, "first-owner@example.com").await;
    let second_membership = organization_membership(
        &harness,
        &first,
        organization_id,
        "second-owner@example.com",
    )
    .await;

    // Both are made to arrive together by a third connection holding the very
    // lock the handlers take; see the note in the demotion race above.
    let mut blocker = harness.pool.get().await.expect("connection");
    let racing = blocker
        .transaction::<_, diesel::result::Error, _>(async |transaction| {
            organizations::table
                .find(organization_id)
                .select(organizations::id)
                .for_update()
                .first::<Uuid>(transaction)
                .await?;

            let one = disable_member(&harness, &first, organization_id, second_membership);
            let other = disable_member(&harness, &second, organization_id, first_membership);
            tokio::time::sleep(QUEUING_WINDOW).await;
            Ok((one, other))
        })
        .await
        .expect("holding the organization lock must succeed");
    drop(blocker);

    let (one, other) = racing;
    let outcomes = [
        one.await.expect("the disable task must not panic"),
        other.await.expect("the disable task must not panic"),
    ];
    let accepted = outcomes.iter().filter(|status| status.is_success()).count();
    assert_eq!(accepted, 1, "exactly one disable lands: {outcomes:?}");

    // Three refusals are correct, and which one arrives says how far the loser
    // got before the winner committed. `409` is the invariant firing, both
    // requests already past their guards, which is what the lock above
    // arranges. `403` is the extractor refusing an account the winner had
    // already disabled. `401` is the same moment one step later: the winner
    // deleted the loser's session with it, so there is nothing left to
    // authenticate. Being served is the only wrong answer.
    let refused = outcomes
        .iter()
        .find(|status| !status.is_success())
        .expect("one disable is refused");
    assert!(
        matches!(
            *refused,
            StatusCode::CONFLICT | StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
        ),
        "the losing disable is refused, not served: {refused}",
    );

    let mut connection = harness.pool.get().await.expect("connection");
    let admins: i64 = organization_memberships::table
        .inner_join(users::table)
        .filter(organization_memberships::organization_id.eq(organization_id))
        .filter(organization_memberships::roles.contains(vec![anubis::tenancy::ADMIN_ROLE]))
        .filter(users::disabled_at.is_null())
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(
        admins, 1,
        "the organization always keeps an admin who can sign in",
    );
}

/// The organization registration bootstrapped for `cookie`.
async fn bootstrapped_organization(harness: &Harness, cookie: &str) -> Uuid {
    let (status, _headers, body) = send(
        &harness.router,
        "GET",
        "/tenancy/memberships",
        None,
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    body["organizations"][0]["id"]
        .as_str()
        .expect("registration bootstraps one organization")
        .parse()
        .expect("an organization id is a UUID")
}

/// The organization membership the roster lists for `email`.
async fn organization_membership(
    harness: &Harness,
    cookie: &str,
    organization_id: Uuid,
    email: &str,
) -> Uuid {
    let path = format!("/tenancy/organizations/{organization_id}/members");
    let (status, _headers, body) = send(&harness.router, "GET", &path, None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    body["members"]
        .as_array()
        .expect("the roster is an array")
        .iter()
        .find(|member| member["email"] == json!(email))
        .and_then(|member| member["membership_id"].as_str())
        .expect("the claimed membership must be on the roster")
        .parse()
        .expect("a membership id is a UUID")
}

/// Starts one account disable on its own task, without waiting for it.
fn disable_member(
    harness: &Harness,
    cookie: &str,
    organization_id: Uuid,
    membership_id: Uuid,
) -> JoinHandle<StatusCode> {
    let router = harness.router.clone();
    let cookie = cookie.to_owned();
    tokio::spawn(async move {
        let path =
            format!("/tenancy/organizations/{organization_id}/members/{membership_id}/disable");
        let (status, _headers, _body) = send(&router, "POST", &path, None, Some(&cookie)).await;
        status
    })
}

/// The claimed admin memberships of a team, in a stable order.
async fn admin_memberships(harness: &Harness, cookie: &str, team_id: Uuid) -> Vec<Uuid> {
    let path = format!("/tenancy/teams/{team_id}/members");
    let (status, _headers, body) = send(&harness.router, "GET", &path, None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let mut ids: Vec<Uuid> = body["members"]
        .as_array()
        .expect("the roster is an array")
        .iter()
        .filter(|member| member["pending"] != json!(true))
        .map(|member| {
            member["membership_id"]
                .as_str()
                .expect("a roster row names its membership")
                .parse()
                .expect("a membership id is a UUID")
        })
        .collect();
    ids.sort_unstable();
    ids
}

/// Starts a role replacement on its own task, without waiting for it.
fn demote_member(
    harness: &Harness,
    cookie: &str,
    team_id: Uuid,
    membership_id: Uuid,
    roles: &Value,
) -> JoinHandle<StatusCode> {
    let router = harness.router.clone();
    let cookie = cookie.to_owned();
    let roles = roles.clone();
    tokio::spawn(async move {
        let path = format!("/tenancy/teams/{team_id}/members/{membership_id}");
        let (status, _headers, _body) =
            send(&router, "PATCH", &path, Some(&roles), Some(&cookie)).await;
        status
    })
}

// ---------------------------------------------------------------------------
// The shared bootstrap
// ---------------------------------------------------------------------------

/// The race the advisory lock in `tenancy::bootstrap` exists for. A shared
/// deployment's first signups each find no organization to join, and without
/// ordering each creates one, leaving as many organizations of the same name
/// as there were racers and no way to say which is the one everybody shares.
///
/// The racers are made to arrive together, because a signup spends most of its
/// time hashing a password and firing them at once is not enough on a busy
/// machine. A connection of the test's own holds `organizations` locked
/// against writes: reads are untouched, so both racers get past the question
/// "does it exist yet?" and then queue, which is exactly the interleaving the
/// lock defends against.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_first_signups_create_one_shared_organization() {
    let Some(database) = TestDatabase::create("concurrency_flow").await else {
        return;
    };
    let harness = Harness::boot_with_config(
        &database,
        Harness::config_with(&[
            ("ANUBIS_BOOTSTRAP", "shared"),
            ("ANUBIS_SHARED_ORGANIZATION", "Acme Corporation"),
        ]),
    )
    .await;

    // Both the blocker and the wait below connect outside the application's
    // pool, so every pooled connection is left for a racer to hold while it
    // queues.
    let mut blocker = AsyncPgConnection::establish(database.url())
        .await
        .expect("a blocking connection");
    let mut observer = AsyncPgConnection::establish(database.url())
        .await
        .expect("an observing connection");

    let racing = blocker
        .transaction::<_, diesel::result::Error, _>(async |transaction| {
            diesel::sql_query("LOCK TABLE organizations IN EXCLUSIVE MODE")
                .execute(transaction)
                .await?;

            let mut signups = Vec::with_capacity(FIRST_SIGNUPS);
            for racer in 0..FIRST_SIGNUPS {
                let router = harness.router.clone();
                signups.push(tokio::spawn(async move {
                    let credentials = json!({
                        "email": format!("racer-{racer}@example.com"),
                        "password": PASSWORD,
                    });
                    let (status, _headers, body) =
                        send(&router, "POST", "/auth/register", Some(&credentials), None).await;
                    assert_eq!(status, StatusCode::CREATED, "body: {body}");
                }));
            }
            wait_until_queued(&mut observer, FIRST_SIGNUPS).await;
            Ok(signups)
        })
        .await
        .expect("holding the organizations lock must succeed");
    drop(blocker);

    for signup in racing {
        signup.await.expect("a signup task must not panic");
    }

    let racers = i64::try_from(FIRST_SIGNUPS).expect("the racer count fits");
    let mut connection = harness.pool.get().await.expect("connection");

    let organizations_created: i64 = organizations::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(
        organizations_created, 1,
        "the deployment has the one organization it named",
    );

    let teams_created: i64 = teams::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(teams_created, 1, "and that organization's default team");

    // Whoever lost the race joined rather than failing, so every account is
    // inside, in the organization and in its team.
    let members: i64 = organization_memberships::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(members, racers, "every signup is an organization member");

    let team_members: i64 = team_memberships::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(team_members, racers, "and a member of its team");
}

/// Waits until `count` of this database's sessions are queued on a lock.
///
/// What a test means by "the racers arrived together", asked as the condition
/// itself rather than approximated with a sleep, which on a slow machine
/// releases the blocker before anybody queued and proves nothing at all.
///
/// # Panics
/// Panics when they have not all queued within [`QUEUING_DEADLINE`], because a
/// race that never formed is a failed test rather than a passing one.
async fn wait_until_queued(connection: &mut AsyncPgConnection, count: usize) {
    /// One row of the count, for `sql_query`.
    #[derive(QueryableByName)]
    struct Queued {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        queued: i64,
    }

    let count = i64::try_from(count).expect("the racer count fits");
    let deadline = Instant::now() + QUEUING_DEADLINE;

    loop {
        let queued = diesel::sql_query(
            "SELECT count(*) AS queued FROM pg_stat_activity \
             WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .get_result::<Queued>(&mut *connection)
        .await
        .expect("the activity query must run")
        .queued;

        if queued >= count {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "only {queued} of {count} racers queued behind the lock",
        );
        tokio::time::sleep(QUEUING_POLL).await;
    }
}

// ---------------------------------------------------------------------------
// The first administrator
// ---------------------------------------------------------------------------

/// The race the advisory lock in `tenancy::first_administrator` exists for. Two
/// instances of a deployment boot together, both find no users at all, and
/// without ordering both act on that answer: one of them loses to the unique
/// index on the address it was told to create and fails its boot, which is a
/// crash loop rather than a deployment.
///
/// They are made to arrive together the way the shared bootstrap's racers are.
/// A connection of the test's own holds `users` locked against writes, so the
/// first racer gets past "is anybody here?" and stops at its insert while the
/// second queues on the lock, which is exactly the interleaving that has to be
/// safe.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_instances_booting_together_seed_one_administrator() {
    let Some(database) = TestDatabase::create("concurrency_flow").await else {
        return;
    };
    let config = Harness::config_with(&[
        ("ANUBIS_BOOTSTRAP_ADMIN_EMAIL", BOOTSTRAP_ADMIN_EMAIL),
        ("ANUBIS_BOOTSTRAP_ADMIN_PASSWORD", PASSWORD),
    ]);
    // A pool each, because two instances are two processes.
    let instances = [database.pool().await, database.pool().await];

    let mut blocker = AsyncPgConnection::establish(database.url())
        .await
        .expect("a blocking connection");
    let mut observer = AsyncPgConnection::establish(database.url())
        .await
        .expect("an observing connection");

    let racing = blocker
        .transaction::<_, diesel::result::Error, _>(async |transaction| {
            diesel::sql_query("LOCK TABLE users IN EXCLUSIVE MODE")
                .execute(transaction)
                .await?;

            let mut booting = Vec::with_capacity(instances.len());
            for pool in instances.clone() {
                let config = config.clone();
                booting.push(tokio::spawn(async move {
                    anubis::tenancy::seed_first_administrator(&pool, &config).await
                }));
            }
            wait_until_queued(&mut observer, instances.len()).await;
            Ok(booting)
        })
        .await
        .expect("holding the users lock must succeed");
    drop(blocker);

    for instance in racing {
        instance
            .await
            .expect("a booting instance must not panic")
            .expect("both instances must boot: seeding is safe to run everywhere");
    }

    let mut connection = instances[0].get().await.expect("connection");

    let accounts: i64 = users::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(accounts, 1, "one administrator, not one per instance");

    let organizations_created: i64 = organizations::table
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(
        organizations_created, 1,
        "and the one tenancy it was bootstrapped into",
    );

    let administers: i64 = organization_memberships::table
        .filter(organization_memberships::roles.contains(vec![anubis::tenancy::ADMIN_ROLE]))
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(
        administers, 1,
        "the account that exists is the one that can invite everybody else",
    );
}

// ---------------------------------------------------------------------------
// Realtime channels
// ---------------------------------------------------------------------------

/// Two sockets churning their subscriptions while events publish underneath
/// them. Neither may deadlock, and neither may ever see a frame for a channel
/// it is not entitled to: the socket that unsubscribed stops receiving, and
/// the socket that never subscribed never started.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sockets_churn_without_deadlocking_or_leaking_across_channels() {
    let Some(database) = TestDatabase::create("concurrency_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let address = harness.serve();

    let listener = register(&harness.router, "listener@example.com").await;
    let bystander = register(&harness.router, "bystander@example.com").await;
    let listener_id = signed_in_user(&harness, &listener).await;
    let bystander_id = signed_in_user(&harness, &bystander).await;

    let mut attentive = socket::connect(address, &listener).await;
    let mut standing_by = socket::connect(address, &bystander).await;

    let mine = ChannelName::user(listener_id, "inbox");
    let theirs = ChannelName::user(bystander_id, "inbox");

    let reply = socket::subscribe(&mut attentive, &mine.to_string()).await;
    assert_eq!(reply["type"], json!("subscribed"), "reply: {reply}");

    // The bystander subscribes and immediately drops it again, while events
    // publish the whole time.
    let reply = socket::subscribe(&mut standing_by, &theirs.to_string()).await;
    assert_eq!(reply["type"], json!("subscribed"), "reply: {reply}");
    socket::send(
        &mut standing_by,
        &json!({ "type": "unsubscribe", "channel": theirs.to_string() }),
    )
    .await;
    let reply = socket::next_frame(&mut standing_by).await;
    assert_eq!(reply["type"], json!("unsubscribed"), "reply: {reply}");

    for round in 0..RACERS {
        harness
            .channels
            .publish(&mine, "tick", json!({ "round": round }))
            .await
            .expect("publishing must succeed");
        harness
            .channels
            .publish(&theirs, "tick", json!({ "round": round }))
            .await
            .expect("publishing must succeed");
    }

    // The listener receives its own channel's rounds, in order and complete.
    for round in 0..RACERS {
        let frame = socket::next_frame(&mut attentive).await;
        assert_eq!(frame["type"], json!("event"), "frame: {frame}");
        assert_eq!(frame["channel"], json!(mine.to_string()), "frame: {frame}");
        assert_eq!(frame["payload"]["round"], json!(round), "frame: {frame}");
    }

    // A channel it never held cannot reach it, so the next thing this socket
    // hears is its own subscription again rather than the bystander's.
    harness
        .channels
        .publish(&mine, "last", json!({}))
        .await
        .expect("publishing must succeed");
    let frame = socket::next_frame(&mut attentive).await;
    assert_eq!(frame["event"], json!("last"), "frame: {frame}");

    // The unsubscribed socket heard nothing at all, which it proves by
    // answering the next request it is given rather than a queued event.
    socket::send(
        &mut standing_by,
        &json!({ "type": "unsubscribe", "channel": theirs.to_string() }),
    )
    .await;
    let frame = socket::next_frame(&mut standing_by).await;
    assert_eq!(
        frame,
        json!({ "type": "unsubscribed", "channel": theirs.to_string() }),
        "an unsubscribed socket received an event it should never have seen",
    );
}

/// The id of the account behind a session cookie.
async fn signed_in_user(harness: &Harness, cookie: &str) -> Uuid {
    let (status, _headers, body) =
        send(&harness.router, "GET", "/auth/me", None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    body["user"]["id"]
        .as_str()
        .expect("the profile names the user")
        .parse()
        .expect("a user id is a UUID")
}

// ---------------------------------------------------------------------------
// Outgoing webhooks
// ---------------------------------------------------------------------------

/// Emission rides the caller's own connection precisely so a delivery exists
/// exactly when the write that caused it committed. Racing writers, half of
/// them rolling back, is what proves it: the deliveries and their jobs must
/// count the commits and nothing else.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_emissions_deliver_exactly_the_committed_writes() {
    let Some(database) = TestDatabase::create("concurrency_flow").await else {
        return;
    };
    let harness = Harness::boot(&database).await;

    let admin = register(&harness.router, "publisher@example.com").await;
    let team_id = harness.bootstrapped_team(&admin).await;

    let mut connection = harness.pool.get().await.expect("connection");
    diesel::insert_into(webhook_endpoints::table)
        .values((
            webhook_endpoints::team_id.eq(team_id),
            webhook_endpoints::url.eq("https://example.com/hooks"),
            webhook_endpoints::event_types.eq(vec!["project.created".to_owned()]),
            webhook_endpoints::secret.eq("sealed-by-this-test"),
        ))
        .execute(&mut connection)
        .await
        .expect("the endpoint must insert");
    drop(connection);

    // Even writers commit, odd writers roll back.
    let mut writers = Vec::with_capacity(RACERS);
    for write in 0..RACERS {
        let pool = harness.pool.clone();
        writers.push(tokio::spawn(async move {
            let mut connection = pool.get().await.expect("connection");
            connection
                .transaction::<(), diesel::result::Error, _>(async |transaction| {
                    anubis::webhooks::emit(
                        transaction,
                        team_id,
                        "project.created",
                        &json!({ "write": write }),
                    )
                    .await?;
                    if write % 2 == 0 {
                        Ok(())
                    } else {
                        Err(diesel::result::Error::RollbackTransaction)
                    }
                })
                .await
        }));
    }
    for writer in writers {
        let _outcome = writer.await.expect("a writer must not panic");
    }

    let committed = i64::try_from(RACERS / 2).expect("the racer count fits");
    let mut connection = harness.pool.get().await.expect("connection");

    let payloads: Vec<Value> = webhook_deliveries::table
        .select(webhook_deliveries::payload)
        .load(&mut connection)
        .await
        .expect("the delivery query must run");
    assert_eq!(
        i64::try_from(payloads.len()).expect("the delivery count fits"),
        committed,
        "one delivery per committed write, and none for a rolled-back one",
    );
    for payload in &payloads {
        let write = payload["write"]
            .as_u64()
            .expect("the payload names its write");
        assert_eq!(
            write % 2,
            0,
            "a rolled-back write left a delivery: {payload}"
        );
    }

    let queued: i64 = jobs_table::table
        .filter(jobs_table::queue.eq(anubis::webhooks::QUEUE))
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(queued, committed, "one delivery job per delivery row");
}

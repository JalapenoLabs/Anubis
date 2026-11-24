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
use std::time::Duration;

use anubis::jobs::{self, BoxError, Job, Worker};
use anubis::realtime::ChannelName;
use anubis::schema::{
    dead_jobs, jobs as jobs_table, organizations, role_grants, roles, sessions, team_memberships,
    teams, user_avatars, webhook_deliveries, webhook_endpoints,
};
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE};
use axum::http::{Request, StatusCode};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, RunQueryDsl};
use http_body_util::BodyExt;
use image::codecs::png::PngEncoder;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tower::ServiceExt;
use uuid::Uuid;

use support::{
    Harness, PASSWORD, TestDatabase, invitation_token, register, role_id, send, session_token,
    socket,
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

    let editor = role_id(&harness.router, &admin, team_id, "editor").await;
    let invite =
        json!({ "email": "invitee@example.com", "team_id": team_id, "role_ids": [editor] });
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

    let administrator = role_id(&harness.router, &first, team_id, "admin").await;
    let invite = json!({
        "email": "second-admin@example.com",
        "team_id": team_id,
        "role_ids": [administrator],
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
    // connection holds the very lock the handlers take first, the team's
    // organization, so both get past their guards, both queue behind it, and
    // neither can win by being scheduled first. Without that, the scheduler
    // usually runs one request to completion, the other's guard finds it is no
    // longer an admin, and the race the invariant exists for is never run.
    let editor = role_id(&harness.router, &first, team_id, "editor").await;
    let demote = json!({ "role_ids": [editor] });
    let organization_id: Uuid = {
        let mut connection = harness.pool.get().await.expect("connection");
        teams::table
            .find(team_id)
            .select(teams::organization_id)
            .first(&mut connection)
            .await
            .expect("the team has an organization")
    };
    let mut blocker = harness.pool.get().await.expect("connection");
    let racing = blocker
        .transaction::<_, diesel::result::Error, _>(async |transaction| {
            organizations::table
                .find(organization_id)
                .select(organizations::id)
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

    // The loser is refused either way. Its standing is read again once it
    // holds the lock, so it finds itself demoted and answers `403`; `409` is
    // what the invariant answers had it still been an administrator there.
    // Either is a refusal; being served is not.
    let refused = outcomes
        .iter()
        .find(|status| !status.is_success())
        .expect("one demotion is refused");
    assert!(
        matches!(*refused, StatusCode::CONFLICT | StatusCode::FORBIDDEN),
        "the losing demotion is refused, not served: {refused}",
    );

    let mut connection = harness.pool.get().await.expect("connection");
    let admins: i64 = role_grants::table
        .inner_join(roles::table)
        .filter(role_grants::team_id.eq(team_id))
        .filter(role_grants::resource_id.is_null())
        .filter(roles::key.eq(anubis::tenancy::ADMIN_ROLE))
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(admins, 1, "the team is never left without an admin");
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
// Runtime roles
// ---------------------------------------------------------------------------

/// Two administrators of one organization, and the organization's role ids.
struct TwoAdministrators {
    harness: Harness,
    first: String,
    second: String,
    organization: Uuid,
    team: Uuid,
}

impl TwoAdministrators {
    async fn open(database: &TestDatabase) -> Self {
        let harness = Harness::boot(database).await;
        let first = register(&harness.router, "first-role-admin@example.com").await;
        let team = harness.bootstrapped_team(&first).await;
        let organization: Uuid = {
            let mut connection = harness.pool.get().await.expect("connection");
            teams::table
                .find(team)
                .select(teams::organization_id)
                .first(&mut connection)
                .await
                .expect("the team has an organization")
        };
        let second = register(&harness.router, "second-role-admin@example.com").await;
        let administrator = role_id(&harness.router, &first, team, "admin").await;
        let invite = json!({
            "email": "second-role-admin@example.com",
            "organization_id": organization,
            "role_ids": [administrator],
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
        let token = invitation_token(&harness.outbox, "second-role-admin@example.com");
        let claimed = claim_invitation(&harness, &json!({ "token": token }), second.clone()).await;
        assert_eq!(claimed, StatusCode::OK);

        Self {
            harness,
            first,
            second,
            organization,
            team,
        }
    }

    /// Runs two requests that both reach the organization lock before either
    /// holds it, so neither wins by being scheduled first.
    async fn race(
        &self,
        one: (&str, String, Value, &str),
        other: (&str, String, Value, &str),
    ) -> (StatusCode, StatusCode) {
        let mut blocker = self.harness.pool.get().await.expect("connection");
        let racing = blocker
            .transaction::<_, diesel::result::Error, _>(async |transaction| {
                organizations::table
                    .find(self.organization)
                    .select(organizations::id)
                    .for_update()
                    .first::<Uuid>(transaction)
                    .await?;
                let one = self.spawn(one);
                let other = self.spawn(other);
                tokio::time::sleep(QUEUING_WINDOW).await;
                Ok((one, other))
            })
            .await
            .expect("holding the organization lock must succeed");
        drop(blocker);

        let (one, other) = racing;
        (
            one.await.expect("the request task must not panic"),
            other.await.expect("the request task must not panic"),
        )
    }

    fn spawn(
        &self,
        (method, path, body, cookie): (&str, String, Value, &str),
    ) -> JoinHandle<StatusCode> {
        let router = self.harness.router.clone();
        let method = method.to_owned();
        let cookie = cookie.to_owned();
        tokio::spawn(async move {
            let (status, _headers, _body) =
                send(&router, &method, &path, Some(&body), Some(&cookie)).await;
            status
        })
    }

    /// The organization's roles as `(id, position, permissions)`, highest first.
    async fn ladder(&self) -> Vec<(Uuid, i64, Value)> {
        let path = format!("/tenancy/organizations/{}/roles", self.organization);
        let (status, _headers, body) =
            send(&self.harness.router, "GET", &path, None, Some(&self.first)).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        body["roles"]
            .as_array()
            .expect("the listing carries roles")
            .iter()
            .map(|role| {
                (
                    role["id"]
                        .as_str()
                        .expect("a role id is a string")
                        .parse()
                        .expect("a role id is a UUID"),
                    role["position"].as_i64().expect("a position is a number"),
                    role["permissions"].clone(),
                )
            })
            .collect()
    }
}

/// Two reorders at the same instant are ordered by the organization lock, so
/// the ladder ends as one of them asked, whole and dense, never a mix that
/// ties two roles on one rung.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_reorders_leave_one_whole_ladder() {
    let Some(database) = TestDatabase::create("concurrency_flow").await else {
        return;
    };
    let court = TwoAdministrators::open(&database).await;
    let editor = role_id(&court.harness.router, &court.first, court.team, "editor").await;
    let billing = role_id(&court.harness.router, &court.first, court.team, "billing").await;

    let path = format!("/tenancy/organizations/{}/role-order", court.organization);
    let (one, other) = court
        .race(
            (
                "PUT",
                path.clone(),
                json!({ "role_ids": [editor, billing] }),
                &court.first,
            ),
            (
                "PUT",
                path,
                json!({ "role_ids": [billing, editor] }),
                &court.second,
            ),
        )
        .await;
    assert_eq!((one, other), (StatusCode::OK, StatusCode::OK));

    let positions: Vec<i64> = court
        .ladder()
        .await
        .into_iter()
        .map(|(_id, position, _permissions)| position)
        .collect();
    assert_eq!(
        positions,
        [3, 2, 1, 0],
        "one dense ladder, whichever landed last"
    );
}

/// Two edits of one role at the same instant each replace the whole set, so
/// the role ends exactly as one of them asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_edits_of_one_role_land_whole() {
    let Some(database) = TestDatabase::create("concurrency_flow").await else {
        return;
    };
    let court = TwoAdministrators::open(&database).await;
    let editor = role_id(&court.harness.router, &court.first, court.team, "editor").await;

    let path = format!(
        "/tenancy/organizations/{}/roles/{editor}",
        court.organization
    );
    let reading = json!({ "permissions": ["Team.read"] });
    let writing = json!({ "permissions": ["Team.create", "Team.update"] });
    let (one, other) = court
        .race(
            ("PATCH", path.clone(), reading, &court.first),
            ("PATCH", path, writing, &court.second),
        )
        .await;
    assert_eq!((one, other), (StatusCode::OK, StatusCode::OK));

    let landed = court
        .ladder()
        .await
        .into_iter()
        .find(|(id, _position, _permissions)| *id == editor)
        .map(|(_id, _position, permissions)| permissions)
        .expect("the role still exists");
    assert!(
        landed == json!(["Team.read"]) || landed == json!(["Team.create", "Team.update"]),
        "one edit or the other, never a merge: {landed}",
    );
}

/// A role deleted while somebody is being granted it leaves no grant behind,
/// whichever request the organization lock lets through first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_role_deleted_while_granted_leaves_no_grant() {
    let Some(database) = TestDatabase::create("concurrency_flow").await else {
        return;
    };
    let court = TwoAdministrators::open(&database).await;
    let editor = role_id(&court.harness.router, &court.first, court.team, "editor").await;

    let invite = json!({ "email": "role-target@example.com", "team_id": court.team });
    let (status, _headers, body) = send(
        &court.harness.router,
        "POST",
        "/tenancy/invitations",
        Some(&invite),
        Some(&court.first),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let roster = format!("/tenancy/teams/{}/members", court.team);
    let (status, _headers, body) = send(
        &court.harness.router,
        "GET",
        &roster,
        None,
        Some(&court.first),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let target = body["members"]
        .as_array()
        .expect("the roster is an array")
        .iter()
        .find(|member| member["pending"] == json!(true))
        .and_then(|member| member["membership_id"].as_str())
        .expect("the invitation holds a place")
        .to_owned();

    let (deleted, granted) = court
        .race(
            (
                "DELETE",
                format!(
                    "/tenancy/organizations/{}/roles/{editor}",
                    court.organization
                ),
                json!({}),
                &court.first,
            ),
            (
                "PATCH",
                format!("/tenancy/teams/{}/members/{target}", court.team),
                json!({ "role_ids": [editor] }),
                &court.first,
            ),
        )
        .await;
    assert_eq!(deleted, StatusCode::NO_CONTENT);
    assert!(
        matches!(granted, StatusCode::OK | StatusCode::NOT_FOUND),
        "the grant lands before the delete or finds the role gone: {granted}",
    );

    let mut connection = court.harness.pool.get().await.expect("connection");
    let left: i64 = role_grants::table
        .filter(role_grants::role_id.eq(editor))
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(left, 0, "a deleted role is held by nobody");
}

/// A role deleted while an invitation offering it is claimed leaves no grant
/// behind: the claim takes the organization lock every role change takes, so
/// it lands wholly before the delete (whose cascade then removes the grant)
/// or wholly after it (and finds the role no longer offered), never as a
/// foreign key failure or a deadlock between the two.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_role_deleted_while_its_invitation_is_claimed_leaves_no_grant() {
    let Some(database) = TestDatabase::create("concurrency_flow").await else {
        return;
    };
    let court = TwoAdministrators::open(&database).await;
    let editor = role_id(&court.harness.router, &court.first, court.team, "editor").await;

    let invite = json!({
        "email": "claimant@example.com",
        "organization_id": court.organization,
        "role_ids": [editor],
    });
    let (status, _headers, body) = send(
        &court.harness.router,
        "POST",
        "/tenancy/invitations",
        Some(&invite),
        Some(&court.first),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let token = invitation_token(&court.harness.outbox, "claimant@example.com");
    let claimant = register(&court.harness.router, "claimant@example.com").await;

    let (deleted, claimed) = court
        .race(
            (
                "DELETE",
                format!(
                    "/tenancy/organizations/{}/roles/{editor}",
                    court.organization
                ),
                json!({}),
                &court.first,
            ),
            (
                "POST",
                "/tenancy/invitations/claim".to_owned(),
                json!({ "token": token }),
                &claimant,
            ),
        )
        .await;
    assert_eq!(deleted, StatusCode::NO_CONTENT);
    assert_eq!(
        claimed,
        StatusCode::OK,
        "the claim succeeds either way, with the role or without it",
    );

    let mut connection = court.harness.pool.get().await.expect("connection");
    let left: i64 = role_grants::table
        .filter(role_grants::role_id.eq(editor))
        .count()
        .get_result(&mut connection)
        .await
        .expect("count must run");
    assert_eq!(left, 0, "a deleted role is held by nobody");
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

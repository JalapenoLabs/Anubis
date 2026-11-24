//! Realtime over Server-Sent Events, end to end, in both backends.
//!
//! One narrative, run twice: once with everything in process, and once with
//! the sequence, the replay log, and the fanout in Redis, where the stream is
//! served by one instance, the events are published through a second, and the
//! client resumes on a third. It covers what a stream may carry (who may open
//! one and which channels it is refused), what it carries (payloads, exactly
//! as published, with ids), and how it picks up after a drop (an exact replay,
//! and a reset wherever an exact replay is no longer possible).
//!
//! Beside it, what only Redis can lose (its data, and with it the epoch) and
//! what only a running server can do (drain), each in a test of its own.
//!
//! Requires `DATABASE_URL`; the Redis tests also require `REDIS_URL`. Each
//! logs a skip and passes without them. CI provides both.

mod support;

use std::time::Duration;

use anubis::http::ApiError;
use anubis::realtime::{ChannelName, Channels, ChannelsBuilder, EventStream, Subscriber};
use anubis::schema::organization_memberships;
use axum::extract::Path;
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderValue, StatusCode};
use axum::routing::get;
use axum::{Extension, Router};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::json;
use support::stream::{self, Opening, Received};
use support::{Harness, TestDatabase, register, send};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

/// A topic whose channels keep two events, so a test can outrun its buffer.
const SHORT_TOPIC: &str = "burst";

/// A topic that keeps more events than a stream may fall behind by, so a
/// stream cut for falling behind can resume exactly.
const LONG_TOPIC: &str = "flood";

/// How many events the stalled stream is sent: more than its queue holds.
const FLOOD: usize = 300;

/// How many events one stream's queue holds before it is cut.
const BACKLOG: usize = 256;

/// One deployment's realtime, as the narrative sees it.
struct Deployment {
    /// The instance the browser streams from first.
    serving: Harness,
    /// What the application publishes through.
    publishing: Channels,
    /// The instance the browser resumes on.
    resuming: Harness,
}

/// The retention every instance in these tests builds with.
fn retention() -> ChannelsBuilder {
    Channels::builder()
        .retain(SHORT_TOPIC, 2)
        .retain(LONG_TOPIC, 1_024)
}

#[tokio::test]
async fn in_process_streams_authorize_deliver_and_resume() {
    let Some(database) = TestDatabase::create("realtime_flow in process").await else {
        return;
    };
    let channels = retention().build_in_process();
    let harness = Harness::boot_with_channels(&database, channels.clone()).await;

    narrative(Deployment {
        serving: harness.clone(),
        publishing: channels,
        resuming: harness,
    })
    .await;
}

#[tokio::test]
async fn redis_streams_authorize_deliver_and_resume_across_instances() {
    let Some(redis_url) = redis_url("realtime_flow redis") else {
        return;
    };
    let Some(database) = TestDatabase::create("realtime_flow redis").await else {
        return;
    };

    // Three independent handles on one Redis stand in for three instances:
    // nothing but Redis can carry an event, or a replay, between them.
    let mut instances = Vec::new();
    for _instance in 0..3 {
        let channels = retention()
            .build_redis(&redis_url)
            .await
            .expect("REDIS_URL must name a reachable Redis");
        instances.push(channels);
    }
    let [serving, publishing, resuming]: [Channels; 3] =
        instances.try_into().expect("three instances were built");

    narrative(Deployment {
        serving: Harness::boot_with_channels(&database, serving).await,
        publishing,
        resuming: Harness::boot_with_channels(&database, resuming).await,
    })
    .await;
}

#[expect(
    clippy::too_many_lines,
    reason = "one linear story: who may stream, what a stream carries, and how it resumes"
)]
async fn narrative(deployment: Deployment) {
    let router = deployment.serving.router.clone();
    let resuming = deployment.resuming.router.clone();
    let publishing = &deployment.publishing;

    let member = register(&router, "member@realtime.test").await;
    let outsider = register(&router, "outsider@realtime.test").await;
    let member_id = user_id(&router, &member).await;
    let outsider_id = user_id(&router, &outsider).await;
    let team_id = deployment.serving.bootstrapped_team(&member).await;

    let projects = ChannelName::team(team_id, "projects").to_string();
    let inbox = ChannelName::user(member_id, "inbox").to_string();

    // ------------------------------------------------------------------
    // Who may open a stream at all.
    // ------------------------------------------------------------------
    let (status, _body) = stream::open(&router, &stream::path(&[&projects], &[]), None, None)
        .await
        .expect_err("a stream without a session must be refused");
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, body) = stream::open(&router, "/realtime", Some(&member), None)
        .await
        .expect_err("a stream of nothing must be refused");
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    let many: Vec<String> = (0..65)
        .map(|index| ChannelName::user(member_id, format!("topic{index}")).to_string())
        .collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let (status, body) = stream::open(&router, &stream::path(&many, &[]), Some(&member), None)
        .await
        .expect_err("a stream past its channel limit must be refused");
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    // ------------------------------------------------------------------
    // What a stream may carry: each channel is authorized on its own.
    // ------------------------------------------------------------------
    let stranger = ChannelName::user(outsider_id, "inbox").to_string();
    let ghost = ChannelName::team(Uuid::new_v4(), "projects").to_string();
    let record = ChannelName::resource(team_id, "logs").to_string();
    let malformed = "team:not-a-uuid:projects";

    let mut listening = stream::connect(
        &router,
        &stream::path(
            &[&projects, &inbox, &stranger, &ghost, &record, malformed],
            &[],
        ),
        &member,
        None,
    )
    .await;
    assert_eq!(
        listening
            .headers
            .get(CACHE_CONTROL)
            .map(HeaderValue::as_bytes),
        Some(b"no-cache, no-transform".as_slice()),
    );
    assert_eq!(
        listening
            .headers
            .get("x-accel-buffering")
            .map(HeaderValue::as_bytes),
        Some(b"no".as_slice()),
        "a buffering proxy must be told to pass events straight through",
    );

    let opening = listening.opening().await;
    // With no cursor, a channel opens with a reset: read the state once live.
    assert_eq!(opening.resets(), vec![projects.as_str(), inbox.as_str()]);
    let refusals = [&stranger, &ghost, &record].map(|channel| refusal(&opening, channel));
    for refused in &refusals {
        assert_eq!(refused.frame["code"], json!("not_found"), "{refused:?}");
    }
    assert!(
        refusals
            .iter()
            .all(|refused| refused.frame["message"] == refusals[0].frame["message"]),
        "another user's channel, a team that does not exist, and a resource must read alike",
    );
    assert_eq!(
        refusal(&opening, malformed).frame["code"],
        json!("invalid_channel"),
    );

    // A non-member is refused the member's team exactly as a team that does
    // not exist, so a stream can never confirm that a team is there.
    let mut prying = stream::connect(
        &router,
        &stream::path(&[&projects, &ghost], &[]),
        &outsider,
        None,
    )
    .await;
    let pried = prying.opening().await;
    assert!(
        pried.resets().is_empty(),
        "nothing was the outsider's to hear"
    );
    let not_a_member = refusal(&pried, &projects).frame.clone();
    let no_such_team = refusal(&pried, &ghost).frame.clone();
    assert_eq!(not_a_member["code"], no_such_team["code"]);
    assert_eq!(not_a_member["message"], no_such_team["message"]);
    drop(prying);

    // ------------------------------------------------------------------
    // What a stream carries: every event, exactly as published, with its id.
    // ------------------------------------------------------------------
    let payload = json!({
        "id": 7,
        "name": "Apollo \u{1f680} \"quoted\"\nsecond line",
        "tags": ["a", "b"],
        "nested": { "ratio": 0.25, "missing": null },
    });
    let published = publishing
        .publish(&parse(&projects), "created", payload.clone())
        .await
        .expect("publishing must succeed");
    let created = listening.next().await;
    assert_eq!(created.id, Some(published.to_string()));
    assert_eq!(
        created.frame,
        json!({
            "type": "event",
            "channel": projects,
            "event": "created",
            "payload": payload,
        }),
    );

    let published = publishing
        .publish(&parse(&inbox), "created", json!({ "unread": 1 }))
        .await
        .expect("publishing must succeed");
    let pinged = listening.next().await;
    assert_eq!(pinged.id, Some(published.to_string()));
    assert_eq!(pinged.channel(), inbox);

    // Nothing for a channel the stream was refused ever arrives on it.
    publishing
        .publish(&parse(&stranger), "created", json!({}))
        .await
        .expect("publishing must succeed");
    assert!(
        listening.stays_quiet(Duration::from_millis(300)).await,
        "a refused channel must deliver nothing",
    );

    // ------------------------------------------------------------------
    // Resume: what was missed while away arrives, in order, with no reset.
    // ------------------------------------------------------------------
    let cursor = pinged.id.clone().expect("an event carries its id");
    drop(listening);

    let mut missed = Vec::new();
    for (channel, event) in [
        (&projects, "updated"),
        (&inbox, "read"),
        (&projects, "deleted"),
    ] {
        let id = publishing
            .publish(&parse(channel), event, json!({ "event": event }))
            .await
            .expect("publishing must succeed");
        missed.push((id.to_string(), channel.clone(), event.to_owned()));
    }

    let mut resumed = stream::connect(
        &resuming,
        &stream::path(&[&projects, &inbox], &[]),
        &member,
        Some(&cursor),
    )
    .await;
    let opening = resumed.opening().await;
    assert!(
        opening.resets().is_empty(),
        "an exact replay needs no reset: {opening:?}"
    );
    let replayed: Vec<(String, String, String)> = opening
        .of_kind("event")
        .into_iter()
        .map(|event| {
            (
                event.id.clone().unwrap_or_default(),
                event.channel().to_owned(),
                event.frame["event"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        replayed, missed,
        "the replay is every missed event, in order"
    );

    // And the resumed stream is live.
    let published = publishing
        .publish(&parse(&projects), "created", json!({ "id": 10 }))
        .await
        .expect("publishing must succeed");
    let live = resumed.next().await;
    assert_eq!(live.id, Some(published.to_string()));
    let cursor = live.id.clone().expect("an event carries its id");
    drop(resumed);

    // ------------------------------------------------------------------
    // A channel joining a stream does not inherit its cursor.
    // ------------------------------------------------------------------
    // The view that wants this channel fetched its state just now, so an
    // event published before that must not be replayed onto it, however old
    // the cursor the stream resumes from.
    let alerts = ChannelName::user(member_id, "alerts").to_string();
    publishing
        .publish(
            &parse(&alerts),
            "created",
            json!({ "before": "the view fetched" }),
        )
        .await
        .expect("publishing must succeed");
    let mut widened = stream::connect(
        &resuming,
        &stream::path(&[&projects, &inbox, &alerts], &[&alerts]),
        &member,
        Some(&cursor),
    )
    .await;
    let opening = widened.opening().await;
    assert_eq!(
        opening.resets(),
        vec![alerts.as_str()],
        "the joining channel opens live with a reset; the others resume",
    );
    assert!(opening.of_kind("event").is_empty(), "{opening:?}");
    drop(widened);

    // ------------------------------------------------------------------
    // Past the buffer: a reset, never a quiet gap.
    // ------------------------------------------------------------------
    let burst = ChannelName::team(team_id, SHORT_TOPIC).to_string();
    let mut short = stream::connect(&router, &stream::path(&[&burst], &[]), &member, None).await;
    let cursor = short.opening().await.cursor;
    drop(short);
    for round in 0..3 {
        publishing
            .publish(&parse(&burst), "tick", json!({ "round": round }))
            .await
            .expect("publishing must succeed");
    }
    let mut outrun = stream::connect(
        &resuming,
        &stream::path(&[&burst, &inbox], &[]),
        &member,
        Some(&cursor),
    )
    .await;
    let opening = outrun.opening().await;
    assert_eq!(
        opening.resets(),
        vec![burst.as_str()],
        "the channel that dropped a missed event resets; the other replays exactly",
    );
    assert!(opening.of_kind("event").is_empty(), "{opening:?}");
    drop(outrun);

    // ------------------------------------------------------------------
    // A cursor this deployment never minted resets everything it names.
    // ------------------------------------------------------------------
    let foreign_epoch = other_epoch(&cursor);
    for hostile in ["not-an-id", "00000000000000ab-1-2", foreign_epoch.as_str()] {
        let mut wary = stream::connect(
            &resuming,
            &stream::path(&[&projects, &inbox], &[]),
            &member,
            Some(hostile),
        )
        .await;
        let opening = wary.opening().await;
        assert_eq!(
            opening.resets(),
            vec![projects.as_str(), inbox.as_str()],
            "{hostile:?} must not be resumed from",
        );
        assert!(
            opening.of_kind("event").is_empty(),
            "{hostile:?}: {opening:?}"
        );
    }

    // ------------------------------------------------------------------
    // A stream that falls behind is cut, and resumes exactly.
    // ------------------------------------------------------------------
    let flood = ChannelName::team(team_id, LONG_TOPIC).to_string();
    let mut stalled = stream::connect(&router, &stream::path(&[&flood], &[]), &member, None).await;
    let _opened = stalled.opening().await;
    // Nothing reads the stalled stream while these publish, which is exactly
    // what a client on a stuck connection looks like to the server.
    for round in 0..FLOOD {
        publishing
            .publish(&parse(&flood), "tick", json!({ "round": round }))
            .await
            .expect("publishing must succeed");
    }
    let heard = stalled.until_end().await;
    assert_eq!(
        heard.len(),
        BACKLOG,
        "the queue drains, then the stream ends"
    );
    assert_rounds(&heard, 0);
    let cursor = heard
        .last()
        .and_then(|event| event.id.clone())
        .expect("an event carries its id");

    let mut caught_up = stream::connect(
        &resuming,
        &stream::path(&[&flood], &[]),
        &member,
        Some(&cursor),
    )
    .await;
    let opening = caught_up.opening().await;
    assert!(
        opening.resets().is_empty(),
        "the buffer still holds what was cut"
    );
    let rest: Vec<Received> = opening.of_kind("event").into_iter().cloned().collect();
    assert_eq!(rest.len(), FLOOD - BACKLOG);
    assert_rounds(&rest, BACKLOG);
    drop(caught_up);

    // ------------------------------------------------------------------
    // A suspension closes the team's channels, as it closes its routes.
    // ------------------------------------------------------------------
    let mut connection = deployment
        .serving
        .pool
        .get()
        .await
        .expect("a connection must be available");
    diesel::update(
        organization_memberships::table.filter(organization_memberships::user_id.eq(member_id)),
    )
    .set(organization_memberships::suspended_at.eq(diesel::dsl::now))
    .execute(&mut connection)
    .await
    .expect("the suspension must apply");
    drop(connection);

    let mut suspended =
        stream::connect(&router, &stream::path(&[&projects], &[]), &member, None).await;
    let opening = suspended.opening().await;
    assert!(
        opening.resets().is_empty(),
        "a suspended member hears nothing of the team"
    );
    assert_eq!(
        refusal(&opening, &projects).frame["message"],
        no_such_team["message"],
        "a suspension reads exactly like a team that does not exist",
    );
}

/// An application's own stream, through `Subscriber` and `Channels::stream`:
/// the session check is part of the extractor, a first connection backfills
/// what the channel keeps, a resume is exact, and a resume the buffer cannot
/// vouch for resets and then refills from what is kept.
#[tokio::test]
async fn an_application_stream_backfills_resumes_and_refills_after_a_reset() {
    let Some(database) = TestDatabase::create("realtime_flow application stream").await else {
        return;
    };
    let channels = Channels::builder().retain("logs", 3).build_in_process();
    let harness = Harness::boot_with_channels(&database, channels.clone()).await;
    let member = register(&harness.router, "logs@realtime.test").await;
    let server_id = Uuid::new_v4();
    let logs = ChannelName::resource(server_id, "logs");

    let router = Router::new()
        .route("/servers/{server_id}/logs", get(tail))
        .layer(Extension(channels.clone()))
        .layer(Extension(harness.pool.clone()));
    let path = format!("/servers/{server_id}/logs");

    // The session check is the extractor's, so no handler can forget it.
    let (status, _body) = stream::open(&router, &path, None, None)
        .await
        .expect_err("a stream without a session must be refused");
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    for line in ["booting", "ready"] {
        channels
            .publish(&logs, "line", json!({ "text": line }))
            .await
            .expect("publishing must succeed");
    }

    // A first connection opens with what the channel keeps, and no reset.
    let mut tailing = stream::connect(&router, &path, &member, None).await;
    let opening = tailing.opening().await;
    assert!(opening.resets().is_empty(), "{opening:?}");
    assert_eq!(texts(&opening), vec!["booting", "ready"]);
    let cursor = opening.cursor.clone();
    drop(tailing);

    // A resume within what is kept replays exactly what was missed.
    channels
        .publish(&logs, "line", json!({ "text": "player joined" }))
        .await
        .expect("publishing must succeed");
    let mut resumed = stream::connect(&router, &path, &member, Some(&cursor)).await;
    let opening = resumed.opening().await;
    assert!(opening.resets().is_empty(), "{opening:?}");
    assert_eq!(texts(&opening), vec!["player joined"]);
    let cursor = opening.cursor.clone();
    drop(resumed);

    // Five more lines through a buffer of three: the tail clears what it
    // shows, then fills again from what is still kept, rather than going blank.
    for index in 0..5 {
        channels
            .publish(&logs, "line", json!({ "text": format!("tick {index}") }))
            .await
            .expect("publishing must succeed");
    }
    let mut refilled = stream::connect(&router, &path, &member, Some(&cursor)).await;
    let opening = refilled.opening().await;
    assert_eq!(opening.resets(), vec![logs.to_string().as_str()]);
    assert_eq!(texts(&opening), vec!["tick 2", "tick 3", "tick 4"]);
}

/// `GET /servers/{server_id}/logs`, as an application would write it.
async fn tail(
    subscriber: Subscriber,
    Path(server_id): Path<Uuid>,
    Extension(channels): Extension<Channels>,
) -> Result<EventStream, ApiError> {
    // The application's own authorization, against `subscriber.user()`,
    // goes here; this test lets every signed-in account read every log.
    let stream = channels
        .stream(
            subscriber.backfill(),
            [ChannelName::resource(server_id, "logs")],
        )
        .await?;
    Ok(stream)
}

/// The text of every log line an opening replayed, in order.
fn texts(opening: &Opening) -> Vec<String> {
    opening
        .of_kind("event")
        .into_iter()
        .map(|event| {
            event.frame["payload"]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

/// A Redis that restarted from an older point, or a replica promoted in its
/// place, has a new `run_id`; the log starts a new epoch rather than mint ids
/// clients already hold for other events.
#[tokio::test]
async fn a_redis_that_came_back_from_an_older_point_starts_a_new_epoch() {
    let Some(redis_url) = redis_url("realtime_flow rewind") else {
        return;
    };
    let Some(database) = TestDatabase::create("realtime_flow rewind").await else {
        return;
    };
    let isolated = redis_database(&redis_url, 13);
    flush(&isolated).await;

    let channels = Channels::redis(&isolated)
        .await
        .expect("REDIS_URL must name a reachable Redis");
    let harness = Harness::boot_with_channels(&database, channels.clone()).await;
    let member = register(&harness.router, "rewind@realtime.test").await;
    let inbox = ChannelName::user(user_id(&harness.router, &member).await, "inbox");
    let path = stream::path(&[&inbox.to_string()], &[]);

    let mut listening = stream::connect(&harness.router, &path, &member, None).await;
    let _opened = listening.opening().await;
    let mut cursor = String::new();
    for round in 0..3 {
        channels
            .publish(&inbox, "created", json!({ "round": round }))
            .await
            .expect("publishing must succeed");
        cursor = listening.next().await.id.expect("an event carries its id");
    }

    // What a restore from an older snapshot leaves behind: the same epoch, a
    // sequence that went backwards, and a server that is not the one that
    // wrote it. Only the last is visible to a script, so it is the one that
    // has to decide.
    let client = redis::Client::open(isolated.as_str()).expect("the Redis URL must parse");
    let mut connection = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Redis must be reachable");
    redis::cmd("HSET")
        .arg("{anubis:realtime}:log")
        .arg("sequence")
        .arg(1)
        .arg("run_id")
        .arg("0000000000000000000000000000000000000000")
        .query_async::<()>(&mut connection)
        .await
        .expect("the log must accept the rewind");

    let reissued = channels
        .publish(&inbox, "created", json!({ "round": "after" }))
        .await
        .expect("publishing must succeed")
        .to_string();
    assert_ne!(
        epoch_of(&reissued),
        epoch_of(&cursor),
        "a rewound sequence must not mint ids under the old epoch",
    );
    // The open stream hears an epoch it did not start in, and ends.
    assert!(listening.until_end().await.is_empty());

    let mut resumed = stream::connect(&harness.router, &path, &member, Some(&cursor)).await;
    let opening = resumed.opening().await;
    assert_eq!(opening.resets(), vec![inbox.to_string().as_str()]);
    assert!(opening.of_kind("event").is_empty(), "{opening:?}");
}

/// A Redis that lost its data starts a new epoch, and every stream and cursor
/// from the old one is told to read its state again.
#[tokio::test]
async fn a_redis_that_lost_its_data_resumes_every_client_with_a_reset() {
    let Some(redis_url) = redis_url("realtime_flow epoch") else {
        return;
    };
    let Some(database) = TestDatabase::create("realtime_flow epoch").await else {
        return;
    };
    // A database of the test's own, because this test empties it.
    let isolated = redis_database(&redis_url, 15);
    flush(&isolated).await;

    let channels = Channels::redis(&isolated)
        .await
        .expect("REDIS_URL must name a reachable Redis");
    let harness = Harness::boot_with_channels(&database, channels.clone()).await;
    let member = register(&harness.router, "epoch@realtime.test").await;
    let inbox = ChannelName::user(user_id(&harness.router, &member).await, "inbox");
    let path = stream::path(&[&inbox.to_string()], &[]);

    let mut listening = stream::connect(&harness.router, &path, &member, None).await;
    let cursor = listening.opening().await.cursor;

    flush(&isolated).await;
    channels
        .publish(&inbox, "created", json!({}))
        .await
        .expect("publishing after the loss starts a new epoch");

    // The open stream hears an event from an epoch it did not start in, so it
    // ends rather than deliver it after a gap it cannot see.
    assert!(listening.until_end().await.is_empty());

    let mut resumed = stream::connect(&harness.router, &path, &member, Some(&cursor)).await;
    let opening = resumed.opening().await;
    assert_eq!(opening.resets(), vec![inbox.to_string().as_str()]);
    assert_ne!(
        epoch_of(&opening.cursor),
        epoch_of(&cursor),
        "the loss is a new epoch",
    );
}

/// Under a byte budget, Redis lets go of whole channels, least recently
/// published first, and resuming on one that was let go is a reset.
#[tokio::test]
async fn the_redis_budget_lets_go_of_whole_channels_and_says_so() {
    let Some(redis_url) = redis_url("realtime_flow budget") else {
        return;
    };
    let Some(database) = TestDatabase::create("realtime_flow budget").await else {
        return;
    };
    // A database of the test's own: a budget this small would let go of
    // every other test's buffers too.
    let isolated = redis_database(&redis_url, 14);
    flush(&isolated).await;

    let channels = Channels::builder()
        .replay_budget(4 * 1024)
        .build_redis(&isolated)
        .await
        .expect("REDIS_URL must name a reachable Redis");
    let harness = Harness::boot_with_channels(&database, channels.clone()).await;
    let member = register(&harness.router, "budget@realtime.test").await;
    let member_id = user_id(&harness.router, &member).await;
    let oldest = ChannelName::user(member_id, "oldest").to_string();
    let quiet = ChannelName::user(member_id, "quiet").to_string();

    let mut first = stream::connect(
        &harness.router,
        &stream::path(&[&oldest], &[]),
        &member,
        None,
    )
    .await;
    let before = first.opening().await.cursor;
    drop(first);

    channels
        .publish(&parse(&oldest), "created", json!({}))
        .await
        .expect("publishing must succeed");
    // Enough other channels, each published once, to push the oldest out.
    for index in 0..16 {
        channels
            .publish(
                &ChannelName::user(member_id, format!("crowd{index}")),
                "created",
                json!({ "index": index }),
            )
            .await
            .expect("publishing must succeed");
    }

    let mut resumed = stream::connect(
        &harness.router,
        &stream::path(&[&oldest], &[]),
        &member,
        Some(&before),
    )
    .await;
    assert_eq!(
        resumed.opening().await.resets(),
        vec![oldest.as_str()],
        "the event the client missed was let go with its channel",
    );

    // A cursor from after everything that was let go resumes exactly, even
    // on a channel that never had a buffer.
    let mut later = stream::connect(
        &harness.router,
        &stream::path(&[&quiet], &[]),
        &member,
        None,
    )
    .await;
    let after = later.opening().await.cursor;
    drop(later);
    let mut exact = stream::connect(
        &harness.router,
        &stream::path(&[&quiet], &[]),
        &member,
        Some(&after),
    )
    .await;
    let opening = exact.opening().await;
    assert!(opening.resets().is_empty(), "{opening:?}");
    assert!(opening.of_kind("event").is_empty(), "{opening:?}");
}

/// A server that starts draining ends its open streams at once, rather than
/// holding the drain for its whole window.
#[tokio::test]
async fn a_draining_server_ends_its_streams_and_stops_without_waiting_them_out() {
    let Some(database) = TestDatabase::create("realtime_flow drain").await else {
        return;
    };
    let harness = Harness::boot(&database).await;
    let member = register(&harness.router, "drain@realtime.test").await;
    let inbox = ChannelName::user(user_id(&harness.router, &member).await, "inbox");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port must be available");
    let address = listener.local_addr().expect("the listener is bound");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(anubis::server::serve_with_shutdown(
        listener,
        harness.router.clone(),
        async move {
            let _stopped = stopped.await;
        },
    ));

    let mut connection = tokio::net::TcpStream::connect(address)
        .await
        .expect("the server accepts");
    let request = format!(
        "GET {} HTTP/1.1\r\nhost: test\r\ncookie: anubis_session={member}\r\n\r\n",
        stream::path(&[&inbox.to_string()], &[]),
    );
    connection
        .write_all(request.as_bytes())
        .await
        .expect("the request is written");

    // Read until the stream is live, so shutdown meets an open stream.
    let mut received = Vec::new();
    let mut chunk = [0_u8; 4096];
    while !String::from_utf8_lossy(&received).contains(r#""type":"live""#) {
        let read = tokio::time::timeout(Duration::from_secs(5), connection.read(&mut chunk))
            .await
            .expect("the stream opens")
            .expect("the connection is readable");
        assert!(read > 0, "the stream closed before it was live");
        received.extend_from_slice(&chunk[..read]);
    }

    let _stopping = stop.send(());
    let ended = tokio::time::timeout(
        Duration::from_secs(5),
        connection.read_to_end(&mut received),
    )
    .await;
    assert!(ended.is_ok(), "the stream must end when the drain starts");

    tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .expect("the server stops well inside its drain window")
        .expect("the serve task does not panic")
        .expect("the server stops cleanly");
}

/// `REDIS_URL`, or a logged skip naming `narrative`.
fn redis_url(narrative: &str) -> Option<String> {
    let url = std::env::var("REDIS_URL").ok();
    if url.is_none() {
        eprintln!("skipping {narrative}: REDIS_URL is not set");
    }
    url
}

/// The same Redis, numbered database `index`.
fn redis_database(url: &str, index: u8) -> String {
    let mut parsed = url::Url::parse(url).expect("REDIS_URL must be a URL");
    parsed.set_path(&format!("/{index}"));
    parsed.to_string()
}

/// Empties one Redis database, as a Redis restarted without persistence is.
async fn flush(url: &str) {
    let client = redis::Client::open(url).expect("the Redis URL must parse");
    let mut connection = redis::aio::ConnectionManager::new(client)
        .await
        .expect("Redis must be reachable");
    redis::cmd("FLUSHDB")
        .query_async::<()>(&mut connection)
        .await
        .expect("the database must flush");
}

/// The channel name a test wrote out, as the publisher takes it.
fn parse(channel: &str) -> ChannelName {
    channel.parse().expect("the test names valid channels")
}

/// The refusal an opening carries for `channel`.
fn refusal<'opening>(opening: &'opening Opening, channel: &str) -> &'opening Received {
    opening
        .refusal(channel)
        .unwrap_or_else(|| panic!("{channel} must be refused: {opening:?}"))
}

/// Asserts `events` are consecutive rounds, counting up from `first`.
fn assert_rounds(events: &[Received], first: usize) {
    for (offset, event) in events.iter().enumerate() {
        assert_eq!(
            event.frame["payload"]["round"],
            json!(first + offset),
            "rounds must arrive in order and complete: {event:?}",
        );
    }
}

/// The epoch half of an id.
fn epoch_of(id: &str) -> &str {
    id.split_once('-').map_or(id, |(epoch, _sequence)| epoch)
}

/// A well-formed id from an epoch other than `id`'s: its first digit flipped.
fn other_epoch(id: &str) -> String {
    let (epoch, sequence) = id.split_once('-').expect("an id is epoch-sequence");
    let flipped = if epoch.starts_with('0') { '1' } else { '0' };
    format!("{flipped}{}-{sequence}", &epoch[1..])
}

/// The signed-in user's own id.
async fn user_id(router: &axum::Router, cookie: &str) -> Uuid {
    let (status, _headers, body) = send(router, "GET", "/auth/me", None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    body["user"]["id"]
        .as_str()
        .expect("a signed-in user has an id")
        .parse()
        .expect("a user id is a UUID")
}

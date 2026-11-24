//! `GET /realtime`: one stream carrying every channel a page listens to.
//!
//! The session cookie authenticates the request, and each channel it names is
//! authorized on its own against the name, so a stream can never carry more
//! than the person behind it may hear. The channels are in the query string,
//! `?channels=team:{id}:projects,user:{id}:notifications`, and a client
//! changing what it listens to opens a new stream that resumes the old one.

use std::collections::HashSet;

use axum::extract::Query;
use axum::routing::get;
use axum::{Extension, Router};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde::Deserialize;
use uuid::Uuid;

use super::channel::{Audience, ChannelName};
use super::protocol::ErrorCode;
use super::stream::{EventStream, Subscriber};
use super::{Channels, MAX_CHANNELS_PER_STREAM, Refusal};
use crate::auth::User;
use crate::db::DbPool;
use crate::guard::admitted_teams;
use crate::http::ApiError;
use crate::schema::teams;

/// Where the endpoint is mounted, and what `crate::spa` reserves for it.
///
/// Public so an application can name the route, in a proxy rule or a probe,
/// without repeating the literal.
pub const PATH: &str = "/realtime";

/// What separates channel names in the query string.
///
/// A comma is not in the topic alphabet or a UUID, so it can never split a
/// name in two.
const SEPARATOR: char = ',';

/// Mounts `GET /realtime` on its own router.
///
/// Merge it into the application router alongside the framework's other
/// routers. The extensions the endpoint needs travel with it, so nothing else
/// has to be layered for it to work, and the request timeout needs no
/// exception for it: the timeout clocks the handler, which answers once the
/// stream is open, never the body that follows.
///
/// ```no_run
/// # fn compose(pool: anubis::db::DbPool, channels: anubis::realtime::Channels) -> axum::Router {
/// axum::Router::new().merge(anubis::realtime::router(pool, channels))
/// # }
/// ```
pub fn router(pool: DbPool, channels: Channels) -> Router {
    Router::new()
        .route(PATH, get(open))
        .layer(Extension(channels))
        .layer(Extension(pool))
}

/// What a request names in its query string.
#[derive(Debug, Deserialize)]
struct Request {
    /// Every channel the stream should carry, comma-separated.
    #[serde(default)]
    channels: String,
    /// The channels among them that do not continue `Last-Event-ID`, because
    /// the connection that sent it did not carry them. They open at the head
    /// of the sequence, with a reset, instead of replaying from a cursor that
    /// was never theirs.
    #[serde(default)]
    fresh: String,
}

/// Authorizes each channel the request names, then opens the stream.
///
/// A channel the subscriber may not hear, a channel that does not exist, and
/// a resource channel, which only an application's own endpoint may serve,
/// are refused identically, inside the stream, so one bad name never costs a
/// page its other channels.
async fn open(
    subscriber: Subscriber,
    Extension(channels): Extension<Channels>,
    Extension(pool): Extension<DbPool>,
    Query(request): Query<Request>,
) -> Result<EventStream, ApiError> {
    let mut seen = HashSet::new();
    let requested: Vec<&str> = request
        .channels
        .split(SEPARATOR)
        .filter(|name| !name.is_empty() && seen.insert(*name))
        .collect();
    if requested.is_empty() {
        return Err(ApiError::validation("Name at least one channel to stream."));
    }
    if requested.len() > MAX_CHANNELS_PER_STREAM {
        return Err(ApiError::validation(format!(
            "A stream may carry at most {MAX_CHANNELS_PER_STREAM} channels."
        )));
    }
    let fresh: HashSet<&str> = request.fresh.split(SEPARATOR).collect();

    let mut refusals = Vec::new();
    let mut named = Vec::with_capacity(requested.len());
    for name in requested {
        match name.parse::<ChannelName>() {
            Ok(channel) => named.push((name, channel)),
            Err(_error) => refusals.push(Refusal::new(name, ErrorCode::InvalidChannel)),
        }
    }

    let teams = admitted_team_ids(&pool, subscriber.user(), &named).await?;
    let mut requests = Vec::with_capacity(named.len());
    for (name, channel) in named {
        let admitted = match channel.audience() {
            Audience::User(user_id) => user_id == subscriber.user().id,
            Audience::Team(team_id) => teams.contains(&team_id),
            // No name says who may hear a record's stream; the application's
            // own endpoint for it does.
            Audience::Resource(_) => false,
        };
        if admitted {
            let start = subscriber.start(!fresh.contains(name));
            requests.push((channel, start));
        } else {
            refusals.push(Refusal::new(name, ErrorCode::NotFound));
        }
    }

    Ok(channels.open(subscriber, requests, refusals).await?)
}

/// Which of the teams `channels` name `user` is admitted to, in one query.
///
/// The rule is [`admitted_teams`], the same one every team route's guard
/// applies, so a member a route refuses, a suspended one above all, is never
/// told about the team on a channel either. A team that does not exist and a
/// team the user may not reach are equally absent from the answer.
async fn admitted_team_ids(
    pool: &DbPool,
    user: &User,
    channels: &[(&str, ChannelName)],
) -> Result<HashSet<Uuid>, ApiError> {
    let wanted: Vec<Uuid> = channels
        .iter()
        .filter_map(|(_name, channel)| match channel.audience() {
            Audience::Team(team_id) => Some(team_id),
            Audience::User(_) | Audience::Resource(_) => None,
        })
        .collect();
    if wanted.is_empty() {
        return Ok(HashSet::new());
    }

    let mut connection = pool.get().await.map_err(|error| {
        tracing::error!(
            error.message = %error,
            "realtime authorization could not reach the database: {{error.message}}",
        );
        ApiError::unavailable("Live updates are unavailable right now. Try again shortly.")
    })?;

    let admitted: Vec<Uuid> = admitted_teams(user.id)
        .filter(teams::id.eq_any(wanted))
        .select(teams::id)
        .load(&mut connection)
        .await?;

    Ok(admitted.into_iter().collect())
}

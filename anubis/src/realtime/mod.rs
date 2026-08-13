//! Realtime channels: the server publishes, browsers subscribe.
//!
//! [`Channels`] is the one service events are published through, from a
//! handler or from a background job. Browsers receive them over a single
//! websocket at `GET /realtime`, mounted by [`router`], authenticated by the
//! same session cookie the rest of the framework uses. Everything about the
//! wire contract, the namespace, and running this across several instances is
//! documented for application authors in `docs/realtime.md`.
//!
//! ```no_run
//! # async fn example(channels: &anubis::realtime::Channels, team_id: uuid::Uuid)
//! # -> Result<(), anubis::realtime::Error> {
//! use anubis::realtime::ChannelName;
//! use serde_json::json;
//!
//! channels
//!     .publish(
//!         &ChannelName::team(team_id, "projects"),
//!         "created",
//!         json!({ "id": 42, "name": "Apollo" }),
//!     )
//!     .await?;
//! # Ok(())
//! # }
//! ```
//!
//! # The namespace
//!
//! Channel names are structured, never free-form: `team:{team_id}:{topic}` or
//! `user:{user_id}:{topic}`. The audience in the name *is* the authorization
//! rule, so the server can answer a subscribe request without a table of
//! hand-written rules: a team channel admits that team's members, a user
//! channel admits that one user. An unauthorized subscribe is refused exactly
//! as a nonexistent one is, so a browser cannot probe for teams it does not
//! belong to. See [`ChannelName`].
//!
//! # The two backends
//!
//! Fanout is in-process by default: a publish reaches the subscribers attached
//! to this instance and costs nothing else, which is the right shape for the
//! single-instance deployment the framework targets out of the box. Setting
//! `REDIS_URL` switches the fanout to Redis pub/sub, where a publish reaches
//! the subscribers of *every* instance. Nothing else changes: the same
//! [`Channels`] API, the same frames, the same authorization.
//!
//! Redis stays a fanout in both cases. Nothing here is durable, and no state
//! this module holds survives a restart, which is why the blessed architecture
//! can keep Redis optional. A client that misses events while disconnected
//! refetches through the REST API; realtime is how it learns to, not where the
//! data lives.
//!
//! # Bounded by live interest
//!
//! A channel exists only while somebody is listening to it: the last
//! subscriber to leave closes it, and, under Redis, unsubscribes the process
//! from it. Subscriptions are also capped, at 64 channels per socket and
//! 16,384 channels per process, because channel names arrive from browsers.

mod channel;
mod fanout;
mod protocol;
mod registry;
mod socket;

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;

use serde::Serialize;

use crate::config::AppConfig;
use fanout::RedisFanout;
use protocol::ServerFrame;
use registry::{Registry, Subscription};

#[doc(inline)]
pub use channel::{Audience, ChannelName, ParseError};
#[doc(inline)]
pub use socket::router;

/// Publishes realtime events. Cheap to clone.
///
/// Build one at startup with [`Channels::from_config`], hand it to
/// [`router`], and clone it into anything that publishes. Every clone shares
/// one registry and one fanout backend.
#[derive(Debug, Clone)]
pub struct Channels {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    registry: Arc<Registry>,
    backend: Backend,
}

/// How a publish reaches subscribers.
#[derive(Debug)]
enum Backend {
    /// Straight into this process's registry.
    InProcess,
    /// Out through Redis, and back into every instance's registry.
    Redis(RedisFanout),
}

impl Channels {
    /// Channels served entirely inside this process. The default.
    ///
    /// Publishing reaches the browsers connected to this instance and nothing
    /// else, which is exactly right until an application runs more than one.
    #[must_use]
    pub fn in_process() -> Self {
        Self {
            inner: Arc::new(Inner {
                registry: Registry::new(None),
                backend: Backend::InProcess,
            }),
        }
    }

    /// Channels fanned out through Redis pub/sub, for several instances.
    ///
    /// `url` is a `redis://` or `rediss://` URL, from `REDIS_URL`. Redis is
    /// contacted here rather than at the first publish, so a deployment that
    /// names an unreachable Redis fails at startup instead of silently losing
    /// events.
    ///
    /// # Errors
    /// Returns an [`Error`] when the URL does not parse or Redis cannot be
    /// reached.
    pub async fn redis(url: &str) -> Result<Self, Error> {
        // The registry announces what this process is listening to, and the
        // pump follows those announcements. The pump holds only a weak handle
        // on the registry, so it stops when the last clone of this service is
        // dropped.
        let (announcements, interest) = tokio::sync::mpsc::unbounded_channel();
        let registry = Registry::new(Some(announcements));
        let fanout = RedisFanout::connect(url, interest, Arc::downgrade(&registry))
            .await
            .map_err(|source| Error::new(ErrorKind::Configuration(source)))?;

        Ok(Self {
            inner: Arc::new(Inner {
                registry,
                backend: Backend::Redis(fanout),
            }),
        })
    }

    /// The channels the configuration asks for.
    ///
    /// Redis wherever `REDIS_URL` is set, in-process everywhere else, so
    /// choosing the fanout is a deployment decision rather than a code change.
    ///
    /// # Errors
    /// Returns an [`Error`] when a configured Redis cannot be reached.
    pub async fn from_config(config: &AppConfig) -> Result<Self, Error> {
        match &config.redis {
            Some(redis) => Self::redis(redis.url()).await,
            None => Ok(Self::in_process()),
        }
    }

    /// Publishes `event` with `payload` to everyone listening on `channel`.
    ///
    /// Publishing to a channel nobody is listening to is free and is not an
    /// error: a publisher never has to know whether anyone is connected. The
    /// event name is a short verb the client branches on, such as `created` or
    /// `updated`, and the payload is any serializable value.
    ///
    /// # Errors
    /// Returns an [`Error`] when `payload` cannot be serialized, or when the
    /// Redis backend cannot deliver the publish.
    pub async fn publish(
        &self,
        channel: &ChannelName,
        event: &str,
        payload: impl Serialize + Send,
    ) -> Result<(), Error> {
        let payload = serde_json::to_value(payload)
            .map_err(|source| Error::new(ErrorKind::Payload(source)))?;
        let name = channel.to_string();
        let frame = ServerFrame::Event {
            channel: &name,
            event,
            payload,
        }
        .render();

        match &self.inner.backend {
            Backend::InProcess => {
                self.inner.registry.dispatch(&name, &frame);
                Ok(())
            }
            // Deliberately not dispatched locally as well: the publish comes
            // back to this instance through Redis like any other, so local
            // subscribers see each event once and in the same order as remote
            // ones.
            Backend::Redis(fanout) => fanout
                .publish(&name, &frame)
                .await
                .map_err(|source| Error::new(ErrorKind::Transport(source))),
        }
    }

    /// Subscribes this process to `channel` until the handle is dropped.
    ///
    /// Sockets are the only subscriber the framework ships; browsers reach
    /// this through [`router`].
    fn subscribe(&self, channel: &ChannelName) -> Option<Subscription> {
        self.inner.registry.subscribe(channel)
    }
}

/// A realtime event could not be published, or a backend could not be built.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    backtrace: Backtrace,
}

#[derive(Debug)]
enum ErrorKind {
    Configuration(redis::RedisError),
    Transport(redis::RedisError),
    Payload(serde_json::Error),
}

impl Error {
    fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            backtrace: Backtrace::capture(),
        }
    }

    /// Returns `true` when the configured Redis could not be reached at all.
    ///
    /// This is a deployment problem: the URL is wrong, or Redis is not there.
    #[must_use]
    pub fn is_configuration(&self) -> bool {
        matches!(self.kind, ErrorKind::Configuration(_))
    }

    /// Returns `true` when Redis refused or dropped this one publish.
    ///
    /// The connection reconnects underneath, so a later publish may well
    /// succeed. Events are not queued for retry; see the module docs.
    #[must_use]
    pub fn is_transport(&self) -> bool {
        matches!(self.kind, ErrorKind::Transport(_))
    }

    /// Returns `true` when the payload could not be serialized to JSON.
    ///
    /// A programming error in the published type rather than a runtime fault.
    #[must_use]
    pub fn is_payload(&self) -> bool {
        matches!(self.kind, ErrorKind::Payload(_))
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.kind {
            // Redis errors render the address but never the password.
            ErrorKind::Configuration(source) => {
                write!(f, "the realtime Redis backend is unusable: {source}")?;
            }
            ErrorKind::Transport(source) => {
                write!(f, "failed to publish a realtime event: {source}")?;
            }
            ErrorKind::Payload(source) => {
                write!(f, "a realtime payload failed to serialize: {source}")?;
            }
        }
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            ErrorKind::Configuration(source) | ErrorKind::Transport(source) => Some(source),
            ErrorKind::Payload(source) => Some(source),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::{ChannelName, Channels};
    use crate::realtime::registry::Delivery;

    #[tokio::test]
    async fn a_published_event_reaches_a_subscriber_as_one_frame() {
        let channels = Channels::in_process();
        let channel = ChannelName::team(Uuid::new_v4(), "projects");
        let mut subscription = channels.subscribe(&channel).expect("room for a channel");

        channels
            .publish(&channel, "created", json!({ "id": 42 }))
            .await
            .expect("publishing in process never fails");

        let Some(Delivery::Event(frame)) = subscription.recv().await else {
            panic!("expected an event frame");
        };
        let parsed: Value = serde_json::from_str(&frame).expect("frames are JSON");
        assert_eq!(
            parsed,
            json!({
                "type": "event",
                "channel": channel.to_string(),
                "event": "created",
                "payload": { "id": 42 },
            }),
        );
    }

    #[tokio::test]
    async fn a_subscriber_hears_only_its_own_channel() {
        let channels = Channels::in_process();
        let team_id = Uuid::new_v4();
        let listening = ChannelName::team(team_id, "projects");
        let elsewhere = ChannelName::team(team_id, "invoices");
        let mut subscription = channels.subscribe(&listening).expect("room for a channel");

        channels
            .publish(&elsewhere, "created", json!({}))
            .await
            .expect("publishing in process never fails");
        channels
            .publish(&listening, "updated", json!({}))
            .await
            .expect("publishing in process never fails");

        let Some(Delivery::Event(frame)) = subscription.recv().await else {
            panic!("expected an event frame");
        };
        assert!(frame.contains("updated"), "got: {frame}");
    }

    #[tokio::test]
    async fn publishing_to_a_channel_nobody_holds_succeeds() {
        let channels = Channels::in_process();

        channels
            .publish(
                &ChannelName::user(Uuid::new_v4(), "inbox"),
                "created",
                json!({}),
            )
            .await
            .expect("a publisher never has to know who is connected");
    }

    #[tokio::test]
    async fn a_payload_that_cannot_serialize_is_named_as_such() {
        let channels = Channels::in_process();
        // A map keyed by something other than a string has no JSON rendering.
        let payload = std::collections::HashMap::from([(vec![1_u8], "value")]);

        let error = channels
            .publish(
                &ChannelName::user(Uuid::new_v4(), "inbox"),
                "created",
                payload,
            )
            .await
            .expect_err("the payload cannot be JSON");

        assert!(error.is_payload(), "got: {error}");
        assert!(!error.is_transport(), "got: {error}");
    }
}

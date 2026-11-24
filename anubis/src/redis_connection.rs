//! The one recipe for connecting to the optional Redis.
//!
//! [`open`] is the recipe for a Redis connection that serves requests: the
//! connection timeout and the reconnection policy that bound a request waiting
//! on Redis, decided once. The session store and realtime's command connection
//! both open through it, each naming how long its own commands may take to
//! answer; realtime's subscriber connection is its own, because a connection
//! in pub/sub mode carries nothing else. Both sit on the path of requests, so
//! a Redis that stops answering must turn into a prompt error, not a request
//! that hangs until the server's own timeout gives up on it.
//!
//! The connection is a [`ConnectionManager`]: one multiplexed connection,
//! cheap to clone and safe to share across tasks, that reconnects on its own
//! when Redis goes away and comes back. It is opened lazily, so building one
//! performs no I/O and needs only a running tokio runtime; the first command
//! connects. A caller that must know Redis answers before it serves anything,
//! as the server's boot does, sends a command of its own first.

use std::time::Duration;

use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::{Client, RedisError};

/// How long one attempt to reach Redis may take before it counts as failed.
///
/// Redis answers a connection in milliseconds on the same host or network, so
/// a second is generous for a healthy one and short enough that an unreachable
/// one fails a request quickly instead of holding it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

/// How many times a lost connection is retried before a command fails.
///
/// The client library's default is six, with a delay that doubles each time,
/// which keeps a request waiting for many seconds on a Redis that is down.
/// Two retries ride out a restart or a brief network blip, and a Redis that
/// stays down fails the request instead of stalling every one behind it.
const RECONNECT_ATTEMPTS: usize = 2;

/// The longest pause between two reconnection attempts.
const RECONNECT_MAX_DELAY: Duration = Duration::from_millis(500);

/// Opens a lazily connected, self-reconnecting connection to Redis at `url`.
///
/// `response_timeout` is how long one command may wait for its answer, which
/// only the caller can size, because it depends on what its commands do.
///
/// Performs no I/O: the connection is established by the first command sent
/// on it. Must be called inside a tokio runtime, because the connection's
/// supervision task is spawned here.
///
/// # Errors
/// Returns the Redis error when `url` is not a Redis connection URL. A URL
/// from [`crate::config::RedisConfig`] was parsed when the configuration
/// loaded, so it never fails here.
pub(crate) fn open(url: &str, response_timeout: Duration) -> Result<ConnectionManager, RedisError> {
    let client = Client::open(url)?;
    let config = ConnectionManagerConfig::new()
        .set_connection_timeout(Some(CONNECT_TIMEOUT))
        .set_response_timeout(Some(response_timeout))
        .set_number_of_retries(RECONNECT_ATTEMPTS)
        .set_max_delay(RECONNECT_MAX_DELAY);
    ConnectionManager::new_lazy_with_config(client, config)
}

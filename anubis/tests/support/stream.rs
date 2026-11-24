//! A Server-Sent Events client for the realtime endpoints.
//!
//! A stream is opened through the router itself, with `oneshot`, so no port is
//! needed, and read one event at a time off the response body. The body is
//! only pulled while a test reads, which is exactly how a client that stopped
//! reading looks to the server. Every wait is bounded by [`EVENT_TIMEOUT`],
//! because a stream that says nothing is a failure to report rather than a
//! test that hangs.

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::header::{ACCEPT, CONTENT_TYPE, COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

/// How long a test waits for an event before calling the stream broken.
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);

/// One event read off a stream: its SSE id, when it had one, and its frame.
#[derive(Debug, Clone)]
pub struct Received {
    pub id: Option<String>,
    pub frame: Value,
}

impl Received {
    /// The frame's `type`.
    pub fn kind(&self) -> &str {
        self.frame["type"].as_str().unwrap_or_default()
    }

    /// The channel the frame is about, when it names one.
    pub fn channel(&self) -> &str {
        self.frame["channel"].as_str().unwrap_or_default()
    }
}

/// What a stream opened with: every frame before `live`, and `live`'s id.
#[derive(Debug)]
pub struct Opening {
    pub frames: Vec<Received>,
    pub cursor: String,
}

impl Opening {
    /// The frames of one kind, in the order they arrived.
    pub fn of_kind(&self, kind: &str) -> Vec<&Received> {
        self.frames
            .iter()
            .filter(|frame| frame.kind() == kind)
            .collect()
    }

    /// The channels that opened with a reset.
    pub fn resets(&self) -> Vec<&str> {
        self.of_kind("reset")
            .into_iter()
            .map(Received::channel)
            .collect()
    }

    /// The refusal for `channel`, when there was one.
    pub fn refusal(&self, channel: &str) -> Option<&Received> {
        self.of_kind("error")
            .into_iter()
            .find(|frame| frame.channel() == channel)
    }
}

/// One open stream.
#[derive(Debug)]
pub struct Stream {
    pub headers: HeaderMap,
    body: Body,
    pending: String,
}

/// The path of a `/realtime` stream carrying `channels`, with `fresh` marked.
///
/// Each name is percent-encoded on its own, so the adversarial suite can send
/// names no channel could have and still send a valid request.
pub fn path(channels: &[&str], fresh: &[&str]) -> String {
    let mut path = format!("/realtime?channels={}", encoded(channels));
    if !fresh.is_empty() {
        path.push_str("&fresh=");
        path.push_str(&encoded(fresh));
    }
    path
}

/// Names as one comma-separated query value.
fn encoded(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| url::form_urlencoded::byte_serialize(name.as_bytes()).collect::<String>())
        .collect::<Vec<_>>()
        .join(",")
}

/// Opens a stream at `path`, as `cookie`, resuming from `last_event_id`.
///
/// Returns the refusal's status and body when the request is refused before
/// any stream exists.
///
/// # Panics
/// Panics when the router cannot answer at all.
pub async fn open(
    router: &Router,
    path: &str,
    cookie: Option<&str>,
    last_event_id: Option<&str>,
) -> Result<Stream, (StatusCode, Value)> {
    let mut request = Request::builder()
        .uri(path)
        .header(ACCEPT, "text/event-stream");
    if let Some(cookie) = cookie {
        request = request.header(COOKIE, format!("anubis_session={cookie}"));
    }
    if let Some(id) = last_event_id {
        request = request.header("last-event-id", id);
    }
    let request = request.body(Body::empty()).expect("the request must build");

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("the router must answer");

    let status = response.status();
    let headers = response.headers().clone();
    if status != StatusCode::OK {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("a refusal's body must collect")
            .to_bytes();
        return Err((
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        ));
    }

    assert_eq!(
        headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream"),
        "a stream answers as an event stream",
    );
    Ok(Stream {
        headers,
        body: response.into_body(),
        pending: String::new(),
    })
}

/// Opens a stream that must be accepted.
///
/// # Panics
/// Panics when the request is refused.
pub async fn connect(
    router: &Router,
    path: &str,
    cookie: &str,
    last_event_id: Option<&str>,
) -> Stream {
    match open(router, path, Some(cookie), last_event_id).await {
        Ok(stream) => stream,
        Err((status, body)) => panic!("the stream was refused with {status}: {body}"),
    }
}

impl Stream {
    /// Reads the next event, skipping heartbeat comments.
    ///
    /// # Panics
    /// Panics when the stream ends or falls silent for [`EVENT_TIMEOUT`].
    pub async fn next(&mut self) -> Received {
        self.next_within(EVENT_TIMEOUT).await
    }

    /// Reads the next event, waiting at most `wait` for it.
    ///
    /// For an event something slower than a publish produces, such as a job a
    /// worker has to claim first.
    ///
    /// # Panics
    /// Panics when the stream ends or falls silent for `wait`.
    pub async fn next_within(&mut self, wait: Duration) -> Received {
        tokio::time::timeout(wait, self.read())
            .await
            .unwrap_or_else(|_elapsed| panic!("no event arrived within {wait:?}"))
            .expect("the stream ended where an event was expected")
    }

    /// Reads what the stream opened with, up to and including `live`.
    ///
    /// # Panics
    /// Panics when the stream ends or stalls before `live`.
    pub async fn opening(&mut self) -> Opening {
        let mut frames = Vec::new();
        loop {
            let event = self.next().await;
            if event.kind() == "live" {
                let cursor = event.id.expect("the live frame carries the cursor");
                return Opening { frames, cursor };
            }
            frames.push(event);
        }
    }

    /// Reads events until the stream ends, returning them.
    ///
    /// # Panics
    /// Panics when the stream does not end within [`EVENT_TIMEOUT`].
    pub async fn until_end(&mut self) -> Vec<Received> {
        let mut events = Vec::new();
        loop {
            let read = tokio::time::timeout(EVENT_TIMEOUT, self.read())
                .await
                .unwrap_or_else(|_elapsed| {
                    panic!("the stream did not end within {EVENT_TIMEOUT:?}")
                });
            match read {
                Some(event) => events.push(event),
                None => return events,
            }
        }
    }

    /// Whether nothing but heartbeats arrives for `quiet`.
    ///
    /// An event that does arrive is consumed, which is fine for what this is
    /// for: asserting that nothing should have.
    pub async fn stays_quiet(&mut self, quiet: Duration) -> bool {
        tokio::time::timeout(quiet, self.read()).await.is_err()
    }

    /// The next event, or `None` once the body ends.
    async fn read(&mut self) -> Option<Received> {
        loop {
            if let Some(event) = self.take_block() {
                return Some(event);
            }
            let frame = self.body.frame().await?.expect("the body must be readable");
            if let Ok(bytes) = frame.into_data() {
                self.pending
                    .push_str(std::str::from_utf8(&bytes).expect("an event stream is UTF-8"));
            }
        }
    }

    /// Parses one complete event out of what has arrived, skipping comments.
    fn take_block(&mut self) -> Option<Received> {
        loop {
            let end = self.pending.find("\n\n")?;
            let block: String = self.pending.drain(..end + 2).collect();

            let mut id = None;
            let mut data = Vec::new();
            for line in block.lines() {
                if let Some(value) = line.strip_prefix("id: ") {
                    id = Some(value.to_owned());
                } else if let Some(value) = line.strip_prefix("data: ") {
                    data.push(value);
                }
            }
            // A block with no data is a heartbeat comment.
            if data.is_empty() {
                continue;
            }

            let frame = serde_json::from_str(&data.join("\n")).expect("every frame is JSON");
            return Some(Received { id, frame });
        }
    }
}

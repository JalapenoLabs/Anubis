//! The JSON a stream carries in each Server-Sent Event's `data` field.
//!
//! Every frame is a JSON object with a `type` discriminator. Events and the
//! `live` checkpoint also carry the SSE `id` field, which is where a client's
//! `Last-Event-ID` comes from. The wire contract is documented for client
//! authors in `docs/realtime.md`; this module is the server's half of it.

use serde::Serialize;
use serde_json::Value;

/// One frame, as it is rendered into an event's `data` field.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum Frame<'a> {
    /// One published event.
    Event {
        channel: &'a str,
        event: &'a str,
        payload: &'a Value,
    },
    /// What the client holds for `channel` may be stale: read it again.
    ///
    /// Sent when a channel starts at the head of the sequence rather than
    /// resuming, and when the events a resume asked for are no longer held.
    Reset { channel: &'a str },
    /// The stream will not carry `channel`.
    Error {
        channel: &'a str,
        code: ErrorCode,
        message: &'a str,
    },
    /// Everything before this frame has been delivered, or reset.
    ///
    /// Carries the id of the head of the sequence when the stream opened, so a
    /// client that hears nothing else still holds a cursor to resume from.
    Live,
}

impl Frame<'_> {
    /// Renders the frame as the text of one `data` field.
    ///
    /// # Panics
    /// Panics only if a frame fails to serialize, which for these shapes means
    /// a payload that was already a JSON value has stopped being one, a
    /// contradiction.
    pub(crate) fn render(&self) -> String {
        serde_json::to_string(self).expect("stream frames always serialize")
    }
}

/// Why a stream will not carry a channel it was asked for.
///
/// Codes are the part of a refusal a client may branch on; the message beside
/// them is for a person reading a console.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ErrorCode {
    /// The name is not a channel name.
    InvalidChannel,
    /// The channel does not exist, or the subscriber may not listen to it.
    ///
    /// One code covers both on purpose: a browser must not be able to learn
    /// that a team exists by being told it may not listen to it. This is the
    /// stream's form of the `404` the ownership-chain guards answer.
    NotFound,
    /// This process is already holding as many channels as it may.
    TooManyChannels,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ErrorCode, Frame};

    fn parsed(frame: &Frame<'_>) -> serde_json::Value {
        serde_json::from_str(&frame.render()).expect("frames are JSON")
    }

    #[test]
    fn frames_render_the_documented_shapes() {
        assert_eq!(
            parsed(&Frame::Event {
                channel: "team:t:projects",
                event: "created",
                payload: &json!({ "id": 1 }),
            }),
            json!({
                "type": "event",
                "channel": "team:t:projects",
                "event": "created",
                "payload": { "id": 1 },
            }),
        );
        assert_eq!(
            parsed(&Frame::Reset {
                channel: "team:t:projects"
            }),
            json!({ "type": "reset", "channel": "team:t:projects" }),
        );
        assert_eq!(
            parsed(&Frame::Error {
                channel: "team:t:projects",
                code: ErrorCode::NotFound,
                message: "No such channel.",
            }),
            json!({
                "type": "error",
                "channel": "team:t:projects",
                "code": "not_found",
                "message": "No such channel.",
            }),
        );
        assert_eq!(parsed(&Frame::Live), json!({ "type": "live" }));
    }

    #[test]
    fn a_frame_renders_on_one_line() {
        // An SSE `data` field ends at a line break, so a frame carrying one
        // would arrive as two fields and be rejoined with a newline. Stored
        // replay entries rely on it as well: an entry is one line of text.
        let rendered = Frame::Event {
            channel: "team:t:projects",
            event: "line\nbreak",
            payload: &json!({ "text": "two\nlines\r" }),
        }
        .render();

        assert!(!rendered.contains('\n'), "got: {rendered}");
        assert!(!rendered.contains('\r'), "got: {rendered}");
    }
}

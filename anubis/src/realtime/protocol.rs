//! The JSON frames a browser and the server exchange over one websocket.
//!
//! Every frame is a JSON object with a `type` discriminator. The wire contract
//! is documented for client authors in `docs/realtime.md`; this module is the
//! server's half of it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a browser sends.
///
/// Unknown fields are ignored, so a newer client may carry extra keys without
/// an older server refusing its frames.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ClientFrame {
    /// Start receiving events published to `channel`.
    Subscribe {
        /// The channel name, as [`super::ChannelName`] renders it.
        channel: String,
    },
    /// Stop receiving them.
    Unsubscribe {
        /// The channel name, as [`super::ChannelName`] renders it.
        channel: String,
    },
}

/// What the server sends.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ServerFrame<'a> {
    /// The subscription is live; events for `channel` follow.
    Subscribed { channel: &'a str },
    /// The subscription is gone; no more events for `channel` follow.
    Unsubscribed { channel: &'a str },
    /// One published event.
    Event {
        channel: &'a str,
        event: &'a str,
        payload: Value,
    },
    /// A request was refused, or a subscription degraded.
    ///
    /// `channel` is absent only when the frame that caused it named no
    /// channel, such as a frame that was not JSON at all.
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        channel: Option<&'a str>,
        code: ErrorCode,
        message: &'a str,
    },
}

impl ServerFrame<'_> {
    /// Renders the frame as the text sent over the socket.
    ///
    /// # Panics
    /// Panics only if a frame fails to serialize, which for these shapes means
    /// a payload that was already accepted by [`super::Channels::publish`] has
    /// become unserializable, a contradiction.
    pub(crate) fn render(&self) -> String {
        serde_json::to_string(self).expect("server frames always serialize")
    }
}

/// Why a request was refused, or a subscription degraded.
///
/// Codes are the part of an error a client may branch on; the message beside
/// them is for a person reading a console.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ErrorCode {
    /// The frame was not JSON, named no known type, or named no valid channel.
    InvalidFrame,
    /// The channel does not exist, or the subscriber may not listen to it.
    ///
    /// One code covers both on purpose: a browser must not be able to learn
    /// that a team exists by being told it may not listen to it. This is the
    /// websocket's form of the `404` the ownership-chain guards answer.
    NotFound,
    /// The socket, or the process, is holding as many channels as it may.
    TooManyChannels,
    /// The subscriber fell behind and the server dropped events for it.
    Lagged,
    /// The server failed to answer the request and the client may retry.
    Internal,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ClientFrame, ErrorCode, ServerFrame};

    #[test]
    fn client_frames_parse_by_their_type_tag() {
        let subscribe: ClientFrame =
            serde_json::from_str(r#"{"type":"subscribe","channel":"user:x:inbox"}"#)
                .expect("a subscribe frame must parse");
        assert_eq!(
            subscribe,
            ClientFrame::Subscribe {
                channel: "user:x:inbox".to_owned(),
            },
        );

        let unsubscribe: ClientFrame =
            serde_json::from_str(r#"{"type":"unsubscribe","channel":"user:x:inbox","id":7}"#)
                .expect("unknown fields are ignored");
        assert_eq!(
            unsubscribe,
            ClientFrame::Unsubscribe {
                channel: "user:x:inbox".to_owned(),
            },
        );
    }

    #[test]
    fn frames_that_name_no_known_type_are_refused() {
        for value in [
            "not json at all",
            "{}",
            r#"{"type":"publish","channel":"user:x:inbox"}"#,
            r#"{"type":"subscribe"}"#,
        ] {
            assert!(
                serde_json::from_str::<ClientFrame>(value).is_err(),
                "{value:?} must be refused",
            );
        }
    }

    #[test]
    fn server_frames_render_the_documented_shape() {
        let event = ServerFrame::Event {
            channel: "team:t:projects",
            event: "created",
            payload: json!({ "id": 1 }),
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&event.render()).expect("valid JSON"),
            json!({
                "type": "event",
                "channel": "team:t:projects",
                "event": "created",
                "payload": { "id": 1 },
            }),
        );

        let refused = ServerFrame::Error {
            channel: Some("team:t:projects"),
            code: ErrorCode::NotFound,
            message: "No such channel.",
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&refused.render()).expect("valid JSON"),
            json!({
                "type": "error",
                "channel": "team:t:projects",
                "code": "not_found",
                "message": "No such channel.",
            }),
        );
    }

    #[test]
    fn an_error_about_no_channel_omits_the_key_rather_than_sending_null() {
        let refused = ServerFrame::Error {
            channel: None,
            code: ErrorCode::InvalidFrame,
            message: "That frame is not valid JSON.",
        };

        let rendered = refused.render();

        assert!(!rendered.contains("channel"), "got: {rendered}");
    }
}

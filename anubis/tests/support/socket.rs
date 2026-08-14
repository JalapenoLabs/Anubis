//! A websocket client for the realtime endpoint.
//!
//! The realtime contract is a request and a reply per frame, so a test reads
//! like a conversation: connect, send, expect. Everything here bounds its wait
//! on [`FRAME_TIMEOUT`], because a socket that answers nothing is a failure to
//! report rather than a test that hangs.

use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::http::header::COOKIE;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

/// How long a test waits for a frame before calling the socket broken.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// One connected realtime socket.
pub type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Opens an authenticated realtime socket.
///
/// # Panics
/// Panics when the upgrade is refused, which for an authenticated request is a
/// failure of the endpoint rather than of the client.
pub async fn connect(address: SocketAddr, cookie: &str) -> Socket {
    let mut request = format!("ws://{address}/realtime")
        .into_client_request()
        .expect("the realtime URL must parse");
    request.headers_mut().insert(
        COOKIE,
        HeaderValue::from_str(&format!("anubis_session={cookie}"))
            .expect("a session token is a valid header value"),
    );

    let (socket, _response) = connect_async(request)
        .await
        .expect("the realtime upgrade must succeed");
    socket
}

/// Sends one client frame.
///
/// # Panics
/// Panics when the socket refuses the write.
pub async fn send(socket: &mut Socket, frame: &Value) {
    socket
        .send(Message::Text(frame.to_string().into()))
        .await
        .expect("the socket must accept a frame");
}

/// Sends a binary frame, which the protocol has no use for.
///
/// # Panics
/// Panics when the socket refuses the write.
pub async fn send_binary(socket: &mut Socket, payload: Vec<u8>) {
    socket
        .send(Message::Binary(payload.into()))
        .await
        .expect("the socket must accept a frame");
}

/// Subscribes to `channel` and returns the server's reply.
///
/// # Panics
/// Panics when the socket refuses the write or answers nothing.
pub async fn subscribe(socket: &mut Socket, channel: &str) -> Value {
    send(socket, &json!({ "type": "subscribe", "channel": channel })).await;
    next_frame(socket).await
}

/// Reads the next server frame, ignoring the heartbeat traffic under it.
///
/// # Panics
/// Panics when the socket closes or falls silent for [`FRAME_TIMEOUT`].
pub async fn next_frame(socket: &mut Socket) -> Value {
    let frame = tokio::time::timeout(FRAME_TIMEOUT, async {
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => {
                    return serde_json::from_str(&text).expect("server frames are JSON");
                }
                // Pings and pongs are the heartbeat, not the conversation.
                Some(Ok(Message::Ping(_beat) | Message::Pong(_beat))) => {}
                other => panic!("expected a text frame, got {other:?}"),
            }
        }
    })
    .await;

    frame.expect("the server must answer within the frame timeout")
}

/// Returns `true` when the socket is closed rather than still conversing.
///
/// A frame the protocol refuses outright, an oversized one above all, ends the
/// connection instead of drawing a reply, so this is how a test asserts that.
///
/// # Panics
/// Panics when the socket neither closes nor answers within
/// [`FRAME_TIMEOUT`].
pub async fn is_closed(socket: &mut Socket) -> bool {
    let outcome = tokio::time::timeout(FRAME_TIMEOUT, async {
        loop {
            match socket.next().await {
                None
                | Some(
                    Ok(Message::Close(..)) | Err(WsError::ConnectionClosed | WsError::Protocol(..)),
                ) => return true,
                Some(Ok(Message::Ping(_beat) | Message::Pong(_beat))) => {}
                Some(Ok(_other)) => return false,
                Some(Err(error)) => panic!("unexpected socket error: {error}"),
            }
        }
    })
    .await;

    outcome.expect("the socket must either close or answer")
}

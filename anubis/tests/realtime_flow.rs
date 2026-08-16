//! Realtime channels over a real websocket, against a real Postgres database.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one. The Redis-backed test additionally requires
//! `REDIS_URL` and skips on its own when that is absent, so a machine without
//! Redis still runs everything the in-process backend covers.

use std::net::SocketAddr;
use std::time::Duration;

use anubis::realtime::{ChannelName, Channels};
use anubis::schema::{team_memberships, teams, users};
use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tower::ServiceExt;
use uuid::Uuid;

/// The starter's roles, trimmed to what tenancy bootstrapping needs.
const ROLES_YML: &str = "
roles:
  default:
    models: {}
  admin:
    includes: [default]
    models:
      Team: [manage]
";

/// How long a test waits for a frame before calling the socket broken.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The composed application, served on a real port so websockets can connect.
struct TestServer {
    router: Router,
    address: SocketAddr,
    channels: Channels,
    pool: anubis::db::DbPool,
}

impl TestServer {
    /// Boots the framework routers over `database_url`, with `channels`.
    async fn start(database_url: &str, channels: Channels) -> Self {
        anubis::db::run_pending_migrations(database_url)
            .await
            .expect("migrations must apply");
        let pool = anubis::db::connect(database_url)
            .await
            .expect("database must be reachable");

        let config = anubis::config::AppConfig::from_lookup(|name| match name {
            "ANUBIS_ENV" => Some("test".to_owned()),
            _ => None,
        })
        .expect("test config must parse");
        let roles = anubis::roles::RoleSet::from_yaml(ROLES_YML).expect("roles must parse");
        let (mailer, _outbox) = anubis::mail::Mailer::test();
        let rate_limit = anubis::rate_limit::RateLimiter::new(&config.rate_limit);

        let router = Router::new()
            .merge(anubis::realtime::router(pool.clone(), channels.clone()))
            .nest(
                "/auth",
                anubis::auth::router(pool.clone(), mailer.clone(), &config, &rate_limit),
            )
            .nest(
                "/tenancy",
                anubis::tenancy::router(
                    pool.clone(),
                    mailer,
                    roles.clone(),
                    None,
                    &config,
                    &rate_limit,
                ),
            )
            .layer(anubis::guard::layer(pool.clone(), roles));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port must be available");
        let address = listener.local_addr().expect("the listener must be bound");

        let serving = router.clone();
        tokio::spawn(async move {
            let _served = axum::serve(
                listener,
                serving.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await;
        });

        Self {
            router,
            address,
            channels,
            pool,
        }
    }

    /// Registers a user and returns their session token.
    async fn register(&self, email: &str) -> String {
        let credentials = json!({ "email": email, "password": "correct horse battery staple" });
        let request = Request::builder()
            .method("POST")
            .uri("/auth/register")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&credentials).expect("credentials serialize"),
            ))
            .expect("request must build");

        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .expect("registration must complete");
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .into_body()
            .collect()
            .await
            .expect("body must collect")
            .to_bytes();
        assert_eq!(
            status,
            StatusCode::CREATED,
            "body: {}",
            String::from_utf8_lossy(&body),
        );

        session_token(&headers)
    }

    /// Opens a websocket carrying `session`, or returns the refusal status.
    async fn connect(&self, session: Option<&str>) -> Result<Socket, StatusCode> {
        let mut request = format!("ws://{}/realtime", self.address)
            .into_client_request()
            .expect("the URL must be a valid websocket request");
        if let Some(token) = session {
            request.headers_mut().insert(
                COOKIE,
                HeaderValue::from_str(&format!("anubis_session={token}"))
                    .expect("a token is a valid header value"),
            );
        }

        match connect_async(request).await {
            Ok((socket, _response)) => Ok(socket),
            Err(WsError::Http(response)) => Err(response.status()),
            Err(other) => panic!("the upgrade failed for the wrong reason: {other}"),
        }
    }
}

fn session_token(headers: &HeaderMap) -> String {
    for value in headers.get_all(SET_COOKIE) {
        if let Some(rest) = value
            .to_str()
            .ok()
            .and_then(|rendered| rendered.strip_prefix("anubis_session="))
        {
            let token = rest.split(';').next().unwrap_or_default();
            if !token.is_empty() {
                return token.to_owned();
            }
        }
    }
    panic!("no session cookie in response");
}

/// Sends one client frame.
async fn send(socket: &mut Socket, frame: &Value) {
    socket
        .send(Message::Text(frame.to_string().into()))
        .await
        .expect("the socket must accept a frame");
}

/// Reads the next server frame, ignoring the heartbeat traffic around it.
async fn next(socket: &mut Socket) -> Value {
    let deadline = tokio::time::Instant::now() + FRAME_TIMEOUT;
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("a frame must arrive before the timeout")
            .expect("the socket must stay open")
            .expect("the frame must be readable");

        match message {
            Message::Text(text) => {
                return serde_json::from_str(&text).expect("server frames are JSON");
            }
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("unexpected websocket message: {other:?}"),
        }
    }
}

/// The team the freshly registered user was bootstrapped into.
async fn bootstrapped_team(pool: &anubis::db::DbPool, email: &str) -> (Uuid, Uuid) {
    let mut connection = pool.get().await.expect("connection must be available");
    let user_id: Uuid = users::table
        .filter(users::email.eq(email))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the user must exist");
    let team_id: Uuid = teams::table
        .inner_join(
            team_memberships::table.on(team_memberships::team_id
                .eq(teams::id)
                .and(team_memberships::user_id.eq(user_id))),
        )
        .select(teams::id)
        .first(&mut connection)
        .await
        .expect("the bootstrapped team must exist");

    (user_id, team_id)
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over a single websocket flow"
)]
async fn realtime_channels_authorize_subscribers_and_deliver_events() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping realtime_flow test: DATABASE_URL is not set");
        return;
    };

    let server = TestServer::start(&database_url, Channels::in_process()).await;

    let run = Uuid::new_v4();
    let member_email = format!("realtime-member-{run}@example.com");
    let outsider_email = format!("realtime-outsider-{run}@example.com");
    let member_session = server.register(&member_email).await;
    let outsider_session = server.register(&outsider_email).await;
    let (member_id, team_id) = bootstrapped_team(&server.pool, &member_email).await;
    let (outsider_id, _outsider_team) = bootstrapped_team(&server.pool, &outsider_email).await;

    let projects = ChannelName::team(team_id, "projects");
    let inbox = ChannelName::user(member_id, "inbox");

    // An upgrade without a session never becomes a websocket.
    let refused = server
        .connect(None)
        .await
        .expect_err("an anonymous upgrade must be refused");
    assert_eq!(refused, StatusCode::UNAUTHORIZED);

    let mut socket = server
        .connect(Some(&member_session))
        .await
        .expect("a session must be enough to upgrade");

    // A member may listen to their team.
    send(
        &mut socket,
        &json!({ "type": "subscribe", "channel": projects.to_string() }),
    )
    .await;
    assert_eq!(
        next(&mut socket).await,
        json!({ "type": "subscribed", "channel": projects.to_string() }),
    );

    // What the server publishes arrives on the socket.
    server
        .channels
        .publish(&projects, "created", json!({ "id": 7, "name": "Apollo" }))
        .await
        .expect("publishing must succeed");
    assert_eq!(
        next(&mut socket).await,
        json!({
            "type": "event",
            "channel": projects.to_string(),
            "event": "created",
            "payload": { "id": 7, "name": "Apollo" },
        }),
    );

    // A user channel is self-only, and needs no membership at all.
    send(
        &mut socket,
        &json!({ "type": "subscribe", "channel": inbox.to_string() }),
    )
    .await;
    assert_eq!(
        next(&mut socket).await,
        json!({ "type": "subscribed", "channel": inbox.to_string() }),
    );

    let stranger = ChannelName::user(outsider_id, "inbox");
    send(
        &mut socket,
        &json!({ "type": "subscribe", "channel": stranger.to_string() }),
    )
    .await;
    let refused = next(&mut socket).await;
    assert_eq!(refused["type"], json!("error"));
    assert_eq!(refused["code"], json!("not_found"));

    // Unsubscribing stops delivery. The subscribe reply that follows the
    // publish proves nothing was delivered in between.
    send(
        &mut socket,
        &json!({ "type": "unsubscribe", "channel": projects.to_string() }),
    )
    .await;
    assert_eq!(
        next(&mut socket).await,
        json!({ "type": "unsubscribed", "channel": projects.to_string() }),
    );
    server
        .channels
        .publish(&projects, "created", json!({ "id": 8 }))
        .await
        .expect("publishing must succeed");
    send(
        &mut socket,
        &json!({ "type": "subscribe", "channel": projects.to_string() }),
    )
    .await;
    assert_eq!(
        next(&mut socket).await,
        json!({ "type": "subscribed", "channel": projects.to_string() }),
        "an event published while unsubscribed must not have been delivered",
    );

    // A frame the protocol does not define is named as such rather than
    // silently ignored.
    send(
        &mut socket,
        &json!({ "type": "publish", "channel": projects.to_string() }),
    )
    .await;
    let refused = next(&mut socket).await;
    assert_eq!(refused["type"], json!("error"));
    assert_eq!(refused["code"], json!("invalid_frame"));

    // A non-member is refused exactly as a nonexistent team is.
    let mut outsider_socket = server
        .connect(Some(&outsider_session))
        .await
        .expect("any session may open a socket");
    send(
        &mut outsider_socket,
        &json!({ "type": "subscribe", "channel": projects.to_string() }),
    )
    .await;
    let not_a_member = next(&mut outsider_socket).await;

    let ghost = ChannelName::team(Uuid::new_v4(), "projects");
    send(
        &mut outsider_socket,
        &json!({ "type": "subscribe", "channel": ghost.to_string() }),
    )
    .await;
    let no_such_team = next(&mut outsider_socket).await;

    assert_eq!(not_a_member["code"], json!("not_found"));
    assert_eq!(
        not_a_member["message"], no_such_team["message"],
        "a team you may not see must answer exactly as one that does not exist",
    );

    // In-process fanout reaches every socket listening to the channel.
    let mut second_socket = server
        .connect(Some(&member_session))
        .await
        .expect("a session may open more than one socket");
    send(
        &mut second_socket,
        &json!({ "type": "subscribe", "channel": projects.to_string() }),
    )
    .await;
    assert_eq!(next(&mut second_socket).await["type"], json!("subscribed"));

    server
        .channels
        .publish(&projects, "updated", json!({ "id": 9 }))
        .await
        .expect("publishing must succeed");
    for listener in [&mut socket, &mut second_socket] {
        let event = next(listener).await;
        assert_eq!(event["type"], json!("event"));
        assert_eq!(event["event"], json!("updated"));
        assert_eq!(event["payload"], json!({ "id": 9 }));
    }
}

#[tokio::test]
async fn redis_fanout_carries_events_between_instances() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping realtime_flow Redis test: DATABASE_URL is not set");
        return;
    };
    let Ok(redis_url) = std::env::var("REDIS_URL") else {
        eprintln!("skipping realtime_flow Redis test: REDIS_URL is not set");
        return;
    };

    // Two independent handles on the same Redis stand in for two instances:
    // the socket is served by the first, and the publish goes through the
    // second, so nothing but Redis can carry the event between them.
    let serving = Channels::redis(&redis_url)
        .await
        .expect("REDIS_URL must name a reachable Redis");
    let publishing = Channels::redis(&redis_url)
        .await
        .expect("REDIS_URL must name a reachable Redis");

    let server = TestServer::start(&database_url, serving).await;

    let run = Uuid::new_v4();
    let member_email = format!("realtime-redis-{run}@example.com");
    let member_session = server.register(&member_email).await;
    let (_member_id, team_id) = bootstrapped_team(&server.pool, &member_email).await;
    let projects = ChannelName::team(team_id, "projects");

    let mut socket = server
        .connect(Some(&member_session))
        .await
        .expect("a session must be enough to upgrade");
    send(
        &mut socket,
        &json!({ "type": "subscribe", "channel": projects.to_string() }),
    )
    .await;
    assert_eq!(next(&mut socket).await["type"], json!("subscribed"));

    // The subscribe reply is sent before the pump has necessarily told Redis
    // about the channel, so publish until one lands rather than racing it.
    let event = loop {
        publishing
            .publish(&projects, "created", json!({ "id": 11 }))
            .await
            .expect("publishing through Redis must succeed");

        match tokio::time::timeout(Duration::from_millis(250), next(&mut socket)).await {
            Ok(frame) => break frame,
            Err(_elapsed) => {}
        }
    };

    assert_eq!(event["type"], json!("event"));
    assert_eq!(event["channel"], json!(projects.to_string()));
    assert_eq!(event["payload"], json!({ "id": 11 }));
}

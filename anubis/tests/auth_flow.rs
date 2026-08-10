//! End-to-end auth flow against a real Postgres database.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes, so
//! plain `cargo test` still works on machines without a database. CI always
//! provides one.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn post_json(router: &Router, path: &str, body: &Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(body).expect("body must serialize"),
        ))
        .expect("request must build");

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("request must complete");

    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn register_and_login_round_trip() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping auth_flow test: DATABASE_URL is not set");
        return;
    };

    anubis::db::run_pending_migrations(&database_url)
        .await
        .expect("migrations must apply");
    let pool = anubis::db::connect(&database_url)
        .await
        .expect("database must be reachable");

    let router = anubis::auth::router(pool);
    let email = format!("it-{}@example.com", uuid::Uuid::new_v4());
    let password = "correct horse battery staple";

    // Registration creates the account and returns the serialized user.
    let (status, body) = post_json(
        &router,
        "/register",
        &json!({ "email": email, "password": password }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["user"]["email"], json!(email));
    assert!(body["user"]["id"].is_string(), "body: {body}");
    assert!(body["user"].get("password_hash").is_none(), "body: {body}");

    // The same email cannot register twice.
    let (status, body) = post_json(
        &router,
        "/register",
        &json!({ "email": email, "password": password }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");

    // Correct credentials log in.
    let (status, body) = post_json(
        &router,
        "/login",
        &json!({ "email": email, "password": password }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["user"]["email"], json!(email));

    // Email lookup is case-insensitive because emails normalize on the way in.
    let (status, _body) = post_json(
        &router,
        "/login",
        &json!({ "email": email.to_uppercase(), "password": password }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // A wrong password and an unknown email fail identically.
    let (status, body) = post_json(
        &router,
        "/login",
        &json!({ "email": email, "password": "wrong password entirely" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let wrong_password_message = body["message"].clone();

    let (status, body) = post_json(
        &router,
        "/login",
        &json!({ "email": "nobody@example.com", "password": password }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["message"], wrong_password_message);

    // Broken input is rejected before it touches the database.
    let (status, _body) = post_json(
        &router,
        "/register",
        &json!({ "email": "not-an-email", "password": password }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

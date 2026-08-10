//! Avatar upload, serving, caching, and deletion against a real Postgres
//! database.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE, ETAG, IF_NONE_MATCH, SET_COOKIE};
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

fn sample_png() -> Vec<u8> {
    let source = image::DynamicImage::new_rgb8(900, 600);
    let mut bytes = Vec::new();
    source
        .write_with_encoder(image::codecs::png::PngEncoder::new(&mut bytes))
        .expect("encoding the fixture must succeed");
    bytes
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn avatars_upload_serve_cache_and_delete() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping avatar_flow test: DATABASE_URL is not set");
        return;
    };

    anubis::db::run_pending_migrations(&database_url)
        .await
        .expect("migrations must apply");
    let pool = anubis::db::connect(&database_url)
        .await
        .expect("database must be reachable");

    let config = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        _ => None,
    })
    .expect("test config must parse");
    let (mailer, _outbox) = anubis::mail::Mailer::test();
    let router = Router::new()
        .merge(anubis::auth::avatar_router(pool.clone()))
        .nest("/auth", anubis::auth::router(pool, mailer, &config));

    let email = format!("avatar-{}@example.com", Uuid::new_v4());
    let register = Request::builder()
        .method("POST")
        .uri("/auth/register")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({
                "email": email,
                "password": "correct horse battery staple",
            }))
            .expect("body must serialize"),
        ))
        .expect("request must build");
    let response = router
        .clone()
        .oneshot(register)
        .await
        .expect("request must complete");
    assert_eq!(response.status(), StatusCode::CREATED);
    let cookie = response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .find_map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|rendered| rendered.strip_prefix("anubis_session="))
                .map(|rest| rest.split(';').next().unwrap_or_default().to_owned())
        })
        .expect("registration must set a session cookie");

    // Unauthenticated uploads are rejected.
    let upload = Request::builder()
        .method("POST")
        .uri("/auth/profile/avatar")
        .body(Body::from(sample_png()))
        .expect("request must build");
    let response = router
        .clone()
        .oneshot(upload)
        .await
        .expect("request must complete");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Upload and read back the avatar URL.
    let upload = Request::builder()
        .method("POST")
        .uri("/auth/profile/avatar")
        .header(COOKIE, format!("anubis_session={cookie}"))
        .body(Body::from(sample_png()))
        .expect("request must build");
    let response = router
        .clone()
        .oneshot(upload)
        .await
        .expect("request must complete");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    let avatar_url = body["avatar_url"].as_str().expect("avatar_url").to_owned();

    // The public URL serves an optimized square JPEG with cache headers.
    let fetch = Request::builder()
        .uri(&avatar_url)
        .body(Body::empty())
        .expect("request must build");
    let response = router
        .clone()
        .oneshot(fetch)
        .await
        .expect("request must complete");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(CONTENT_TYPE)
            .map(axum::http::HeaderValue::as_bytes),
        Some(b"image/jpeg".as_slice())
    );
    let etag = response
        .headers()
        .get(ETAG)
        .expect("an ETag must be served")
        .to_str()
        .expect("ETag must render")
        .to_owned();
    let image_bytes = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();
    let decoded = image::load_from_memory(&image_bytes).expect("served bytes must decode");
    assert_eq!(decoded.width(), 512);
    assert_eq!(decoded.height(), 512);

    // Matching If-None-Match answers 304 with no body.
    let conditional = Request::builder()
        .uri(&avatar_url)
        .header(IF_NONE_MATCH, &etag)
        .body(Body::empty())
        .expect("request must build");
    let response = router
        .clone()
        .oneshot(conditional)
        .await
        .expect("request must complete");
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

    // Deletion makes the URL 404.
    let remove = Request::builder()
        .method("DELETE")
        .uri("/auth/profile/avatar")
        .header(COOKIE, format!("anubis_session={cookie}"))
        .body(Body::empty())
        .expect("request must build");
    let response = router
        .clone()
        .oneshot(remove)
        .await
        .expect("request must complete");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let fetch = Request::builder()
        .uri(&avatar_url)
        .body(Body::empty())
        .expect("request must build");
    let response = router.oneshot(fetch).await.expect("request must complete");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

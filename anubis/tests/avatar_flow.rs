//! Avatar upload, serving, caching, and deletion against a real Postgres
//! database.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

use axum::Router;
use axum::body::Body;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, COOKIE, ETAG, IF_NONE_MATCH, SET_COOKIE};
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

/// A PNG of one flat `shade`, so two uploads differ in content and version.
fn sample_png(shade: u8) -> Vec<u8> {
    let mut source = image::RgbImage::new(900, 600);
    for pixel in source.pixels_mut() {
        *pixel = image::Rgb([shade, shade, shade]);
    }

    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgb8(source)
        .write_with_encoder(image::codecs::png::PngEncoder::new(&mut bytes))
        .expect("encoding the fixture must succeed");
    bytes
}

/// The header value as a string, for readable assertions.
fn header(response: &axum::response::Response, name: axum::http::HeaderName) -> String {
    response
        .headers()
        .get(name)
        .expect("the header must be present")
        .to_str()
        .expect("the header must render")
        .to_owned()
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
    let rate_limit = anubis::rate_limit::RateLimiter::new(&config.rate_limit);
    let router = Router::new()
        .merge(anubis::auth::avatar_router(pool.clone()))
        .nest(
            "/auth",
            anubis::auth::router(pool, mailer, &config, &rate_limit),
        );

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
        .body(Body::from(sample_png(0)))
        .expect("request must build");
    let response = router
        .clone()
        .oneshot(upload)
        .await
        .expect("request must complete");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // Upload and read back the versioned avatar URL.
    let upload = Request::builder()
        .method("POST")
        .uri("/auth/profile/avatar")
        .header(COOKIE, format!("anubis_session={cookie}"))
        .body(Body::from(sample_png(0)))
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
    let (bare_url, version) = avatar_url
        .split_once("?v=")
        .expect("the upload answers a versioned URL");
    let bare_url = bare_url.to_owned();
    let version = version.to_owned();

    // The profile payload carries the same version, so every consumer builds
    // the same URL without asking the avatar endpoint anything.
    let profile = Request::builder()
        .uri("/auth/me")
        .header(COOKIE, format!("anubis_session={cookie}"))
        .body(Body::empty())
        .expect("request must build");
    let response = router
        .clone()
        .oneshot(profile)
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
    assert_eq!(body["user"]["avatar_version"], json!(version));

    // The versioned URL serves an optimized square JPEG, cacheable for a year.
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
    assert_eq!(header(&response, CONTENT_TYPE), "image/jpeg");
    assert_eq!(
        header(&response, CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );
    let etag = header(&response, ETAG);
    let image_bytes = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();
    let decoded = image::load_from_memory(&image_bytes).expect("served bytes must decode");
    assert_eq!(decoded.width(), 512);
    assert_eq!(decoded.height(), 512);

    // The bare URL names the account rather than the image, so it keeps the
    // modest policy external consumers revalidate against.
    let fetch = Request::builder()
        .uri(&bare_url)
        .body(Body::empty())
        .expect("request must build");
    let response = router
        .clone()
        .oneshot(fetch)
        .await
        .expect("request must complete");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header(&response, CACHE_CONTROL),
        "public, max-age=3600, stale-while-revalidate=86400"
    );

    // A stale client naming a version we no longer serve gets the same modest
    // policy, so yesterday's URL never freezes today's bytes for a year.
    let fetch = Request::builder()
        .uri(format!("{bare_url}?v=notthecurrentone"))
        .body(Body::empty())
        .expect("request must build");
    let response = router
        .clone()
        .oneshot(fetch)
        .await
        .expect("request must complete");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header(&response, CACHE_CONTROL),
        "public, max-age=3600, stale-while-revalidate=86400"
    );

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
    assert_eq!(
        header(&response, CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );

    // A second upload changes the version, which is what makes a fresh picture
    // appear everywhere the moment a consumer refetches the profile.
    let upload = Request::builder()
        .method("POST")
        .uri("/auth/profile/avatar")
        .header(COOKIE, format!("anubis_session={cookie}"))
        .body(Body::from(sample_png(200)))
        .expect("request must build");
    let response = router
        .clone()
        .oneshot(upload)
        .await
        .expect("request must complete");
    assert_eq!(response.status(), StatusCode::OK);

    let profile = Request::builder()
        .uri("/auth/me")
        .header(COOKIE, format!("anubis_session={cookie}"))
        .body(Body::empty())
        .expect("request must build");
    let response = router
        .clone()
        .oneshot(profile)
        .await
        .expect("request must complete");
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    let second_version = body["user"]["avatar_version"]
        .as_str()
        .expect("avatar_version")
        .to_owned();
    assert_ne!(second_version, version, "a new picture is a new URL");

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
    let response = router
        .clone()
        .oneshot(fetch)
        .await
        .expect("request must complete");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // The profile drops the version too, so consumers fall back to initials.
    let profile = Request::builder()
        .uri("/auth/me")
        .header(COOKIE, format!("anubis_session={cookie}"))
        .body(Body::empty())
        .expect("request must build");
    let response = router
        .oneshot(profile)
        .await
        .expect("request must complete");
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
    assert_eq!(body["user"]["avatar_version"], serde_json::Value::Null);
}

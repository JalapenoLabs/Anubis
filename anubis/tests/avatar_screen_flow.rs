//! The image screen an application plugs into the avatar upload, and the audit
//! trail every avatar change leaves, against a real Postgres database.
//!
//! The screen here is a stand-in that records what it was shown and answers
//! whatever the test sets: no classifier and no real imagery, only flat-colored
//! PNGs. What is under test is the framework's half of the contract: the screen
//! sees each upload once, decoded, before anything is stored, and nothing but
//! an acceptance lets a picture through. `avatar_flow` covers the upload with
//! no screen at all, which must behave exactly as it always has.
//!
//! Each test takes a database of its own (see `support::TestDatabase`),
//! because the assertions count audit rows. Requires `DATABASE_URL`; without
//! it each test logs a skip and passes. CI always provides one.

mod support;

use std::sync::{Arc, Mutex};

use anubis::images::{
    DecodedImage, IMAGE_REFUSED, IMAGE_SCREEN_UNAVAILABLE, ImageFormat, ImageScreen, ScreenError,
    ScreenFuture, Verdict,
};
use anubis::schema::audit_events;
use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, COOKIE};
use axum::http::{Request, StatusCode};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use support::{TestDatabase, register, send};

/// What the stand-in screen answers next.
#[derive(Debug, Clone, Copy)]
enum Outcome {
    Accept,
    Refuse,
    Fail,
}

/// What the screen was shown on one call: the shape, the format, and how many
/// RGBA bytes the pixels came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Shown {
    width: u32,
    height: u32,
    format: ImageFormat,
    rgba_bytes: usize,
}

#[derive(Debug)]
struct Ledger {
    outcome: Outcome,
    shown: Vec<Shown>,
}

/// An [`ImageScreen`] that records every image it is shown and answers the
/// outcome the test last set.
///
/// Cloned before it is handed to the router, so the test keeps a handle onto
/// the same ledger the router's copy writes to.
#[derive(Debug, Clone)]
struct RecordingScreen {
    ledger: Arc<Mutex<Ledger>>,
}

impl RecordingScreen {
    fn new() -> Self {
        Self {
            ledger: Arc::new(Mutex::new(Ledger {
                outcome: Outcome::Accept,
                shown: Vec::new(),
            })),
        }
    }

    fn answer(&self, outcome: Outcome) {
        self.ledger.lock().expect("the ledger lock").outcome = outcome;
    }

    fn shown(&self) -> Vec<Shown> {
        self.ledger.lock().expect("the ledger lock").shown.clone()
    }
}

impl ImageScreen for RecordingScreen {
    fn screen<'a>(&'a self, image: &'a DecodedImage) -> ScreenFuture<'a> {
        let mut ledger = self.ledger.lock().expect("the ledger lock");
        ledger.shown.push(Shown {
            width: image.width(),
            height: image.height(),
            format: image.format(),
            rgba_bytes: image.to_rgba8().len(),
        });
        let answer = match ledger.outcome {
            Outcome::Accept => Ok(Verdict::Accept),
            Outcome::Refuse => Ok(Verdict::Refuse),
            Outcome::Fail => Err(ScreenError::new("the stand-in model is not loaded")),
        };
        Box::pin(async move { answer })
    }
}

/// A 900 by 600 PNG of one flat `shade`, so two uploads differ in version.
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

/// The framework's auth routes and the public avatar route, with `screen`
/// plugged in and the configuration `variables` name.
async fn boot(
    database: &TestDatabase,
    screen: &RecordingScreen,
    variables: &[(&str, &str)],
) -> Router {
    let pool = database.pool().await;
    let config = anubis::config::AppConfig::from_lookup(|name| {
        if name == "ANUBIS_ENV" {
            return Some("test".to_owned());
        }
        variables
            .iter()
            .find(|(variable, _value)| *variable == name)
            .map(|(_variable, value)| (*value).to_owned())
    })
    .expect("the test config must parse");
    let (mailer, _outbox) = anubis::mail::Mailer::test();
    let rate_limit = anubis::rate_limit::RateLimiter::new(&config.rate_limit);

    Router::new()
        .merge(anubis::auth::avatar_router(pool.clone()))
        .nest(
            "/auth",
            anubis::auth::router_with(
                pool,
                mailer,
                &config,
                &rate_limit,
                anubis::auth::Options::new().image_screen(screen.clone()),
            ),
        )
}

/// Posts `image` as the signed-in account's avatar.
async fn upload(router: &Router, cookie: &str, image: Vec<u8>) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri("/auth/profile/avatar")
        .header(COOKIE, format!("anubis_session={cookie}"))
        .header(CONTENT_TYPE, "image/png")
        .body(Body::from(image))
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The signed-in account's id and its current avatar version.
async fn whoami(router: &Router, cookie: &str) -> (Uuid, Value) {
    let (status, _headers, body) = send(router, "GET", "/auth/me", None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let id = body["user"]["id"]
        .as_str()
        .expect("the profile names its id")
        .parse()
        .expect("an account id is a UUID");
    (id, body["user"]["avatar_version"].clone())
}

/// Every avatar event recorded against `user_id`, oldest first, as
/// `(action, changes)`.
async fn avatar_events(database: &TestDatabase, user_id: Uuid) -> Vec<(String, Value)> {
    let pool = database.pool().await;
    let mut connection = pool.get().await.expect("a connection");
    audit_events::table
        .filter(audit_events::subject_id.eq(user_id))
        .filter(audit_events::action.like("avatar.%"))
        .order(audit_events::created_at.asc())
        .select((audit_events::action, audit_events::changes))
        .load(&mut connection)
        .await
        .expect("the audit query must run")
}

#[tokio::test]
async fn the_screen_sees_each_upload_once_and_only_an_acceptance_stores_it() {
    let Some(database) = TestDatabase::create("avatar_screen_flow").await else {
        return;
    };
    let screen = RecordingScreen::new();
    let router = boot(&database, &screen, &[]).await;
    let cookie = register(&router, &format!("screened-{}@example.com", Uuid::new_v4())).await;
    let (user_id, version) = whoami(&router, &cookie).await;
    assert_eq!(version, Value::Null, "a new account has no picture");

    // Accepted: stored, and the screen saw the decoded source, uncropped, once.
    let (status, body) = upload(&router, &cookie, sample_png(0)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        screen.shown(),
        vec![Shown {
            width: 900,
            height: 600,
            format: ImageFormat::Png,
            rgba_bytes: 900 * 600 * 4,
        }],
    );
    let (_user_id, accepted) = whoami(&router, &cookie).await;
    assert!(accepted.is_string(), "the accepted picture is served");

    // Refused: a neutral 400 with the stable code, and the old picture stays.
    screen.answer(Outcome::Refuse);
    let (status, body) = upload(&router, &cookie, sample_png(200)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["code"], json!(IMAGE_REFUSED));
    let message = body["message"].as_str().expect("a message");
    assert!(
        !message.to_lowercase().contains("explicit") && !message.to_lowercase().contains("nud"),
        "the refusal says nothing about why: {message}",
    );
    assert_eq!(screen.shown().len(), 2, "one look per upload");
    let (_user_id, after_refusal) = whoami(&router, &cookie).await;
    assert_eq!(after_refusal, accepted, "a refused upload stores nothing");

    // Failed: refused too, as a 503 the person may retry, and still nothing
    // stored. A screen that could not decide has not said yes.
    screen.answer(Outcome::Fail);
    let (status, body) = upload(&router, &cookie, sample_png(200)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {body}");
    assert_eq!(body["code"], json!(IMAGE_SCREEN_UNAVAILABLE));
    assert_eq!(screen.shown().len(), 3, "one look per upload");
    let (_user_id, after_failure) = whoami(&router, &cookie).await;
    assert_eq!(after_failure, accepted, "a failed screen stores nothing");

    // The log holds the acceptance and the refusal, never the image. A screen
    // failure is the server's fault rather than the account's act, so it is
    // logged for the operator and not recorded against the person.
    assert_eq!(
        avatar_events(&database, user_id).await,
        vec![
            (
                "avatar.uploaded".to_owned(),
                json!({ "version": { "old": null, "new": accepted } }),
            ),
            ("avatar.refused".to_owned(), json!({})),
        ],
    );
}

#[tokio::test]
async fn an_image_over_the_pixel_budget_is_refused_before_the_screen_sees_it() {
    let Some(database) = TestDatabase::create("avatar_screen_flow").await else {
        return;
    };
    let screen = RecordingScreen::new();
    // 900 by 600 is 540,000 pixels, over a budget of half a megapixel.
    let router = boot(&database, &screen, &[("AVATAR_MAX_PIXELS", "500000")]).await;
    let cookie = register(&router, &format!("budget-{}@example.com", Uuid::new_v4())).await;

    let (status, body) = upload(&router, &cookie, sample_png(0)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(
        body["code"],
        Value::Null,
        "a size refusal is not a screen's"
    );
    assert!(
        body["message"].as_str().is_some_and(
            |message| message.contains("too large") && message.contains("0.5 megapixels")
        ),
        "the refusal says the picture is too large: {body}",
    );
    assert!(
        screen.shown().is_empty(),
        "nothing over budget reaches the screen"
    );
}

#[tokio::test]
async fn uploads_and_removals_are_audited_with_their_versions() {
    let Some(database) = TestDatabase::create("avatar_screen_flow").await else {
        return;
    };
    let screen = RecordingScreen::new();
    let router = boot(&database, &screen, &[]).await;
    let cookie = register(&router, &format!("audited-{}@example.com", Uuid::new_v4())).await;
    let (user_id, _none) = whoami(&router, &cookie).await;

    let (status, _body) = upload(&router, &cookie, sample_png(0)).await;
    assert_eq!(status, StatusCode::OK);
    let (_user_id, first) = whoami(&router, &cookie).await;

    let (status, _body) = upload(&router, &cookie, sample_png(200)).await;
    assert_eq!(status, StatusCode::OK);
    let (_user_id, second) = whoami(&router, &cookie).await;
    assert_ne!(first, second, "a new picture is a new version");

    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        "/auth/profile/avatar",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Removing a picture that is already gone succeeds and records nothing,
    // because nothing changed.
    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        "/auth/profile/avatar",
        None,
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert_eq!(
        avatar_events(&database, user_id).await,
        vec![
            (
                "avatar.uploaded".to_owned(),
                json!({ "version": { "old": null, "new": first } }),
            ),
            (
                "avatar.uploaded".to_owned(),
                json!({ "version": { "old": first, "new": second } }),
            ),
            (
                "avatar.removed".to_owned(),
                json!({ "version": { "old": second, "new": null } }),
            ),
        ],
    );
}

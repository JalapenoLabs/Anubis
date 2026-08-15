//! The OAuth sign-in flow, end to end, against a mock OpenID Connect provider.
//!
//! A real provider cannot run in a test, so this test *is* one: a small axum
//! server serves a discovery document, a JWKS, and a token endpoint that mints
//! ID tokens signed with a fixed test key. The framework points at it through
//! `GOOGLE_OAUTH_ISSUER`, which is the same escape hatch a self-hosted
//! identity server uses, so nothing about the code under test is special-cased
//! for the test. That makes the whole path real: the redirect out, PKCE, the
//! code exchange, ID token verification, identity linking, account creation,
//! and the session cookie, over a real Postgres.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anubis::schema::{organization_memberships, organizations, team_memberships, teams};
use axum::extract::State;
use axum::http::header::{CONTENT_TYPE, COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use http_body_util::BodyExt;
use openidconnect::core::{
    CoreIdToken, CoreIdTokenClaims, CoreIdTokenFields, CoreJsonWebKeySet, CoreJwsSigningAlgorithm,
    CoreProviderMetadata, CoreResponseType, CoreRsaPrivateSigningKey, CoreSubjectIdentifierType,
    CoreTokenResponse, CoreTokenType,
};
use openidconnect::{
    AccessToken, Audience, AuthUrl, EmptyAdditionalClaims, EmptyAdditionalProviderMetadata,
    EmptyExtraTokenFields, EndUserEmail, EndUserFamilyName, EndUserGivenName, IssuerUrl,
    JsonWebKeyId, JsonWebKeySetUrl, Nonce, PrivateSigningKey, ResponseTypes, StandardClaims,
    SubjectIdentifier, TokenUrl,
};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

/// A throwaway RSA key, generated for this test and used nowhere else.
///
/// The mock provider signs its ID tokens with it and publishes the matching
/// public key through its JWKS, which is what the framework verifies against.
const TEST_SIGNING_KEY: &str = "\
-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEA37bHgk0R4aCrUt50TV/9HoqEYs44CTL/DG8g+iaoHp7EGNjl
JKP+bBwrZ6SPLw68AeNB3vf+K6ZnVMrWD1n4y4WSeRAcAnv22W2c6TE+OzjA+bSH
b36z769JbxWHO8Dtp7WG8FQnzXAJU2qzBwvTyCdSVwsluh7Fbdd+3o5nTKL46gou
lDnCzEiG3Ogw7WOG0eDKs3r+hSgis4MxaaE+vIBgfNx4vipaRcIciI92mbeAXSBH
gT2wEVUMksEKyosnEo4dKPBTjGUgq/nNkqZ1ZRiIe67MkvO7TK786fFBtOfyDGJT
d+nUvItA8/PkCFOR2QE2PPyh+MaPoI4NIbmumwIDAQABAoIBABbNcph9co0k61GP
Cxu35Pzv8X6AtoV5hTWnPh1BQ3GbjTFbKkAJ1yz90g7GXzHUtqUanOQ1MtsQIwgp
hJgb+5gDDWL5mWFHcWnIGm5KbqVqq4DIPeXHbF/J5hpEf3w/tfmaLx7f9Q6jlM/D
2GuncPa9y07D/Bx0dnszs+LLcQwGf6f15/0Jk8UqRY5X7QXZYews8Brj+j3ruIAu
WwgmTDk4iwSMSccyd2tGe1fvD4bt6FWsapxOruL3QE8qgyZP9BBvXYyacWJx1xE4
Hx//aT4Fy0NCE7giMvExzGqsZUMnvO0E0Vtac90pSyHL8EJWNCXmZKEuZvkrWioh
dLvIhQECgYEA/nLGAu7NxffrZ5+DsLdiRbneuEK1piySrJPOGNC9QGRzIPBOyTyI
Ag2yMeDSzc3SxAnD7OpnFRzQ8BPT8zDaRdyVaFQmaHJ37PLHOG9BwcaCxhloHa5K
/7FbWFmoKLuhB5dfrN9f5hMFkCx2dekxXCvEwT/rgH1buqM+oTJHJrECgYEA4RQG
jGowyRCHoiD5/HbBfjK5jbHq28yNPntrst4g1jMUIyXfm8BSYOPUzF0k62cf72Nt
gwfrVK7+AeIm8sdA4mwbHxCSz1I3CBIji++i5CiWBEJ+Nu7bM6iihvW8EfCAPpYO
DoWw5heyImeQXBleDVtHtCln45FnBnJJo9cGlQsCgYEA7CBypTIbf4XszUL4oLvt
1KsChphRnh5rFwArGFhN6D3PoVegpZso1E8FeMgcmKRS3V36lheJBcyyELk1zc8e
IAruE91Tr0XbCObb/gExUrP3lALr3e9q5hIepMS/Ct3kN/k/7lt00TwBw6OfYxi+
l7x+YKAC2kB7KZ5odosEAGECgYB53lrxWmoR5CZcbdiNj0uTZim8BBKzcl0j8LXO
wqEq+bs0kMQzU/4GwjWtdd2QrGTJPJ/GK9qLHrkgEfCe0a5bKsfAmTu0j8KGVzPy
CA291g/sPIiUe94qaWufAZ0UZZE60grIaDDxVPE52bN7eqzHNJ5teWHsAQW0otsm
oD3LIwKBgQDuVUQG8IjlQcJKEF2D9mqNem0lPt+7NFv39Sq4ElKjqEs4HTcDiNuE
LrH+wOF+7MGA3YrZdedhCXWNO78b+7x1GA6qK7fyY4eQQqfA//3JkpUNbhx3jktQ
MJKr6V91kG2lfwN7y7aJ9sUGL4czjQR6NNWnts8JGatUJNs7WcwX3A==
-----END RSA PRIVATE KEY-----
";

/// The client id the mock provider issues to this application.
const CLIENT_ID: &str = "anubis-test-client";

/// The origin the browser is pretending to be on.
const APP_URL: &str = "http://localhost:3000";

/// What the mock provider will assert about the next person to sign in.
#[derive(Clone)]
struct Assertion {
    subject: String,
    email: String,
    email_verified: bool,
}

/// The mock provider's mutable half: its issuer, and the next assertion.
struct MockState {
    issuer: String,
    /// The nonce from the authorization request the test just read.
    nonce: String,
    assertion: Assertion,
}

/// Serves discovery, keys, and tokens for one issuer.
async fn start_mock_provider() -> (String, Arc<Mutex<MockState>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port must be available");
    let port = listener
        .local_addr()
        .expect("the listener must have an address")
        .port();
    let issuer = format!("http://127.0.0.1:{port}");

    let state = Arc::new(Mutex::new(MockState {
        issuer: issuer.clone(),
        nonce: String::new(),
        assertion: Assertion {
            subject: "subject-unset".to_owned(),
            email: "unset@example.com".to_owned(),
            email_verified: true,
        },
    }));

    let router = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/jwks", get(jwks))
        .route("/token", post(token))
        .with_state(Arc::clone(&state));

    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("the mock provider must serve");
    });

    (issuer, state)
}

fn signing_key() -> CoreRsaPrivateSigningKey {
    CoreRsaPrivateSigningKey::from_pem(
        TEST_SIGNING_KEY,
        Some(JsonWebKeyId::new("anubis-test".to_owned())),
    )
    .expect("the test key must parse")
}

async fn discovery(State(state): State<Arc<Mutex<MockState>>>) -> impl IntoResponse {
    let issuer = state
        .lock()
        .expect("the mock state is never poisoned")
        .issuer
        .clone();

    let metadata = CoreProviderMetadata::new(
        IssuerUrl::new(issuer.clone()).expect("the issuer is a URL"),
        AuthUrl::new(format!("{issuer}/authorize")).expect("the auth URL is a URL"),
        JsonWebKeySetUrl::new(format!("{issuer}/jwks")).expect("the JWKS URL is a URL"),
        vec![ResponseTypes::new(vec![CoreResponseType::Code])],
        vec![CoreSubjectIdentifierType::Public],
        vec![CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256],
        EmptyAdditionalProviderMetadata {},
    )
    .set_token_endpoint(Some(
        TokenUrl::new(format!("{issuer}/token")).expect("the token URL is a URL"),
    ));

    Json(metadata)
}

async fn jwks() -> impl IntoResponse {
    Json(CoreJsonWebKeySet::new(vec![
        signing_key().as_verification_key(),
    ]))
}

async fn token(State(state): State<Arc<Mutex<MockState>>>) -> impl IntoResponse {
    let (issuer, nonce, assertion) = {
        let state = state.lock().expect("the mock state is never poisoned");
        (
            state.issuer.clone(),
            state.nonce.clone(),
            state.assertion.clone(),
        )
    };

    let claims = CoreIdTokenClaims::new(
        IssuerUrl::new(issuer).expect("the issuer is a URL"),
        vec![Audience::new(CLIENT_ID.to_owned())],
        Utc::now() + Duration::minutes(5),
        Utc::now(),
        StandardClaims::new(SubjectIdentifier::new(assertion.subject))
            .set_email(Some(EndUserEmail::new(assertion.email)))
            .set_email_verified(Some(assertion.email_verified))
            .set_given_name(Some(EndUserGivenName::new("Ada".to_owned()).into()))
            .set_family_name(Some(EndUserFamilyName::new("Lovelace".to_owned()).into())),
        EmptyAdditionalClaims {},
    )
    // The framework replays the nonce it stored; a mismatch must fail.
    .set_nonce(Some(Nonce::new(nonce)));

    let id_token = CoreIdToken::new(
        claims,
        &signing_key(),
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
        None,
        None,
    )
    .expect("the ID token must sign");

    Json(CoreTokenResponse::new(
        AccessToken::new("mock-access-token".to_owned()),
        CoreTokenType::Bearer,
        CoreIdTokenFields::new(Some(id_token), EmptyExtraTokenFields {}),
    ))
}

/// Sends one request to the application under test.
async fn send(
    router: &Router,
    method: &str,
    path: &str,
    body: Option<&Value>,
    session_cookie: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(cookie) = session_cookie {
        builder = builder.header(COOKIE, format!("anubis_session={cookie}"));
    }

    let request = match body {
        Some(value) => {
            builder
                .header(CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_vec(value).expect("body must serialize"),
                ))
        }
        None => builder.body(axum::body::Body::empty()),
    }
    .expect("request must build");

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("request must complete");

    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body must collect")
        .to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, value)
}

fn location(headers: &HeaderMap) -> String {
    headers
        .get(LOCATION)
        .expect("a redirect must carry a Location")
        .to_str()
        .expect("Location is ASCII")
        .to_owned()
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

/// The query parameters of a URL, as a map.
fn query_of(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .expect("the URL must parse")
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

/// Tells the mock which nonce to echo, and what to assert about the user.
fn arm_mock(mock: &Arc<Mutex<MockState>>, nonce: &str, assertion: Assertion) {
    let mut state = mock.lock().expect("the mock state is never poisoned");
    nonce.clone_into(&mut state.nonce);
    state.assertion = assertion;
}

/// Runs `start`, tells the mock what to assert, and completes the callback.
async fn sign_in_with(
    router: &Router,
    state: &Arc<Mutex<MockState>>,
    start_path: &str,
    assertion: Assertion,
) -> (StatusCode, HeaderMap) {
    let (status, headers, _body) = send(router, "GET", start_path, None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER, "start must redirect");

    let authorize = query_of(&location(&headers));
    arm_mock(state, &authorize["nonce"], assertion);

    let callback = format!(
        "/auth/oauth/google/callback?code=mock-code&state={}",
        authorize["state"],
    );
    let (status, headers, _body) = send(router, "GET", &callback, None, None).await;
    (status, headers)
}

/// Discovery answers with the providers the environment enabled, and nothing
/// else: the sign-in page renders buttons from this list, so a provider that
/// would fail at click time must be absent from it.
#[tokio::test]
async fn provider_discovery_lists_what_the_environment_configured() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping oauth_flow test: DATABASE_URL is not set");
        return;
    };

    let pool = anubis::db::connect(&database_url)
        .await
        .expect("database must be reachable");

    // An application with no OAuth credentials set: the registry knows Google,
    // the deployment does not, and the page must render no button for it.
    let bare = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        "APP_URL" => Some(APP_URL.to_owned()),
        _ => None,
    })
    .expect("test config must parse");
    let (mailer, _outbox) = anubis::mail::Mailer::test();
    let router = Router::new().nest("/auth", anubis::auth::router(pool.clone(), mailer, &bare));

    let (status, _headers, body) = send(&router, "GET", "/auth/oauth/providers", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["providers"], serde_json::json!([]));

    // The same application with Google's credentials set.
    let configured = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        "APP_URL" => Some(APP_URL.to_owned()),
        "GOOGLE_OAUTH_CLIENT_ID" => Some(CLIENT_ID.to_owned()),
        "GOOGLE_OAUTH_CLIENT_SECRET" => Some("mock-client-secret".to_owned()),
        _ => None,
    })
    .expect("test config must parse");
    let (mailer, _outbox) = anubis::mail::Mailer::test();
    let router = Router::new().nest("/auth", anubis::auth::router(pool, mailer, &configured));

    let (status, _headers, body) = send(&router, "GET", "/auth/oauth/providers", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["providers"],
        serde_json::json!([{ "key": "google", "display_name": "Google" }]),
        "discovery must name the provider and nothing about its credentials",
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn oauth_sign_in_creates_links_and_refuses() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping oauth_flow test: DATABASE_URL is not set");
        return;
    };

    anubis::db::run_pending_migrations(&database_url)
        .await
        .expect("migrations must apply");
    let pool = anubis::db::connect(&database_url)
        .await
        .expect("database must be reachable");

    let (issuer, mock) = start_mock_provider().await;

    let config = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        "APP_URL" => Some(APP_URL.to_owned()),
        "GOOGLE_OAUTH_CLIENT_ID" => Some(CLIENT_ID.to_owned()),
        "GOOGLE_OAUTH_CLIENT_SECRET" => Some("mock-client-secret".to_owned()),
        "GOOGLE_OAUTH_ISSUER" => Some(issuer.clone()),
        _ => None,
    })
    .expect("test config must parse");
    assert_eq!(config.oauth.len(), 1, "the provider must be enabled");

    let (mailer, _outbox) = anubis::mail::Mailer::test();
    let router = Router::new().nest("/auth", anubis::auth::router(pool, mailer, &config));

    // ------------------------------------------------------------------
    // Start: the browser is sent to the provider with everything the spec
    // wants, and nothing it does not.
    // ------------------------------------------------------------------
    let (status, headers, _body) = send(
        &router,
        "GET",
        "/auth/oauth/google/start?next=%2Fmembers",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    let authorize_url = location(&headers);
    assert!(
        authorize_url.starts_with(&format!("{issuer}/authorize")),
        "got: {authorize_url}",
    );
    let authorize = query_of(&authorize_url);
    assert_eq!(authorize["client_id"], CLIENT_ID);
    assert_eq!(authorize["response_type"], "code");
    assert_eq!(
        authorize["redirect_uri"],
        format!("{APP_URL}/auth/oauth/google/callback"),
    );
    assert_eq!(authorize["code_challenge_method"], "S256");
    assert!(!authorize["code_challenge"].is_empty());
    assert!(!authorize["state"].is_empty());
    assert!(!authorize["nonce"].is_empty());
    for scope in ["openid", "email", "profile"] {
        assert!(
            authorize["scope"].split(' ').any(|asked| asked == scope),
            "scope {scope} is missing from {}",
            authorize["scope"],
        );
    }

    // ------------------------------------------------------------------
    // A callback nobody started fails cleanly, and so does a missing code.
    // ------------------------------------------------------------------
    for callback in [
        "/auth/oauth/google/callback?code=x&state=never-issued",
        "/auth/oauth/google/callback",
    ] {
        let (status, headers, _body) = send(&router, "GET", callback, None, None).await;
        assert_eq!(status, StatusCode::SEE_OTHER, "for {callback}");
        assert_eq!(
            location(&headers),
            format!("{APP_URL}/sign-in?error=oauth_expired"),
            "for {callback}",
        );
    }

    // A provider the environment never configured is refused the same way.
    let (status, headers, _body) =
        send(&router, "GET", "/auth/oauth/gitlab/start", None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        location(&headers),
        format!("{APP_URL}/sign-in?error=oauth_unavailable"),
    );

    // The provider refusing is its own outcome, not a failure of ours.
    let (status, headers, _body) = send(
        &router,
        "GET",
        "/auth/oauth/google/callback?error=access_denied",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        location(&headers),
        format!("{APP_URL}/sign-in?error=oauth_denied"),
    );

    // ------------------------------------------------------------------
    // An unknown verified address creates the account, bootstrapped exactly
    // as registration bootstraps one, and lands on the preserved destination.
    // ------------------------------------------------------------------
    let fresh_email = format!("oauth-fresh-{}@example.com", Uuid::new_v4());
    let fresh_subject = format!("subject-{}", Uuid::new_v4());
    let (status, headers) = sign_in_with(
        &router,
        &mock,
        "/auth/oauth/google/start?next=%2Fmembers",
        Assertion {
            subject: fresh_subject.clone(),
            email: fresh_email.clone(),
            email_verified: true,
        },
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location(&headers), format!("{APP_URL}/members"));

    let cookie = session_token(&headers);
    let (status, _headers, body) = send(&router, "GET", "/auth/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["user"]["email"], Value::String(fresh_email.clone()));
    assert_eq!(body["user"]["email_verified"], Value::Bool(true));
    assert_eq!(body["user"]["first_name"], Value::String("Ada".to_owned()));
    assert_eq!(
        body["user"]["last_name"],
        Value::String("Lovelace".to_owned()),
    );
    let created_id = body["user"]["id"].clone();

    // ------------------------------------------------------------------
    // The same subject signs back into the same account, even after the
    // address at the provider changes: the subject is the identity.
    // ------------------------------------------------------------------
    let (status, headers) = sign_in_with(
        &router,
        &mock,
        "/auth/oauth/google/start",
        Assertion {
            subject: fresh_subject,
            email: format!("renamed-{}@example.com", Uuid::new_v4()),
            email_verified: true,
        },
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    // No destination was asked for, so the flow lands at the root.
    assert_eq!(location(&headers), format!("{APP_URL}/"));

    let cookie = session_token(&headers);
    let (status, _headers, body) = send(&router, "GET", "/auth/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["user"]["id"], created_id, "the identity must win");
    assert_eq!(
        body["user"]["email"],
        Value::String(fresh_email),
        "linking must never rewrite the account's own address",
    );

    // ------------------------------------------------------------------
    // A password account is adopted, not duplicated, when the provider
    // vouches for its address.
    // ------------------------------------------------------------------
    let existing_email = format!("oauth-existing-{}@example.com", Uuid::new_v4());
    let credentials =
        serde_json::json!({ "email": existing_email, "password": "correct horse battery staple" });
    let (status, headers, body) =
        send(&router, "POST", "/auth/register", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let registered_id = body["user"]["id"].clone();
    let _registered_cookie = session_token(&headers);

    let (status, headers) = sign_in_with(
        &router,
        &mock,
        "/auth/oauth/google/start",
        Assertion {
            subject: format!("subject-{}", Uuid::new_v4()),
            email: existing_email.clone(),
            email_verified: true,
        },
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = session_token(&headers);
    let (status, _headers, body) = send(&router, "GET", "/auth/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["user"]["id"], registered_id,
        "a verified address must adopt the account that already owns it",
    );

    // ------------------------------------------------------------------
    // An address the provider will not vouch for is refused: it is the only
    // proof of ownership there is.
    // ------------------------------------------------------------------
    let (status, headers) = sign_in_with(
        &router,
        &mock,
        "/auth/oauth/google/start",
        Assertion {
            subject: format!("subject-{}", Uuid::new_v4()),
            email: existing_email,
            email_verified: false,
        },
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        location(&headers),
        format!("{APP_URL}/sign-in?error=oauth_email_unverified"),
    );

    // ------------------------------------------------------------------
    // A state token is single-use: a replayed callback fails like a stale one.
    // ------------------------------------------------------------------
    let (status, headers, _body) =
        send(&router, "GET", "/auth/oauth/google/start", None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let authorize = query_of(&location(&headers));
    arm_mock(
        &mock,
        &authorize["nonce"],
        Assertion {
            subject: format!("subject-{}", Uuid::new_v4()),
            email: format!("oauth-replay-{}@example.com", Uuid::new_v4()),
            email_verified: true,
        },
    );
    let callback = format!(
        "/auth/oauth/google/callback?code=mock-code&state={}",
        authorize["state"],
    );

    let (status, _headers, _body) = send(&router, "GET", &callback, None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (status, headers, _body) = send(&router, "GET", &callback, None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        location(&headers),
        format!("{APP_URL}/sign-in?error=oauth_expired"),
        "a state token must not be reusable",
    );

    // ------------------------------------------------------------------
    // A destination that leaves the application is dropped, not followed.
    // ------------------------------------------------------------------
    let (status, headers) = sign_in_with(
        &router,
        &mock,
        "/auth/oauth/google/start?next=https%3A%2F%2Fevil.example",
        Assertion {
            subject: format!("subject-{}", Uuid::new_v4()),
            email: format!("oauth-open-redirect-{}@example.com", Uuid::new_v4()),
            email_verified: true,
        },
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location(&headers), format!("{APP_URL}/"));
}

/// A closed deployment signs existing accounts in and creates none.
///
/// The button is the same door as the sign-up form, so the registration mode
/// gates it identically: an address nobody owns is refused, and one that
/// already has an account is linked and signed in as always.
#[tokio::test]
async fn oauth_sign_up_obeys_the_registration_mode() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping oauth_flow test: DATABASE_URL is not set");
        return;
    };

    anubis::db::run_pending_migrations(&database_url)
        .await
        .expect("migrations must apply");
    let pool = anubis::db::connect(&database_url)
        .await
        .expect("database must be reachable");

    let (issuer, mock) = start_mock_provider().await;
    let variables = move |name: &str| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        "APP_URL" => Some(APP_URL.to_owned()),
        "GOOGLE_OAUTH_CLIENT_ID" => Some(CLIENT_ID.to_owned()),
        "GOOGLE_OAUTH_CLIENT_SECRET" => Some("mock-client-secret".to_owned()),
        "GOOGLE_OAUTH_ISSUER" => Some(issuer.clone()),
        _ => None,
    };

    // The deployment before it closed, which is where the account comes from.
    let open = anubis::config::AppConfig::from_lookup(&variables).expect("test config must parse");
    let (mailer, _outbox) = anubis::mail::Mailer::test();
    let open_router =
        Router::new().nest("/auth", anubis::auth::router(pool.clone(), mailer, &open));

    let member_email = format!("oauth-member-{}@example.com", Uuid::new_v4());
    let credentials =
        serde_json::json!({ "email": member_email, "password": "correct horse battery staple" });
    let (status, _headers, body) = send(
        &open_router,
        "POST",
        "/auth/register",
        Some(&credentials),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let member_id = body["user"]["id"].clone();

    let closed = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_REGISTRATION" => Some("invite_only".to_owned()),
        other => variables(other),
    })
    .expect("test config must parse");
    let (mailer, _outbox) = anubis::mail::Mailer::test();
    let router = Router::new().nest("/auth", anubis::auth::router(pool, mailer, &closed));

    // An address no account owns is a registration, and this deployment
    // accepts none.
    let stranger_email = format!("oauth-stranger-{}@example.com", Uuid::new_v4());
    let (status, headers) = sign_in_with(
        &router,
        &mock,
        "/auth/oauth/google/start",
        Assertion {
            subject: format!("subject-{}", Uuid::new_v4()),
            email: stranger_email,
            email_verified: true,
        },
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        location(&headers),
        format!("{APP_URL}/sign-in?error=oauth_registration_closed"),
    );

    // The account that already exists signs in, because linking an identity to
    // it creates nothing.
    let (status, headers) = sign_in_with(
        &router,
        &mock,
        "/auth/oauth/google/start",
        Assertion {
            subject: format!("subject-{}", Uuid::new_v4()),
            email: member_email,
            email_verified: true,
        },
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = session_token(&headers);
    let (status, _headers, body) = send(&router, "GET", "/auth/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["user"]["id"], member_id);
}

/// A shared deployment puts an OAuth signup where it puts a form signup.
///
/// Both paths call the one bootstrap function, and this is the assertion that
/// keeps them from drifting: the form creates the organization configuration
/// named, and the button joins that organization and its default team rather
/// than minting a private one.
#[tokio::test]
async fn oauth_sign_up_joins_the_shared_organization() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping oauth_flow test: DATABASE_URL is not set");
        return;
    };

    anubis::db::run_pending_migrations(&database_url)
        .await
        .expect("migrations must apply");
    let pool = anubis::db::connect(&database_url)
        .await
        .expect("database must be reachable");

    let (issuer, mock) = start_mock_provider().await;

    // Named per run, because this suite shares one database with the others
    // and the count below is a claim about this deployment's organization.
    let shared_organization = format!("Shared {}", Uuid::new_v4());
    let config = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        "APP_URL" => Some(APP_URL.to_owned()),
        "GOOGLE_OAUTH_CLIENT_ID" => Some(CLIENT_ID.to_owned()),
        "GOOGLE_OAUTH_CLIENT_SECRET" => Some("mock-client-secret".to_owned()),
        "GOOGLE_OAUTH_ISSUER" => Some(issuer.clone()),
        "ANUBIS_BOOTSTRAP" => Some("shared".to_owned()),
        "ANUBIS_SHARED_ORGANIZATION" => Some(shared_organization.clone()),
        _ => None,
    })
    .expect("test config must parse");

    let (mailer, _outbox) = anubis::mail::Mailer::test();
    let router = Router::new().nest("/auth", anubis::auth::router(pool.clone(), mailer, &config));

    // The sign-up form gets there first and creates the organization.
    let form_email = format!("shared-form-{}@example.com", Uuid::new_v4());
    let credentials =
        serde_json::json!({ "email": form_email, "password": "correct horse battery staple" });
    let (status, _headers, body) =
        send(&router, "POST", "/auth/register", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    // The button arrives at an organization that already exists, and joins it.
    let (status, headers) = sign_in_with(
        &router,
        &mock,
        "/auth/oauth/google/start",
        Assertion {
            subject: format!("subject-{}", Uuid::new_v4()),
            email: format!("shared-oauth-{}@example.com", Uuid::new_v4()),
            email_verified: true,
        },
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let _session = session_token(&headers);

    let mut connection = pool.get().await.expect("a connection");
    let organization_ids: Vec<Uuid> = organizations::table
        .filter(organizations::name.eq(&shared_organization))
        .select(organizations::id)
        .load(&mut connection)
        .await
        .expect("the query must run");
    assert_eq!(
        organization_ids.len(),
        1,
        "one organization, whichever door the account came through",
    );
    let organization_id = organization_ids[0];

    let members: i64 = organization_memberships::table
        .filter(organization_memberships::organization_id.eq(organization_id))
        .count()
        .get_result(&mut connection)
        .await
        .expect("the count must run");
    assert_eq!(members, 2, "the form signup and the OAuth signup");

    let team_members: i64 = team_memberships::table
        .inner_join(teams::table)
        .filter(teams::organization_id.eq(organization_id))
        .count()
        .get_result(&mut connection)
        .await
        .expect("the count must run");
    assert_eq!(team_members, 2, "both in the organization's default team");
}

//! The living templates, end to end against a real Postgres database.
//!
//! One narrative covers everything a scaffolded model owes its application:
//! create, list with the locked pagination envelope, nest a child under its
//! parent, refuse another tenant's records with `404`, refuse a read-only
//! member's writes with `403`, update, and destroy. When `anubis scaffold
//! model` transforms these templates, it transforms this test with them, so
//! every generated model arrives with the same proof.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

use anubis::mail::TestOutbox;
use anubis::roles::RoleSet;
use axum::http::header::{CONTENT_TYPE, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::{Router, body::Body};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

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
        Some(value) => builder
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(value).expect("body must serialize"),
            )),
        None => builder.body(Body::empty()),
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

async fn register(router: &Router, email: &str) -> String {
    let credentials = json!({ "email": email, "password": "correct horse battery staple" });
    let (status, headers, body) =
        send(router, "POST", "/auth/register", Some(&credentials), None).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    session_token(&headers)
}

/// The id of the team registration bootstrapped for this account.
async fn bootstrapped_team(router: &Router, cookie: &str) -> String {
    let (status, _headers, body) =
        send(router, "GET", "/tenancy/memberships", None, Some(cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    body["organizations"][0]["teams"][0]["id"]
        .as_str()
        .expect("registration bootstraps one team")
        .to_owned()
}

/// Invites `email` to `team_id` with `roles` and claims the invitation.
async fn invite_and_claim(
    router: &Router,
    outbox: &TestOutbox,
    admin_cookie: &str,
    joining_cookie: &str,
    team_id: &str,
    email: &str,
    roles: &[&str],
) {
    let invite = json!({ "email": email, "team_id": team_id, "roles": roles });
    let (status, _headers, body) = send(
        router,
        "POST",
        "/tenancy/invitations",
        Some(&invite),
        Some(admin_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let invitation = outbox
        .emails()
        .into_iter()
        .find(|sent| sent.to == email && sent.subject.contains("invited"))
        .expect("the invitation email must be in the outbox");
    let (_before, rest) = invitation
        .text_body
        .split_once("token=")
        .expect("the email must contain a link");
    let token = rest.split_whitespace().next().unwrap_or_default();

    let (status, _headers, body) = send(
        router,
        "POST",
        "/tenancy/invitations/claim",
        Some(&json!({ "token": token })),
        Some(joining_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn the_living_templates_serve_a_full_crud_slice() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping scaffolding_flow test: DATABASE_URL is not set");
        return;
    };

    anubis::db::run_pending_migrations(&database_url)
        .await
        .expect("framework migrations must apply");
    anubis::db::run_app_migrations(&database_url, anubis_starter::APP_MIGRATIONS)
        .await
        .expect("application migrations must apply");
    let pool = anubis::db::connect(&database_url)
        .await
        .expect("database must be reachable");

    let config = anubis::config::AppConfig::from_lookup(|name| match name {
        "ANUBIS_ENV" => Some("test".to_owned()),
        _ => None,
    })
    .expect("test config must parse");
    let roles = RoleSet::from_yaml(anubis_starter::ROLES_YML).expect("roles.yml must parse");
    let (mailer, outbox) = anubis::mail::Mailer::test();

    let router = Router::new()
        .nest(
            "/auth",
            anubis::auth::router(pool.clone(), mailer.clone(), &config),
        )
        .nest(
            "/tenancy",
            anubis::tenancy::router(pool.clone(), mailer, roles.clone(), &config),
        )
        .nest("/account", anubis_starter::account_router(&pool, &roles))
        .layer(anubis::guard::layer(pool, roles));

    let run = Uuid::new_v4();
    let owner_email = format!("concept-owner-{run}@example.com");
    let outsider_email = format!("concept-outsider-{run}@example.com");

    let owner_cookie = register(&router, &owner_email).await;
    let outsider_cookie = register(&router, &outsider_email).await;
    let team_id = bootstrapped_team(&router, &owner_cookie).await;
    let concepts_path = format!("/account/teams/{team_id}/creative-concepts");

    // Create the parent. The owner registered as an admin, which includes
    // editor, which is granted `manage` on both models.
    let (status, _headers, body) = send(
        &router,
        "POST",
        &concepts_path,
        Some(&json!({ "name": "Lighthouse" })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let concept_id = body["creative_concept"]["id"]
        .as_str()
        .expect("the response carries the record")
        .to_owned();
    assert_eq!(body["creative_concept"]["name"], json!("Lighthouse"));

    // A blank name never reaches the database.
    let (status, _headers, _body) = send(
        &router,
        "POST",
        &concepts_path,
        Some(&json!({ "name": "   " })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    for name in ["Beacon", "Compass"] {
        let (status, _headers, body) = send(
            &router,
            "POST",
            &concepts_path,
            Some(&json!({ "name": name })),
            Some(&owner_cookie),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "body: {body}");
    }

    // Listing follows the locked conventions: plural key, pagination envelope,
    // 1-based pages, whitelisted sort, per-field filter.
    let (status, _headers, body) = send(
        &router,
        "GET",
        &format!("{concepts_path}?page=1&limit=2&sort=name"),
        None,
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let page = body["creative_concepts"]
        .as_array()
        .expect("the plural key carries the records");
    assert_eq!(page.len(), 2);
    assert_eq!(page[0]["name"], json!("Beacon"));
    assert_eq!(page[1]["name"], json!("Compass"));
    assert_eq!(body["pagination"]["page"], json!(1));
    assert_eq!(body["pagination"]["limit"], json!(2));
    assert_eq!(body["pagination"]["total_items"], json!(3));
    assert_eq!(body["pagination"]["total_pages"], json!(2));

    let (status, _headers, body) = send(
        &router,
        "GET",
        &format!("{concepts_path}?page=2&limit=2&sort=name&name=house"),
        None,
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        body["pagination"]["total_items"],
        json!(1),
        "filtered count"
    );
    assert!(
        body["creative_concepts"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "page 2 of a single filtered match is empty: {body}",
    );

    // Create the child under its parent; the parent comes from the route.
    let things_path = format!("/account/creative-concepts/{concept_id}/tangible-things");
    let (status, _headers, body) = send(
        &router,
        "POST",
        &things_path,
        Some(&json!({ "name": "Fresnel lens", "description": "Casts the beam" })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let thing_id = body["tangible_thing"]["id"]
        .as_str()
        .expect("the response carries the record")
        .to_owned();
    assert_eq!(
        body["tangible_thing"]["creative_concept_id"],
        json!(concept_id)
    );

    let (status, _headers, body) =
        send(&router, "GET", &things_path, None, Some(&owner_cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["pagination"]["total_items"], json!(1));

    // Another tenant's records are indistinguishable from records that do not
    // exist: both answer 404, on the parent and on the child.
    let thing_path = format!("/account/tangible-things/{thing_id}");
    let concept_path = format!("/account/creative-concepts/{concept_id}");
    for path in [&concept_path, &things_path, &thing_path] {
        let (status, _headers, _body) =
            send(&router, "GET", path, None, Some(&outsider_cookie)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "leaked {path}");
    }
    let ghost = format!("/account/creative-concepts/{}", Uuid::new_v4());
    let (status, _headers, _body) = send(&router, "GET", &ghost, None, Some(&owner_cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Signing out entirely is a 401, before any record is touched.
    let (status, _headers, _body) = send(&router, "GET", &concept_path, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A `default` member reads the team's records but writes none of them.
    invite_and_claim(
        &router,
        &outbox,
        &owner_cookie,
        &outsider_cookie,
        &team_id,
        &outsider_email,
        &["default"],
    )
    .await;

    let (status, _headers, body) =
        send(&router, "GET", &concept_path, None, Some(&outsider_cookie)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (status, _headers, _body) = send(
        &router,
        "POST",
        &concepts_path,
        Some(&json!({ "name": "Contraband" })),
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _headers, _body) = send(
        &router,
        "PATCH",
        &thing_path,
        Some(&json!({ "name": "Contraband" })),
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _headers, _body) = send(
        &router,
        "DELETE",
        &concept_path,
        None,
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Update: the trigger keeps updated_at moving, and a blank description
    // clears the column.
    let (status, _headers, body) = send(
        &router,
        "PATCH",
        &thing_path,
        Some(&json!({ "name": "First-order lens", "description": "  " })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["tangible_thing"]["name"], json!("First-order lens"));
    assert_eq!(body["tangible_thing"]["description"], Value::Null);
    assert_ne!(
        body["tangible_thing"]["updated_at"], body["tangible_thing"]["created_at"],
        "the set_updated_at trigger must fire",
    );

    // A parent from another team is refused by the valid_* scoping method.
    let outsider_team = bootstrapped_team(&router, &outsider_cookie).await;
    let (status, _headers, body) = send(
        &router,
        "POST",
        &format!("/account/teams/{outsider_team}/creative-concepts"),
        Some(&json!({ "name": "Elsewhere" })),
        Some(&outsider_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let foreign_concept_id = body["creative_concept"]["id"]
        .as_str()
        .expect("the response carries the record")
        .to_owned();

    let (status, _headers, _body) = send(
        &router,
        "PATCH",
        &thing_path,
        Some(&json!({ "creative_concept_id": foreign_concept_id })),
        Some(&owner_cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "cross-team parent refused");

    // Destroy: the child goes, then the parent.
    let (status, _headers, _body) =
        send(&router, "DELETE", &thing_path, None, Some(&owner_cookie)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _headers, _body) =
        send(&router, "GET", &thing_path, None, Some(&owner_cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _headers, _body) =
        send(&router, "DELETE", &concept_path, None, Some(&owner_cookie)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _headers, _body) =
        send(&router, "GET", &concept_path, None, Some(&owner_cookie)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

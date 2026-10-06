//! Bringing people into a deployment, and letting a locked-out one back in.
//!
//! Two narratives over one deployment. In the first an operator invites a
//! colleague as a fellow operator, the link is resent, and the colleague opens
//! it, chooses a password, and is signed in with a verified address and the
//! role already held; around that, every way an invitation can be unusable is
//! tried and every one answers the same. In the second an account's owner is
//! locked out, an operator hands them a temporary password, and the account
//! can do nothing with it but choose its own.
//!
//! The operator routes are written here, the way an application writes them:
//! the framework provides the primitives and the guard, never the surface.
//!
//! Requires `DATABASE_URL`; without it the test logs a skip and passes. CI
//! always provides one.

mod support;

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use anubis::audit;
use anubis::auth::{CurrentUser, PASSWORD_CHANGE_REQUIRED};
use anubis::config::AppConfig;
use anubis::db::DbPool;
use anubis::guard::PlatformMember;
use anubis::http::{ApiError, ListParams};
use anubis::mail::{EmailKind, TestOutbox};
use anubis::platform::{self, Accounts, INVITATION_TTL_HOURS, InviteRequest};
use anubis::roles::{Action, RoleSet};
use anubis::schema::{audit_events, platform_invitations, sessions, users};
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{Duration, Utc};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde::Deserialize;
use serde_json::{Value, json};
use support::{PASSWORD, TestDatabase, register, send, session_token};
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

/// An operator may manage invitations and accounts; nobody else operates.
const ROLES_YML: &str = "
roles:
  default:
    models: {}
  admin:
    includes: [default]
    models: {}
  operator:
    scopes: [platform]
    models:
      Invitation: [manage]
      Account: [manage]
";

/// The seeded operator's password.
const OPERATOR_PASSWORD: &str = "the operator's own password";

/// The password the invitee chooses.
const INVITEE_PASSWORD: &str = "a password the invitee chose";

#[derive(Deserialize)]
struct InviteBody {
    email: String,
    platform_role: Option<String>,
}

async fn invite(
    operator: PlatformMember,
    context: audit::Context,
    Extension(accounts): Extension<Accounts>,
    Extension(pool): Extension<DbPool>,
    Json(body): Json<InviteBody>,
) -> Result<impl IntoResponse, ApiError> {
    operator.require(Action::Create, "Invitation")?;
    let mut connection = pool.get().await.map_err(|_error| ApiError::internal())?;
    let invitation = accounts
        .invite(
            &mut connection,
            &operator,
            &context,
            InviteRequest {
                email: &body.email,
                platform_role: body.platform_role.as_deref(),
            },
        )
        .await?;
    Ok((StatusCode::CREATED, Json(invitation)))
}

async fn list_invitations(
    operator: PlatformMember,
    Extension(pool): Extension<DbPool>,
    Query(params): Query<ListParams>,
) -> Result<impl IntoResponse, ApiError> {
    operator.require(Action::Read, "Invitation")?;
    let mut connection = pool.get().await.map_err(|_error| ApiError::internal())?;
    let page = platform::pending_invitations(&mut connection, &params).await?;
    let expired: Vec<bool> = page
        .invitations
        .iter()
        .map(platform::PlatformInvitation::is_expired)
        .collect();
    Ok(Json(json!({ "page": page, "expired": expired })))
}

async fn resend(
    operator: PlatformMember,
    context: audit::Context,
    Extension(accounts): Extension<Accounts>,
    Extension(pool): Extension<DbPool>,
    Path(invitation_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    operator.require(Action::Update, "Invitation")?;
    let mut connection = pool.get().await.map_err(|_error| ApiError::internal())?;
    let invitation = accounts
        .resend_invitation(&mut connection, &operator, &context, invitation_id)
        .await?;
    Ok(Json(invitation))
}

async fn revoke(
    operator: PlatformMember,
    context: audit::Context,
    Extension(accounts): Extension<Accounts>,
    Extension(pool): Extension<DbPool>,
    Path(invitation_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    operator.require(Action::Destroy, "Invitation")?;
    let mut connection = pool.get().await.map_err(|_error| ApiError::internal())?;
    let invitation = accounts
        .revoke_invitation(&mut connection, &operator, &context, invitation_id)
        .await?;
    Ok(Json(invitation))
}

async fn temporary_password(
    operator: PlatformMember,
    context: audit::Context,
    Extension(accounts): Extension<Accounts>,
    Extension(pool): Extension<DbPool>,
    Path(user_id): Path<Uuid>,
) -> Result<impl IntoResponse, ApiError> {
    operator.require(Action::Update, "Account")?;
    let mut connection = pool.get().await.map_err(|_error| ApiError::internal())?;
    let password = accounts
        .set_temporary_password(&mut connection, &operator, &context, user_id)
        .await?;
    Ok(Json(json!({ "password": password.reveal() })))
}

/// Any application route an ordinary signed-in account may use.
async fn dashboard(CurrentUser(user): CurrentUser) -> impl IntoResponse {
    Json(json!({ "email": user.email }))
}

/// A deployment: the auth routes, the operator routes, and one app route.
struct Deployment {
    router: Router,
    pool: DbPool,
    outbox: TestOutbox,
    operator: String,
}

impl Deployment {
    async fn boot(database: &TestDatabase) -> Self {
        let pool = database.pool().await;
        let roles = RoleSet::from_yaml(ROLES_YML).expect("roles must parse");
        let operator_email = format!("ops-{}@example.com", Uuid::new_v4());
        let seeded_email = operator_email.clone();
        let config = AppConfig::from_lookup(move |name| match name {
            "ANUBIS_ENV" => Some("test".to_owned()),
            "RATE_LIMIT_DISABLED" => Some("true".to_owned()),
            "ANUBIS_INITIAL_ADMIN_EMAIL" => Some(seeded_email.clone()),
            "ANUBIS_INITIAL_ADMIN_PASSWORD" => Some(OPERATOR_PASSWORD.to_owned()),
            _ => None,
        })
        .expect("the test config must parse");
        platform::ensure_initial_admin(&pool, &config, &roles)
            .await
            .expect("the seed must appoint the operator");

        let (mailer, outbox) = anubis::mail::Mailer::test();
        let rate_limit = anubis::rate_limit::RateLimiter::new(&config.rate_limit);
        let accounts = Accounts::new(&config, mailer.clone());

        let router = Router::new()
            .nest(
                "/auth",
                anubis::auth::router(pool.clone(), mailer, &config, &rate_limit),
            )
            .route("/operator/invitations", post(invite).get(list_invitations))
            .route("/operator/invitations/{invitation_id}/resend", post(resend))
            .route("/operator/invitations/{invitation_id}/revoke", post(revoke))
            .route(
                "/operator/accounts/{user_id}/temporary-password",
                post(temporary_password),
            )
            .route("/dashboard", get(dashboard))
            .layer(Extension(accounts))
            .layer(anubis::guard::layer(pool.clone(), roles));

        let (status, headers, body) = send(
            &router,
            "POST",
            "/auth/login",
            Some(&json!({ "email": operator_email, "password": OPERATOR_PASSWORD })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        let operator = session_token(&headers);

        Self {
            router,
            pool,
            outbox,
            operator,
        }
    }

    /// Sends `body` to an operator route as the operator.
    async fn operate(&self, path: &str, body: Option<&Value>) -> (StatusCode, Value) {
        let method = if path == "/operator/invitations" && body.is_none() {
            "GET"
        } else {
            "POST"
        };
        let (status, _headers, response) =
            send(&self.router, method, path, body, Some(&self.operator)).await;
        (status, response)
    }

    /// The token in the most recent invitation email to `email`.
    fn latest_token(&self, email: &str) -> String {
        let invitation = self
            .outbox
            .emails()
            .into_iter()
            .rev()
            .find(|sent| sent.to == email && sent.kind == EmailKind::PlatformInvitation)
            .expect("an invitation email must have been sent");
        let link = invitation
            .params
            .get("link")
            .expect("the invitation email must carry its link");
        link.split_once("token=")
            .expect("the link must carry a token")
            .1
            .to_owned()
    }

    /// The audit rows recorded about `subject_id` with `action`.
    async fn audited(&self, subject_id: Uuid, action: &str) -> Vec<audit::AuditEvent> {
        let mut connection = self.pool.get().await.expect("a connection");
        audit_events::table
            .filter(audit_events::subject_id.eq(subject_id))
            .filter(audit_events::action.eq(action))
            .select(audit::AuditEvent::as_select())
            .load(&mut connection)
            .await
            .expect("the audit log must be readable")
    }
}

async fn lookup(router: &Router, token: &str) -> (StatusCode, Value) {
    let (status, _headers, body) = send(
        router,
        "POST",
        "/auth/invitations/lookup",
        Some(&json!({ "token": token })),
        None,
    )
    .await;
    (status, body)
}

async fn accept(router: &Router, token: &str, password: &str) -> (StatusCode, Value, String) {
    let (status, headers, body) = send(
        router,
        "POST",
        "/auth/invitations/accept",
        Some(&json!({
            "token": token,
            "password": password,
            "time_zone": "America/Denver",
        })),
        None,
    )
    .await;
    let cookie = if status == StatusCode::CREATED {
        session_token(&headers)
    } else {
        String::new()
    };
    (status, body, cookie)
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn an_operator_invites_a_colleague_who_opens_an_account() {
    let Some(database) = TestDatabase::create("platform_invitations_flow").await else {
        return;
    };
    let deployment = Deployment::boot(&database).await;
    let router = &deployment.router;
    let invitee = format!("colleague-{}@example.com", Uuid::new_v4());

    // ---- Only an operator reaches the operator surface. -----------------
    let bystander = register(router, &format!("member-{}@example.com", Uuid::new_v4())).await;
    let (status, _headers, _body) = send(
        router,
        "POST",
        "/operator/invitations",
        Some(&json!({ "email": invitee })),
        Some(&bystander),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // ---- A role the platform cannot grant is refused. -------------------
    let (status, body) = deployment
        .operate(
            "/operator/invitations",
            Some(&json!({ "email": invitee, "platform_role": "admin" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    // ---- The invitation is written, audited, and mailed. ----------------
    let (status, invitation) = deployment
        .operate(
            "/operator/invitations",
            Some(&json!({ "email": invitee.to_uppercase(), "platform_role": "operator" })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {invitation}");
    assert_eq!(invitation["email"], invitee, "the address is normalized");
    assert_eq!(invitation["platform_role"], "operator");
    let invitation_id: Uuid = serde_json::from_value(invitation["id"].clone()).expect("an id");

    let sent = deployment
        .outbox
        .emails()
        .into_iter()
        .find(|sent| sent.to == invitee)
        .expect("the invitation must be mailed");
    assert_eq!(sent.kind, EmailKind::PlatformInvitation);
    assert_eq!(sent.params["hours"], INVITATION_TTL_HOURS.to_string());
    assert!(sent.params["link"].contains("/accept-invitation?token="));
    assert!(sent.params["inviter"].starts_with("ops-"));
    assert_eq!(
        deployment
            .audited(invitation_id, audit::INVITATION_CREATED)
            .await
            .len(),
        1,
    );

    // No account exists until the invitation is accepted.
    let mut connection = deployment.pool.get().await.expect("a connection");
    let accounts: i64 = users::table
        .filter(users::email.eq(&invitee))
        .count()
        .get_result(&mut connection)
        .await
        .expect("users must be readable");
    assert_eq!(accounts, 0, "an invitation is not an account");

    // ---- One live invitation per address, and none for an account. ------
    let (status, body) = deployment
        .operate("/operator/invitations", Some(&json!({ "email": invitee })))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Resend")
    );

    let registered = format!("registered-{}@example.com", Uuid::new_v4());
    register(router, &registered).await;
    let (status, _body) = deployment
        .operate(
            "/operator/invitations",
            Some(&json!({ "email": registered })),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "an account already holds it");

    // ---- The pending list carries it. -----------------------------------
    let (status, listing) = deployment.operate("/operator/invitations", None).await;
    assert_eq!(status, StatusCode::OK, "body: {listing}");
    let listed: Vec<&Value> = listing["page"]["invitations"]
        .as_array()
        .expect("a list")
        .iter()
        .filter(|row| row["id"] == invitation["id"])
        .collect();
    assert_eq!(listed.len(), 1, "the pending invitation is listed");
    assert!(
        listed[0].get("token_hash").is_none(),
        "the hash never leaves"
    );

    // ---- Reading the link names the address. ----------------------------
    let first_token = deployment.latest_token(&invitee);
    let (status, body) = lookup(router, &first_token).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["email"], invitee);

    // ---- A resend kills the old link and mails a new one. ---------------
    let (status, body) = deployment
        .operate(
            &format!("/operator/invitations/{invitation_id}/resend"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let second_token = deployment.latest_token(&invitee);
    assert_ne!(first_token, second_token);
    assert_eq!(
        deployment
            .audited(invitation_id, audit::INVITATION_RESENT)
            .await
            .len(),
        1,
    );

    let (unknown_status, unknown_body) = lookup(router, "not-a-token-anybody-issued").await;
    let (old_status, old_body) = lookup(router, &first_token).await;
    assert_eq!(unknown_status, StatusCode::BAD_REQUEST);
    assert_eq!(
        (old_status, &old_body),
        (unknown_status, &unknown_body),
        "a superseded link answers exactly as one nobody issued",
    );

    let (status, _body, _cookie) = accept(router, &first_token, INVITEE_PASSWORD).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the old link cannot accept"
    );

    // ---- The password policy registration enforces holds here. ----------
    let (status, body, _cookie) = accept(router, &second_token, "short").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    // ---- Accepting creates a verified account, signed in, role held. ----
    let (status, body, cookie) = accept(router, &second_token, INVITEE_PASSWORD).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["user"]["email"], invitee);
    assert_eq!(body["user"]["email_verified"], true, "the link proved it");
    assert_eq!(body["user"]["platform_roles"], json!(["operator"]));
    assert_eq!(body["user"]["time_zone"], "America/Denver");
    assert_eq!(body["user"]["password_change_required"], false);
    let new_user_id: Uuid = serde_json::from_value(body["user"]["id"].clone()).expect("an id");

    let (status, _headers, me) = send(router, "GET", "/auth/me", None, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "the session is live: {me}");

    let (status, _headers, body) = send(
        router,
        "POST",
        "/auth/login",
        Some(&json!({ "email": invitee, "password": INVITEE_PASSWORD })),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the chosen password signs in: {body}"
    );

    let signed_in = deployment
        .audited(new_user_id, audit::SESSION_CREATED)
        .await;
    assert!(
        signed_in
            .iter()
            .any(|event| event.changes["method"]["new"] == "invitation"),
        "the first session records how it began",
    );
    assert_eq!(
        deployment
            .audited(invitation_id, audit::INVITATION_CLAIMED)
            .await
            .len(),
        1,
    );
    assert_eq!(
        deployment
            .audited(new_user_id, platform::PLATFORM_ROLES_CHANGED)
            .await
            .len(),
        1,
        "the role arrived as an audited grant",
    );

    // ---- A used link is dead, and an accepted invitation stays closed. --
    let (status, body, _cookie) = accept(router, &second_token, INVITEE_PASSWORD).await;
    assert_eq!(
        (status, &body),
        (unknown_status, &unknown_body),
        "a used link answers exactly as one nobody issued",
    );
    let (status, _body) = deployment
        .operate(
            &format!("/operator/invitations/{invitation_id}/resend"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "accepted is final");

    let (_status, listing) = deployment.operate("/operator/invitations", None).await;
    assert!(
        listing["page"]["invitations"]
            .as_array()
            .expect("a list")
            .iter()
            .all(|row| row["id"] != invitation["id"]),
        "an accepted invitation is no longer pending",
    );

    // ---- An expired link answers like every other unusable one. ---------
    let late = format!("late-{}@example.com", Uuid::new_v4());
    let (status, late_invitation) = deployment
        .operate("/operator/invitations", Some(&json!({ "email": late })))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let late_id: Uuid = serde_json::from_value(late_invitation["id"].clone()).expect("an id");
    let late_token = deployment.latest_token(&late);

    diesel::update(platform_invitations::table.find(late_id))
        .set(
            platform_invitations::expires_at
                .eq(Utc::now() - Duration::hours(INVITATION_TTL_HOURS) - Duration::minutes(1)),
        )
        .execute(&mut connection)
        .await
        .expect("the invitation must be backdated");

    let (status, body) = lookup(router, &late_token).await;
    assert_eq!((status, &body), (unknown_status, &unknown_body));
    let (status, body, _cookie) = accept(router, &late_token, INVITEE_PASSWORD).await;
    assert_eq!((status, &body), (unknown_status, &unknown_body));

    let (_status, listing) = deployment.operate("/operator/invitations", None).await;
    let position = listing["page"]["invitations"]
        .as_array()
        .expect("a list")
        .iter()
        .position(|row| row["id"] == late_invitation["id"])
        .expect("an expired invitation is still pending");
    assert_eq!(listing["expired"][position], true);

    // A resend revives it with a fresh day.
    let (status, _body) = deployment
        .operate(&format!("/operator/invitations/{late_id}/resend"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _body) = lookup(router, &deployment.latest_token(&late)).await;
    assert_eq!(status, StatusCode::OK);

    // ---- Revoking closes the link and frees the address. ----------------
    let revived_token = deployment.latest_token(&late);
    let (status, _body) = deployment
        .operate(&format!("/operator/invitations/{late_id}/revoke"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = lookup(router, &revived_token).await;
    assert_eq!((status, &body), (unknown_status, &unknown_body));
    let (status, _body) = deployment
        .operate(&format!("/operator/invitations/{late_id}/resend"), None)
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a revoked invitation stays revoked"
    );
    assert_eq!(
        deployment
            .audited(late_id, audit::INVITATION_REVOKED)
            .await
            .len(),
        1,
    );
    let (status, _body) = deployment
        .operate("/operator/invitations", Some(&json!({ "email": late })))
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the address may be invited again"
    );

    // ---- An address registered meanwhile cannot be taken over. ----------
    let raced = format!("raced-{}@example.com", Uuid::new_v4());
    let (status, _body) = deployment
        .operate("/operator/invitations", Some(&json!({ "email": raced })))
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let raced_token = deployment.latest_token(&raced);
    register(router, &raced).await;
    let (status, body, _cookie) = accept(router, &raced_token, INVITEE_PASSWORD).await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");

    let (status, _headers, body) = send(
        router,
        "POST",
        "/auth/login",
        Some(&json!({ "email": raced, "password": PASSWORD })),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the registrant keeps their password: {body}"
    );
}

/// Collects everything the process logs while it is installed.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("log lock")).into_owned()
    }
}

impl Write for CapturedLogs {
    // `buf` is the trait's own name for it, which clippy holds an impl to.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("log lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> MakeWriter<'writer> for CapturedLogs {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one linear end-to-end narrative over shared database state"
)]
async fn a_temporary_password_admits_nothing_but_choosing_a_new_one() {
    let Some(database) = TestDatabase::create("temporary_password_flow").await else {
        return;
    };
    // Every log line the narrative produces, at every level, so the test can
    // say the password never reached one. A current-thread runtime keeps
    // every handler on this thread, where the default subscriber applies.
    let logs = CapturedLogs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(logs.clone())
        .finish();
    let _logging = tracing::subscriber::set_default(subscriber);

    let deployment = Deployment::boot(&database).await;
    let router = &deployment.router;
    let locked_out = format!("locked-out-{}@example.com", Uuid::new_v4());
    let stale_session = register(router, &locked_out).await;

    let mut connection = deployment.pool.get().await.expect("a connection");
    let user_id: Uuid = users::table
        .filter(users::email.eq(&locked_out))
        .select(users::id)
        .first(&mut connection)
        .await
        .expect("the account exists");

    // ---- An unknown account is a 404, and nobody but an operator acts. --
    let (status, _body) = deployment
        .operate(
            &format!("/operator/accounts/{}/temporary-password", Uuid::new_v4()),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _headers, _body) = send(
        router,
        "POST",
        &format!("/operator/accounts/{user_id}/temporary-password"),
        None,
        Some(&stale_session),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "an account cannot rescue itself"
    );

    // ---- The operator sets one and is handed it once. -------------------
    let (status, body) = deployment
        .operate(
            &format!("/operator/accounts/{user_id}/temporary-password"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let temporary = body["password"]
        .as_str()
        .expect("the password is handed over")
        .to_owned();

    // Every session the account had is gone.
    let (status, _headers, _body) =
        send(router, "GET", "/auth/me", None, Some(&stale_session)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let remaining: i64 = sessions::table
        .filter(sessions::user_id.eq(user_id))
        .count()
        .get_result(&mut connection)
        .await
        .expect("sessions must be readable");
    assert_eq!(remaining, 0);

    // The old password no longer signs in.
    let (status, _headers, _body) = send(
        router,
        "POST",
        "/auth/login",
        Some(&json!({ "email": locked_out, "password": PASSWORD })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // ---- Signing in with it works, and says what comes next. ------------
    let (status, headers, body) = send(
        router,
        "POST",
        "/auth/login",
        Some(&json!({ "email": locked_out, "password": temporary })),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["user"]["password_change_required"], true);
    let session = session_token(&headers);

    let (status, _headers, me) = send(router, "GET", "/auth/me", None, Some(&session)).await;
    assert_eq!(status, StatusCode::OK, "reading the account is allowed");
    assert_eq!(me["user"]["password_change_required"], true);

    // ---- Everything else answers 403 with the stable code. --------------
    for (method, path, payload) in [
        ("GET", "/dashboard", None),
        (
            "PATCH",
            "/auth/profile",
            Some(json!({ "first_name": "Locked" })),
        ),
        ("GET", "/auth/sessions", None),
    ] {
        let (status, _headers, body) =
            send(router, method, path, payload.as_ref(), Some(&session)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {body}");
        assert_eq!(body["code"], PASSWORD_CHANGE_REQUIRED, "{method} {path}");
    }

    // ---- The temporary password cannot become the permanent one. --------
    let (status, _headers, body) = send(
        router,
        "POST",
        "/auth/change-password",
        Some(&json!({ "current_password": temporary, "new_password": temporary })),
        Some(&session),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    // ---- Choosing a password clears the flag. ---------------------------
    let chosen = "a password of the owner's own";
    let (status, _headers, body) = send(
        router,
        "POST",
        "/auth/change-password",
        Some(&json!({ "current_password": temporary, "new_password": chosen })),
        Some(&session),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let (status, _headers, body) = send(router, "GET", "/dashboard", None, Some(&session)).await;
    assert_eq!(status, StatusCode::OK, "the account is free again: {body}");
    let (_status, _headers, me) = send(router, "GET", "/auth/me", None, Some(&session)).await;
    assert_eq!(me["user"]["password_change_required"], false);

    // ---- The audit log says who did it, and never what it was. ----------
    let recorded = deployment
        .audited(user_id, audit::PASSWORD_TEMPORARY_SET)
        .await;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].changes, json!({}));

    let every_row: Vec<audit::AuditEvent> = audit_events::table
        .select(audit::AuditEvent::as_select())
        .load(&mut connection)
        .await
        .expect("the audit log must be readable");
    let rendered = serde_json::to_string(&every_row).expect("the log serializes");
    assert!(!rendered.contains(&temporary), "the audit log holds it");

    let logged = logs.text();
    assert!(!logged.is_empty(), "the capture must be listening");
    assert!(!logged.contains(&temporary), "a log line holds it");
}

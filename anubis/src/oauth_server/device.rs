//! The device authorization grant (RFC 8628), for programs without a browser.
//!
//! A program on a headless machine cannot receive a redirect, so it asks for
//! two codes instead: a long device code it keeps, and a short user code it
//! shows. The person opens the verification page on any browser, types the
//! user code, and approves on the same consent screen every other client
//! uses. The program meanwhile polls the token endpoint with its device code,
//! and receives tokens once the person approved.
//!
//! | Route | Who calls it | Effect |
//! |---|---|---|
//! | `POST /oauth/device_authorization` | the program | Issue a device code and a user code |
//! | `POST /oauth/device_verification` | the code-entry page | Find the request a user code names (session) |
//! | `POST /oauth/token`, `grant_type=urn:ietf:params:oauth:grant-type:device_code` | the program | Poll, and exchange once approved |
//!
//! # One request, decided once
//!
//! Issuing the codes also stores an authorization request, the same row the
//! consent screen reads for a redirecting client, linked to the device code.
//! Entering the user code answers that request's id, and the consent
//! screen's decision consumes it, exactly as it does for any request: two
//! browsers entering one code see one request, and the second decision finds
//! it gone. The decision lands on the device code as a grant or a refusal,
//! and the program's next poll reads it.
//!
//! # Polling
//!
//! Every poll writes: its time, the grown interval when it came too soon,
//! the spend when it exchanges. So a poll locks the device code's row and
//! decides from what it reads there ([`DeviceCode::poll`]), and polls of one
//! code take turns. A device code is exchanged once. Presented again it is
//! held twice, which revokes the grant ([`audit::OAUTH_DEVICE_CODE_REUSED`])
//! just as a replayed authorization code does, so two polls racing after an
//! approval mint one token set and the loser's arrival ends it. A program
//! that polls one request at a time, as RFC 8628 has it do, never meets that.
//!
//! # What makes a user code safe to type
//!
//! Eight letters from RFC 8628's twenty consonants, about 34 bits, hashed at
//! rest like every other code. That is guessable offline from a leaked
//! database, and worth nothing there: a code lives ten minutes and only lets
//! its finder approve somebody else's device into their own account. Online,
//! entry needs a session and is charged per client address and per account
//! ([`Budget::UserCodePerClient`] and [`Budget::UserCodePerAccount`]), and at
//! most [`MAX_LIVE_DEVICE_CODES`] codes wait at once, which bounds both what
//! strangers can store and the odds of a guess naming any of them.
//!
//! [`Budget::UserCodePerClient`]: crate::rate_limit::Budget::UserCodePerClient
//! [`Budget::UserCodePerAccount`]: crate::rate_limit::Budget::UserCodePerAccount

use axum::extract::rejection::FormRejection;
use axum::extract::{Form, State};
use axum::http::header::CACHE_CONTROL;
use axum::response::IntoResponse;
use axum::{Extension, Json};
use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::audit;
use crate::auth::{CurrentUser, token};
use crate::http::ApiError;
use crate::oauth_server::Server;
use crate::oauth_server::client::{self, ClientFailure, ClientRow, GrantType};
use crate::oauth_server::error::OauthError;
use crate::oauth_server::grant;
use crate::oauth_server::token::{Outcome, TokenForm};
use crate::rate_limit::RateLimiter;
use crate::schema::{oauth_authorization_requests, oauth_clients, oauth_device_codes};

/// Where the SPA renders the code-entry page, RFC 8628's verification URI.
///
/// A contract with the application's frontend, the way [`super::CONSENT_PATH`]
/// is: the starter's `UrlTree.device` names the same path. The page reads a
/// prefilled code from [`USER_CODE_PARAM`] and the outcome of a decision from
/// [`DEVICE_RESULT_PARAM`].
pub const DEVICE_PATH: &str = "/device";

/// The query parameter `verification_uri_complete` carries the user code in.
pub const USER_CODE_PARAM: &str = "user_code";

/// The query parameter a decided request returns to the code-entry page with,
/// holding `approved` or `denied`.
pub const DEVICE_RESULT_PARAM: &str = "result";

/// How long a device authorization waits for the person, which is also how
/// long both codes live.
///
/// Ten minutes, the same as a redirecting client's request: long enough to
/// reach another device, sign in with a second factor, and type eight letters,
/// and short enough that a code read over somebody's shoulder is soon stale.
const DEVICE_CODE_TTL: Duration = Duration::minutes(10);

/// How many seconds a program waits between polls, until it polls too soon.
///
/// RFC 8628 section 3.2's default, which is what a program assumes anyway.
const POLL_INTERVAL_SECONDS: i32 = 5;

/// How many seconds each too-soon poll adds to the interval, which RFC 8628
/// section 3.5 fixes for `slow_down`.
const SLOW_DOWN_SECONDS: i32 = 5;

/// The letters a user code is drawn from: RFC 8628 section 6.1's twenty
/// consonants, with no vowel to spell a word and no digit to mistake for a
/// letter.
const USER_CODE_ALPHABET: &[u8; 20] = b"BCDFGHJKLMNPQRSTVWXZ";

/// A user code reads as two groups of four, `WDJB-MJHT`, as RFC 8628 shows.
const USER_CODE_GROUPS: usize = 2;
const USER_CODE_GROUP_LENGTH: usize = 4;

/// How many device authorizations may wait at once before the endpoint
/// refuses with `503`.
///
/// The endpoint is open to strangers and every call stores two rows, so this
/// bounds what a flood from rotating addresses can store for the ten minutes
/// they live. It also bounds the odds of a guessed user code naming any live
/// one: five thousand of 20^8 codes is one in five million per guess. Real
/// use is a person connecting a device, finished within minutes.
pub(crate) const MAX_LIVE_DEVICE_CODES: i64 = 5_000;

/// How many user codes are drawn before giving up on finding an unused one.
///
/// A collision needs one of at most [`MAX_LIVE_DEVICE_CODES`] live codes out
/// of 20^8, so a second draw is already a one-in-five-million event.
const USER_CODE_DRAWS: usize = 3;

/// A device authorization request, RFC 8628 section 3.1.
///
/// Every field is optional so a missing one is answered as `invalid_request`
/// in OAuth's shape rather than as a framework rejection.
#[derive(Debug, Deserialize)]
pub(crate) struct DeviceAuthorizationForm {
    client_id: Option<String>,
    scope: Option<String>,
    resource: Option<String>,
}

/// The device authorization response, RFC 8628 section 3.2.
#[derive(Debug, Serialize)]
struct DeviceAuthorizationResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: String,
    expires_in: i64,
    interval: i32,
}

#[derive(Insertable)]
#[diesel(table_name = oauth_device_codes)]
struct NewDeviceCode<'a> {
    client_id: Uuid,
    device_code_hash: &'a str,
    user_code_hash: &'a str,
    interval_seconds: i32,
    expires_at: DateTime<Utc>,
}

/// `POST /oauth/device_authorization`.
pub(crate) async fn authorize(
    State(server): State<Server>,
    form: Result<Form<DeviceAuthorizationForm>, FormRejection>,
) -> Result<impl IntoResponse, OauthError> {
    let Form(form) = form.map_err(|rejection| {
        OauthError::invalid_request(format!(
            "The device authorization request must be a form-encoded body: {rejection}"
        ))
    })?;
    // RFC 8628 section 3.1: a parameter sent without a value was not sent.
    let client_id = form
        .client_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OauthError::invalid_request("client_id is required."))?;
    if form
        .resource
        .as_deref()
        .filter(|value| !value.is_empty())
        .is_some_and(|resource| !server.is_resource(resource))
    {
        return Err(OauthError::invalid_target(
            "The resource is not one this server issues tokens for.",
        ));
    }
    let scopes = server
        .scopes()
        .parse_request(form.scope.as_deref())
        .map_err(|unknown| {
            OauthError::invalid_scope(format!("{unknown} is not a scope this server grants."))
        })?;

    let mut connection = server.pool().get().await.map_err(|error| {
        tracing::error!(
            error.message = %error,
            "device authorization could not reach the database: {{error.message}}",
        );
        OauthError::server_error()
    })?;

    let client = client::resolve(&server, &mut connection, client_id)
        .await
        .map_err(|failure| match failure {
            ClientFailure::Unknown => {
                OauthError::invalid_client("The client is not one this server recognizes.")
            }
            ClientFailure::Internal => OauthError::server_error(),
        })?;
    if !client.allows(GrantType::DeviceCode) {
        return Err(OauthError::unauthorized_client(
            "This client did not declare the device code grant.",
        ));
    }

    let issued = connection
        .transaction::<_, OauthError, _>(async |transaction| {
            issue(transaction, &client, &scopes, server.resource()).await
        })
        .await?;

    let verification_uri = format!("{}{DEVICE_PATH}", server.issuer());
    let verification_uri_complete =
        format!("{verification_uri}?{USER_CODE_PARAM}={}", issued.user_code);
    Ok((
        [(CACHE_CONTROL, "no-store")],
        Json(DeviceAuthorizationResponse {
            device_code: issued.device_code,
            user_code: issued.user_code,
            verification_uri,
            verification_uri_complete,
            expires_in: DEVICE_CODE_TTL.num_seconds(),
            interval: POLL_INTERVAL_SECONDS,
        }),
    ))
}

/// The two codes a program is handed.
struct IssuedCodes {
    device_code: String,
    user_code: String,
}

/// Stores a device authorization and the request the person will decide.
///
/// Sweeps the device authorizations that expired first, which takes their
/// requests with them, and refuses while [`MAX_LIVE_DEVICE_CODES`] still wait.
async fn issue(
    connection: &mut AsyncPgConnection,
    client: &ClientRow,
    scopes: &[String],
    resource: &str,
) -> Result<IssuedCodes, OauthError> {
    let now = Utc::now();
    diesel::delete(oauth_device_codes::table.filter(oauth_device_codes::expires_at.le(now)))
        .execute(connection)
        .await?;
    let live: i64 = oauth_device_codes::table
        .count()
        .get_result(connection)
        .await?;
    if live >= MAX_LIVE_DEVICE_CODES {
        tracing::warn!(
            oauth.live_device_codes = live,
            "device authorization refused: {{oauth.live_device_codes}} are waiting",
        );
        return Err(OauthError::temporarily_unavailable(
            "Device sign-in is busy. Try again in a few minutes.",
        ));
    }

    let device_code = token::generate();
    let device_code_hash = token::hash(&device_code);
    let expires_at = now + DEVICE_CODE_TTL;
    for _draw in 0..USER_CODE_DRAWS {
        let user_code =
            token::readable(USER_CODE_ALPHABET, USER_CODE_GROUPS, USER_CODE_GROUP_LENGTH);
        let user_code_hash = token::hash(
            &normalize_user_code(&user_code).expect("an issued user code is a user code"),
        );

        // A user code already waiting is a collision, drawn again rather than
        // failing the transaction on the unique index.
        let stored: Option<Uuid> = diesel::insert_into(oauth_device_codes::table)
            .values(NewDeviceCode {
                client_id: client.id,
                device_code_hash: &device_code_hash,
                user_code_hash: &user_code_hash,
                interval_seconds: POLL_INTERVAL_SECONDS,
                expires_at,
            })
            .on_conflict_do_nothing()
            .returning(oauth_device_codes::id)
            .get_result(connection)
            .await
            .optional()?;
        let Some(device_code_id) = stored else {
            continue;
        };

        diesel::insert_into(oauth_authorization_requests::table)
            .values((
                oauth_authorization_requests::client_id.eq(client.id),
                oauth_authorization_requests::scopes.eq(scopes),
                oauth_authorization_requests::resource.eq(resource),
                oauth_authorization_requests::expires_at.eq(expires_at),
                oauth_authorization_requests::device_code_id.eq(device_code_id),
            ))
            .execute(connection)
            .await?;
        return Ok(IssuedCodes {
            device_code,
            user_code: user_code.to_string(),
        });
    }

    tracing::error!(
        oauth.draws = USER_CODE_DRAWS,
        "no unused user code was found in {{oauth.draws}} draws",
    );
    Err(OauthError::server_error())
}

/// The user code a person entered, in the form its hash is taken of, or
/// nothing when it cannot be one.
///
/// RFC 8628 section 6.1: compared without regard to case, and without the
/// dash or any spacing a person copies along with it.
fn normalize_user_code(entered: &str) -> Option<String> {
    let normalized: String = entered
        .chars()
        .filter(|character| *character != '-' && !character.is_whitespace())
        .map(|character| character.to_ascii_uppercase())
        .collect();
    let is_user_code = normalized.len() == USER_CODE_GROUPS * USER_CODE_GROUP_LENGTH
        && normalized
            .bytes()
            .all(|byte| USER_CODE_ALPHABET.contains(&byte));
    is_user_code.then_some(normalized)
}

/// What the code-entry page sends.
#[derive(Debug, Deserialize)]
pub(crate) struct VerificationBody {
    user_code: String,
}

/// The request the person goes on to decide on the consent screen.
#[derive(Debug, Serialize)]
struct VerificationResponse {
    request_id: Uuid,
}

/// `POST /oauth/device_verification`: the request a user code names.
///
/// Signed in, so the decision that follows binds to this account. Charged per
/// account before anything is looked up, beside the per-address budget the
/// route's layer charges. Every code that names nothing waiting, whether
/// malformed, unknown, expired, or already decided, answers the same `400`,
/// so the answer says nothing about which.
pub(crate) async fn verify(
    State(server): State<Server>,
    Extension(rate_limit): Extension<RateLimiter>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<VerificationBody>,
) -> Result<impl IntoResponse, ApiError> {
    rate_limit.check_user_code_entry(user.id)?;

    let unusable =
        || ApiError::validation("That code is not valid. Check it against your device's screen.");
    let Some(user_code) = normalize_user_code(&body.user_code) else {
        return Err(unusable());
    };

    let mut connection = server.pool().get().await.map_err(|error| {
        tracing::error!(
            error.message = %error,
            "code entry could not reach the database: {{error.message}}",
        );
        ApiError::internal()
    })?;
    let request_id: Option<Uuid> = oauth_authorization_requests::table
        .inner_join(oauth_device_codes::table)
        .filter(oauth_device_codes::user_code_hash.eq(token::hash(&user_code)))
        .filter(oauth_authorization_requests::expires_at.gt(Utc::now()))
        .select(oauth_authorization_requests::id)
        .first(&mut connection)
        .await
        .optional()?;

    let request_id = request_id.ok_or_else(unusable)?;
    Ok(Json(VerificationResponse { request_id }))
}

/// What the person decided about a device on the consent screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Approved, creating the grant this names.
    Approve(Uuid),
    /// Refused.
    Deny,
}

/// Lands the person's decision on the device authorization `device_code_id`.
///
/// One conditional update, which only an undecided, unexpired authorization
/// takes, so a decision racing another decision or the clock either wins or
/// answers `404`, rolling back with it the grant an approval was about to hand
/// over. Answers where the browser goes: the code-entry page, saying how it
/// went.
pub(crate) async fn decide(
    connection: &mut AsyncPgConnection,
    server: &Server,
    device_code_id: Uuid,
    decision: Decision,
) -> Result<String, ApiError> {
    let now = Utc::now();
    let undecided = oauth_device_codes::table
        .filter(oauth_device_codes::id.eq(device_code_id))
        .filter(oauth_device_codes::grant_id.is_null())
        .filter(oauth_device_codes::denied_at.is_null())
        .filter(oauth_device_codes::expires_at.gt(now));
    let (decided, result) = match decision {
        Decision::Approve(grant_id) => (
            diesel::update(undecided)
                .set(oauth_device_codes::grant_id.eq(Some(grant_id)))
                .execute(connection)
                .await?,
            "approved",
        ),
        Decision::Deny => (
            diesel::update(undecided)
                .set(oauth_device_codes::denied_at.eq(Some(now)))
                .execute(connection)
                .await?,
            "denied",
        ),
    };
    if decided == 0 {
        return Err(ApiError::not_found());
    }

    Ok(format!(
        "{}{DEVICE_PATH}?{DEVICE_RESULT_PARAM}={result}",
        server.issuer()
    ))
}

/// One device authorization, as a poll reads it under its row lock.
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = oauth_device_codes)]
#[diesel(check_for_backend(diesel::pg::Pg))]
struct DeviceCode {
    id: Uuid,
    client_id: Uuid,
    interval_seconds: i32,
    last_polled_at: Option<DateTime<Utc>>,
    expires_at: DateTime<Utc>,
    grant_id: Option<Uuid>,
    denied_at: Option<DateTime<Utc>>,
    used_at: Option<DateTime<Utc>>,
}

/// What one poll answers, decided from the device authorization alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Poll {
    /// Approved and not exchanged: spend it, and issue the grant's tokens.
    Exchange(Uuid),
    /// Exchanged already, so held twice: revoke the grant.
    Reused(Uuid),
    /// The person refused it.
    Denied,
    /// It lapsed before it was exchanged.
    Expired,
    /// Undecided, polled no sooner than the interval allows.
    Pending,
    /// Undecided, polled too soon: the interval grows.
    TooSoon,
}

impl DeviceCode {
    /// Decides what a poll arriving at `now` answers.
    ///
    /// The order is the policy. A spent code is a theft signal whatever else
    /// is true of it, as a replayed authorization code is. A refusal is the
    /// person's word and outranks the clock. An approval does not extend the
    /// code's life. Only a code still waiting on the person is held to the
    /// polling interval.
    fn poll(&self, now: DateTime<Utc>) -> Poll {
        if let Some(grant_id) = self.grant_id
            && self.used_at.is_some()
        {
            return Poll::Reused(grant_id);
        }
        if self.denied_at.is_some() {
            return Poll::Denied;
        }
        if self.expires_at <= now {
            return Poll::Expired;
        }
        if let Some(grant_id) = self.grant_id {
            return Poll::Exchange(grant_id);
        }

        let interval = Duration::seconds(i64::from(self.interval_seconds));
        let too_soon = self
            .last_polled_at
            .is_some_and(|last| now < last + interval);
        if too_soon {
            return Poll::TooSoon;
        }
        Poll::Pending
    }
}

/// The `urn:ietf:params:oauth:grant-type:device_code` grant: one poll.
pub(crate) async fn exchange(
    connection: &mut AsyncPgConnection,
    context: &audit::Context,
    client_id: &str,
    form: &TokenForm,
) -> QueryResult<Outcome> {
    let Some(presented) = form.device_code.as_deref() else {
        return Ok(Err(OauthError::invalid_request("device_code is required.")));
    };
    let unknown = || OauthError::invalid_grant("The device code is invalid or was already used.");

    // Polls of one code take turns, because each one writes; see the module
    // docs. Only this row is locked: a join would lock the client's row too,
    // and every person connecting through one client would wait on it.
    let found: Option<DeviceCode> = oauth_device_codes::table
        .filter(oauth_device_codes::device_code_hash.eq(token::hash(presented)))
        .select(DeviceCode::as_select())
        .for_update()
        .first(connection)
        .await
        .optional()?;
    let Some(device) = found else {
        return Ok(Err(unknown()));
    };
    // Another client naming this code changes nothing and learns nothing.
    let owner: String = oauth_clients::table
        .find(device.client_id)
        .select(oauth_clients::client_id)
        .first(connection)
        .await?;
    if owner != client_id {
        return Ok(Err(unknown()));
    }

    let now = Utc::now();
    let (interval_seconds, refusal) = match device.poll(now) {
        Poll::Exchange(grant_id) => {
            let Some((granted, _client)) = grant::live_with_client(connection, grant_id).await?
            else {
                return Ok(Err(unknown()));
            };
            diesel::update(oauth_device_codes::table.find(device.id))
                .set((
                    oauth_device_codes::used_at.eq(Some(now)),
                    oauth_device_codes::last_polled_at.eq(Some(now)),
                ))
                .execute(connection)
                .await?;
            let scopes = granted.scopes.clone();
            return Ok(Ok(grant::issue_tokens(connection, &granted, scopes).await?));
        }
        Poll::Reused(grant_id) => {
            grant::revoke_for_reuse(
                connection,
                context,
                grant_id,
                audit::OAUTH_DEVICE_CODE_REUSED,
            )
            .await?;
            return Ok(Err(unknown()));
        }
        Poll::Denied => return Ok(Err(OauthError::access_denied())),
        Poll::Expired => return Ok(Err(OauthError::expired_token())),
        Poll::Pending => (device.interval_seconds, OauthError::authorization_pending()),
        Poll::TooSoon => (
            device.interval_seconds.saturating_add(SLOW_DOWN_SECONDS),
            OauthError::slow_down(),
        ),
    };

    diesel::update(oauth_device_codes::table.find(device.id))
        .set((
            oauth_device_codes::last_polled_at.eq(Some(now)),
            oauth_device_codes::interval_seconds.eq(interval_seconds),
        ))
        .execute(connection)
        .await?;
    Ok(Err(refusal))
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use uuid::Uuid;

    use super::{
        DeviceCode, POLL_INTERVAL_SECONDS, Poll, USER_CODE_ALPHABET, USER_CODE_GROUP_LENGTH,
        USER_CODE_GROUPS, normalize_user_code,
    };
    use crate::auth::token;

    /// A device authorization nobody has decided or polled, issued just now.
    fn waiting() -> DeviceCode {
        DeviceCode {
            id: Uuid::new_v4(),
            client_id: Uuid::new_v4(),
            interval_seconds: POLL_INTERVAL_SECONDS,
            last_polled_at: None,
            expires_at: Utc::now() + Duration::minutes(10),
            grant_id: None,
            denied_at: None,
            used_at: None,
        }
    }

    #[test]
    fn an_issued_user_code_reads_as_two_groups_and_normalizes_to_itself() {
        let issued = token::readable(USER_CODE_ALPHABET, USER_CODE_GROUPS, USER_CODE_GROUP_LENGTH);

        assert_eq!(issued.len(), 9);
        assert_eq!(issued.as_bytes()[4], b'-');
        assert_eq!(
            normalize_user_code(&issued).as_deref(),
            Some(issued.replace('-', "").as_str()),
        );
    }

    #[test]
    fn a_user_code_is_compared_without_case_dashes_or_spacing() {
        for entered in [
            "WDJB-MJHT",
            "wdjb-mjht",
            "WDJBMJHT",
            " wdjb mjht ",
            "Wdjb-\tMjht",
        ] {
            assert_eq!(
                normalize_user_code(entered).as_deref(),
                Some("WDJBMJHT"),
                "{entered:?}",
            );
        }
    }

    #[test]
    fn what_cannot_be_a_user_code_is_nothing() {
        // A vowel, a digit, a letter short, a letter long, and an accent.
        for entered in [
            "WDJA-MJHT",
            "WDJB-MJH7",
            "WDJB-MJH",
            "WDJB-MJHTX",
            "",
            "WDJB-MJHÉ",
        ] {
            assert_eq!(normalize_user_code(entered), None, "{entered:?}");
        }
    }

    #[test]
    fn an_undecided_code_waits_and_slows_a_program_that_polls_too_soon() {
        let now = Utc::now();
        let mut device = waiting();
        assert_eq!(
            device.poll(now),
            Poll::Pending,
            "the first poll is never too soon"
        );

        device.last_polled_at = Some(now - Duration::seconds(4));
        assert_eq!(device.poll(now), Poll::TooSoon);

        device.last_polled_at = Some(now - Duration::seconds(5));
        assert_eq!(
            device.poll(now),
            Poll::Pending,
            "the interval itself is soon enough"
        );

        // A grown interval is the one held to.
        device.interval_seconds = 10;
        device.last_polled_at = Some(now - Duration::seconds(9));
        assert_eq!(device.poll(now), Poll::TooSoon);
    }

    #[test]
    fn an_approved_code_exchanges_once_and_is_reused_after() {
        let now = Utc::now();
        let grant_id = Uuid::new_v4();
        let mut device = waiting();
        device.grant_id = Some(grant_id);
        // Polling too soon is beside the point once the person approved.
        device.last_polled_at = Some(now);
        assert_eq!(device.poll(now), Poll::Exchange(grant_id));

        device.used_at = Some(now);
        assert_eq!(device.poll(now), Poll::Reused(grant_id));
    }

    #[test]
    fn a_spent_code_is_reuse_even_once_it_expired() {
        let now = Utc::now();
        let grant_id = Uuid::new_v4();
        let mut device = waiting();
        device.grant_id = Some(grant_id);
        device.used_at = Some(now - Duration::minutes(11));
        device.expires_at = now - Duration::minutes(1);

        assert_eq!(device.poll(now), Poll::Reused(grant_id));
    }

    #[test]
    fn a_refusal_outranks_the_clock_and_the_clock_outranks_an_approval() {
        let now = Utc::now();
        let mut denied = waiting();
        denied.denied_at = Some(now - Duration::minutes(1));
        denied.expires_at = now - Duration::seconds(1);
        assert_eq!(denied.poll(now), Poll::Denied);

        let mut approved_late = waiting();
        approved_late.grant_id = Some(Uuid::new_v4());
        approved_late.expires_at = now;
        assert_eq!(approved_late.poll(now), Poll::Expired);

        let mut undecided = waiting();
        undecided.expires_at = now - Duration::seconds(1);
        assert_eq!(undecided.poll(now), Poll::Expired);
    }
}

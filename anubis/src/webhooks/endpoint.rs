//! Webhook endpoints: what a team subscribed, and the secret it signs with.
//!
//! An endpoint is one team's standing request to be told about events. It
//! carries the URL to POST to, the event types it wants, whether it is live,
//! and the secret every delivery to it is signed with.
//!
//! The secret is **encrypted**, not hashed, which is the one place the
//! framework's token discipline does not apply. Signing a request body means
//! recomputing an HMAC over it at send time, and a one-way hash cannot do
//! that. So it is sealed with [`crate::auth::secret_box`] under
//! `ANUBIS_SECRET_KEY`: a stolen database yields nothing without the key, and
//! the plaintext is shown to the team exactly once, when the endpoint is
//! created.

use chrono::{DateTime, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use url::Url;
use uuid::Uuid;

use crate::auth::secret_box::{self, SecretKey};
use crate::schema::webhook_endpoints;

/// Prefixes every generated signing secret.
///
/// Purely a courtesy to whoever finds one in a log or a config file: the
/// prefix says what the value is and where it came from. It is part of the
/// secret and is signed over like the rest of it.
const SECRET_PREFIX: &str = "whsec_";

/// A team's subscription to a set of event types.
///
/// The signing secret is deliberately absent: this struct is what every
/// endpoint of the management API serializes, and the secret leaves the server
/// exactly once, in the create response.
#[derive(Debug, Clone, Serialize, Queryable, Selectable)]
#[diesel(table_name = webhook_endpoints)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct WebhookEndpoint {
    /// Primary key, and the id deliveries hang off.
    pub id: Uuid,
    /// The team that owns the subscription and sees its deliveries.
    pub team_id: Uuid,
    /// Where deliveries are sent.
    pub url: String,
    /// What the team called this subscription.
    pub description: Option<String>,
    /// The event types this endpoint receives. An empty list receives nothing.
    pub event_types: Vec<String>,
    /// A paused endpoint keeps its history and receives nothing new.
    pub active: bool,
    /// When the endpoint was created.
    pub created_at: DateTime<Utc>,
    /// When the endpoint was last updated.
    pub updated_at: DateTime<Utc>,
}

#[derive(Insertable)]
#[diesel(table_name = webhook_endpoints)]
struct NewWebhookEndpoint<'a> {
    team_id: Uuid,
    url: &'a str,
    description: Option<&'a str>,
    event_types: &'a [String],
    secret: &'a str,
}

/// The fields an update may change, each absent meaning "leave it alone".
#[expect(
    clippy::option_option,
    reason = "Diesel's changeset shape for a nullable column"
)]
#[derive(Debug, Default, AsChangeset)]
#[diesel(table_name = webhook_endpoints)]
pub(super) struct WebhookEndpointChanges {
    pub url: Option<String>,
    pub description: Option<Option<String>>,
    pub event_types: Option<Vec<String>>,
    pub active: Option<bool>,
}

impl WebhookEndpointChanges {
    /// Returns `true` when nothing was submitted, which Diesel refuses to
    /// update.
    pub(super) fn is_empty(&self) -> bool {
        self.url.is_none()
            && self.description.is_none()
            && self.event_types.is_none()
            && self.active.is_none()
    }
}

/// Creates an endpoint and mints its signing secret.
///
/// Returns the endpoint and the raw secret; the secret is not recoverable
/// afterwards, because only its sealed form is stored.
///
/// # Errors
/// Returns the underlying query error when the insert fails.
pub(super) async fn create(
    connection: &mut AsyncPgConnection,
    key: &SecretKey,
    team_id: Uuid,
    url: &str,
    description: Option<&str>,
    event_types: &[String],
) -> QueryResult<(WebhookEndpoint, String)> {
    let raw_secret = format!("{SECRET_PREFIX}{}", crate::auth::token::generate());
    let sealed = secret_box::encrypt(key, &raw_secret);

    let endpoint = diesel::insert_into(webhook_endpoints::table)
        .values(NewWebhookEndpoint {
            team_id,
            url,
            description,
            event_types,
            secret: &sealed,
        })
        .returning(WebhookEndpoint::as_returning())
        .get_result(connection)
        .await?;

    Ok((endpoint, raw_secret))
}

/// Applies a submitted change to one endpoint, returning it as it now stands.
///
/// # Errors
/// Returns the underlying query error when the update fails.
pub(super) async fn update(
    connection: &mut AsyncPgConnection,
    endpoint: WebhookEndpoint,
    changes: WebhookEndpointChanges,
) -> QueryResult<WebhookEndpoint> {
    if changes.is_empty() {
        return Ok(endpoint);
    }

    diesel::update(webhook_endpoints::table.find(endpoint.id))
        .set(changes)
        .returning(WebhookEndpoint::as_returning())
        .get_result(connection)
        .await
}

/// Loads one of a team's endpoints, or `None` when it belongs to another team.
///
/// Scoping the lookup by team is what makes a cross-tenant id answer `404`
/// rather than leak that the endpoint exists.
///
/// # Errors
/// Returns the underlying query error when the lookup fails.
pub(super) async fn load_for_team(
    connection: &mut AsyncPgConnection,
    team_id: Uuid,
    endpoint_id: Uuid,
) -> QueryResult<Option<WebhookEndpoint>> {
    webhook_endpoints::table
        .filter(webhook_endpoints::id.eq(endpoint_id))
        .filter(webhook_endpoints::team_id.eq(team_id))
        .select(WebhookEndpoint::as_select())
        .first(connection)
        .await
        .optional()
}

/// Opens one endpoint's signing secret.
///
/// # Errors
/// Returns the underlying query error when the lookup fails. A secret that no
/// longer decrypts, because `ANUBIS_SECRET_KEY` rotated, comes back as `None`:
/// nothing can be signed for that endpoint until it is recreated, which is
/// safer than sending an unsigned or wrongly signed request.
pub(super) async fn signing_secret(
    connection: &mut AsyncPgConnection,
    key: &SecretKey,
    endpoint_id: Uuid,
) -> QueryResult<Option<String>> {
    let sealed: Option<String> = webhook_endpoints::table
        .find(endpoint_id)
        .select(webhook_endpoints::secret)
        .first(connection)
        .await
        .optional()?;

    Ok(
        sealed.and_then(|sealed| match secret_box::decrypt(key, &sealed) {
            Ok(secret) => Some(secret),
            Err(error) => {
                tracing::error!(
                    webhook.endpoint.id = %endpoint_id,
                    error.message = %error,
                    "the signing secret could not be opened: {{error.message}}",
                );
                None
            }
        }),
    )
}

/// Rejects a URL an endpoint may not point at.
///
/// Deliveries are requests this server makes on a user's instruction, which is
/// server-side request forgery by construction. Two rules bound it:
///
/// 1. **HTTPS only**, so a secret-signed payload is not readable in transit.
///    Plain `http://` is accepted outside production only, because a developer
///    testing against `localhost` has no certificate and no attacker.
/// 2. **No credentials in the URL**, which would otherwise be sent to whatever
///    the host resolves to.
///
/// What this does **not** do is stop a hostname that resolves to a private
/// address, or one that resolves differently on the second lookup than on the
/// first (DNS rebinding). Closing that needs resolution and connection to
/// happen under one policy, which is on the roadmap; `docs/webhooks.md` states
/// the posture.
///
/// # Errors
/// Returns a user-safe message naming the rule the URL broke.
pub(super) fn validate_url(url: &str, allow_insecure: bool) -> Result<String, &'static str> {
    let parsed = Url::parse(url.trim()).map_err(|_error| "Enter a valid URL.")?;

    match parsed.scheme() {
        "https" => {}
        "http" if allow_insecure => {}
        "http" => return Err("Webhook endpoints must use https in production."),
        _ => return Err("Webhook endpoints must use https."),
    }

    if parsed.host_str().is_none() {
        return Err("Enter a URL with a host name.");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Leave credentials out of the webhook URL.");
    }

    Ok(parsed.to_string())
}

#[cfg(test)]
mod tests {
    use super::validate_url;

    #[test]
    fn https_urls_are_accepted_and_normalized() {
        assert_eq!(
            validate_url("  https://example.com/hooks  ", false),
            Ok("https://example.com/hooks".to_owned()),
        );
        // A bare origin gains the root path the parser normalizes it to.
        assert_eq!(
            validate_url("https://example.com", false),
            Ok("https://example.com/".to_owned()),
        );
    }

    #[test]
    fn plain_http_is_a_development_convenience_only() {
        validate_url("http://localhost:4000/hooks", true).expect("development accepts plain http");

        let refused = validate_url("http://localhost:4000/hooks", false)
            .expect_err("production must refuse http");
        assert!(refused.contains("https"), "{refused}");
    }

    #[test]
    fn other_schemes_hosts_and_credentials_are_refused() {
        for url in [
            "ftp://example.com/hooks",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "not a url",
            "",
        ] {
            assert!(validate_url(url, true).is_err(), "must refuse {url:?}");
        }

        let credentials = validate_url("https://user:pass@example.com/hooks", false)
            .expect_err("credentials must be refused");
        assert!(credentials.contains("credentials"), "{credentials}");
    }
}

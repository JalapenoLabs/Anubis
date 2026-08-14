//! OAuth sign-in over OpenID Connect: "Continue with Google" end to end.
//!
//! Three routes carry the whole flow, mounted with the rest of authentication
//! (conventionally under `/auth`):
//!
//! | Route | Effect |
//! |---|---|
//! | `GET /oauth/providers` | List the providers this deployment configured |
//! | `GET /oauth/{provider}/start` | Redirect the browser to the provider |
//! | `GET /oauth/{provider}/callback` | Verify the response and issue the session |
//!
//! The two flow routes are browser navigations, so both answer with a redirect
//! rather than JSON: success lands on the destination the user was headed for,
//! and every failure lands on `/sign-in?error=<code>` with a machine-readable
//! code the sign-in page renders. The codes are listed in `docs/api.md`.
//!
//! `providers` is the discovery route the sign-in page reads before it renders
//! anything: a button for a provider whose credentials are unset would fail
//! with `oauth_unavailable` at click time, so the page asks which buttons are
//! real. It answers signed out, because that is the only state it is read in.
//!
//! # The flow
//!
//! `start` discovers the provider's endpoints from its issuer (cached for the
//! life of the process), generates a PKCE verifier and a nonce, and stores
//! them server-side under a fresh opaque token. That token is the OAuth
//! `state` parameter, so the row is found by the value the provider echoes
//! back; only its SHA-256 is stored, and consuming it deletes it, which makes
//! a replayed callback fail exactly like an expired one. The destination the
//! user was headed for rides in the same row rather than in the URL, so it
//! never has to be trusted on the way back.
//!
//! `callback` exchanges the code with the PKCE verifier, verifies the ID
//! token's signature, issuer, audience, and nonce, and hands the claims to
//! the identity resolver, which links the account that already owns a
//! verified address or creates one with the same bootstrap registration
//! performs. The session cookie is issued exactly as password login issues it.
//!
//! # Configuration
//!
//! A provider is enabled by setting its client id and secret; see
//! [`known_providers`] for the registry and [`crate::config`] for the
//! variables it reads. The redirect URI a provider must be registered with is
//! `<APP_URL>/auth/oauth/<provider>/callback`, so `APP_URL` has to be the
//! origin the browser sees (in development that is the Vite dev server, which
//! proxies `/auth` to the backend).

mod identity;
mod provider;

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{Duration, Utc};
use diesel::prelude::*;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use openidconnect::core::{
    CoreAuthenticationFlow, CoreClient, CoreIdTokenClaims, CoreProviderMetadata,
};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet,
    EndpointSet, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope,
    TokenResponse,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::auth::routes::{AuthState, signed_in_jar};
use crate::auth::token;
use crate::schema::oauth_states;

#[doc(inline)]
pub use provider::{
    IssuerError, OauthProvider, OauthProviderConfig, find_provider, known_providers,
};

/// How long a started flow may wait for its callback.
///
/// Long enough for a consent screen and a password manager, short enough that
/// an abandoned flow is not a standing invitation.
const FLOW_TTL_MINUTES: i64 = 15;

/// Query parameter carrying a failure code back to the sign-in page.
const ERROR_PARAM: &str = "error";

/// Where a failed flow sends the browser.
const SIGN_IN_PATH: &str = "/sign-in";

/// Where a signed-in user lands when no destination was preserved.
const DEFAULT_DESTINATION: &str = "/";

/// The client shape [`CoreClient::from_provider_metadata`] produces: an
/// authorization endpoint from discovery, with the token and userinfo
/// endpoints present only if the provider published them.
type DiscoveredClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

pub(crate) fn router() -> Router<AuthState> {
    Router::new()
        .route("/oauth/providers", get(providers))
        .route("/oauth/{provider}/start", get(start))
        .route("/oauth/{provider}/callback", get(callback))
}

/// The configured providers, their discovery cache, and the HTTP client.
///
/// Cloning shares one cache and one connection pool, following the
/// `Arc<Inner>` service convention the rest of the framework uses.
#[derive(Clone)]
pub(crate) struct Runtime {
    inner: Arc<Inner>,
}

struct Inner {
    http: openidconnect::reqwest::Client,
    providers: Vec<OauthProviderConfig>,
    /// Discovery documents, keyed by provider. Endpoints and signing keys
    /// change rarely, and a stale key surfaces as a failed sign-in rather
    /// than silently; a restart re-discovers.
    metadata: Mutex<HashMap<&'static str, CoreProviderMetadata>>,
}

impl Runtime {
    /// Builds the runtime for the providers the environment enabled.
    ///
    /// # Panics
    /// Panics when the rustls backend cannot initialize, which means a broken
    /// build rather than a runtime condition; `reqwest::Client::new` panics
    /// for the same reason.
    pub(crate) fn new(providers: Vec<OauthProviderConfig>) -> Self {
        let http = openidconnect::reqwest::ClientBuilder::new()
            // Following redirects on a server-side token or discovery call
            // turns a compromised provider response into an SSRF primitive.
            .redirect(openidconnect::reqwest::redirect::Policy::none())
            .build()
            .expect("the rustls TLS backend must initialize");

        Self {
            inner: Arc::new(Inner {
                http,
                providers,
                metadata: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The providers the environment enabled, in registry order.
    fn enabled(&self) -> impl Iterator<Item = &'static OauthProvider> + '_ {
        self.inner
            .providers
            .iter()
            .map(OauthProviderConfig::provider)
    }

    /// The configuration for `key`, when that provider is enabled.
    fn config(&self, key: &str) -> Option<&OauthProviderConfig> {
        self.inner
            .providers
            .iter()
            .find(|config| config.provider().key == key)
    }

    /// The client every outbound call to a provider goes through.
    fn http(&self) -> &openidconnect::reqwest::Client {
        &self.inner.http
    }

    /// The provider's discovery document, fetched once per process.
    ///
    /// The lock is held across the fetch, which is what collapses a burst of
    /// first sign-ins into one discovery request instead of a stampede.
    async fn metadata(
        &self,
        config: &OauthProviderConfig,
    ) -> Result<CoreProviderMetadata, Failure> {
        let key = config.provider().key;
        let mut cache = self.inner.metadata.lock().await;
        if let Some(metadata) = cache.get(key) {
            return Ok(metadata.clone());
        }

        let issuer = IssuerUrl::new(config.issuer().to_owned()).map_err(|error| {
            tracing::error!(
                oauth.provider = key,
                error.message = %error,
                "{{oauth.provider}} has an unusable issuer: {{error.message}}",
            );
            Failure::Unavailable
        })?;
        let metadata = CoreProviderMetadata::discover_async(issuer, self.http())
            .await
            .map_err(|error| {
                tracing::error!(
                    oauth.provider = key,
                    error.message = %error,
                    "discovery failed for {{oauth.provider}}: {{error.message}}",
                );
                Failure::Unavailable
            })?;

        cache.insert(key, metadata.clone());
        Ok(metadata)
    }
}

/// One configured provider, as a sign-in page needs it.
///
/// Two fields, and no more: the key the start URL is built from, and the name
/// a button reads. A client id is not a secret, but an unauthenticated route
/// is the wrong place to hand one out, and nothing on the page needs it.
#[derive(Serialize)]
struct ProviderView {
    key: &'static str,
    display_name: &'static str,
}

#[derive(Serialize)]
struct ProvidersBody {
    providers: Vec<ProviderView>,
}

/// Answers with the providers this deployment can actually sign in with.
///
/// The list is the environment's, so it is empty until a provider's
/// credentials are set, and a sign-in page that renders it renders no button
/// that would fail.
async fn providers(State(state): State<AuthState>) -> Json<ProvidersBody> {
    let providers = state
        .oauth
        .enabled()
        .map(|provider| ProviderView {
            key: provider.key,
            display_name: provider.display_name,
        })
        .collect();

    Json(ProvidersBody { providers })
}

#[derive(Deserialize)]
struct StartQuery {
    /// Where to land once signed in, root-relative.
    ///
    /// Named for the SPA's `DESTINATION_PARAM` in `frontend/src/urls.ts`,
    /// which is what the sign-in page appends to this link.
    next: Option<String>,
}

async fn start(
    State(state): State<AuthState>,
    Path(provider_key): Path<String>,
    Query(query): Query<StartQuery>,
) -> Response {
    match begin_flow(&state, &provider_key, query.next.as_deref()).await {
        Ok(location) => Redirect::to(location.as_str()).into_response(),
        Err(failure) => failure.redirect(&state.app_url),
    }
}

/// Prepares one authorization request and returns the URL to send the browser to.
async fn begin_flow(
    state: &AuthState,
    provider_key: &str,
    destination: Option<&str>,
) -> Result<url::Url, Failure> {
    let Some(config) = state.oauth.config(provider_key) else {
        tracing::warn!(
            oauth.provider = provider_key,
            "sign-in asked for {{oauth.provider}}, which is not a configured provider",
        );
        return Err(Failure::Unavailable);
    };
    let provider = config.provider();

    let metadata = state.oauth.metadata(config).await?;
    let client = discovered_client(config, metadata, &state.app_url)?;

    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let raw_token = token::generate();
    let state_parameter = CsrfToken::new(raw_token.clone());

    let mut request = client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            // The `state` the provider echoes is our own token, so the
            // callback finds the flow row by the value it is handed.
            || state_parameter,
            Nonce::new_random,
        )
        .set_pkce_challenge(challenge);
    for scope in provider.scopes {
        request = request.add_scope(Scope::new((*scope).to_owned()));
    }
    let (location, _csrf_token, nonce) = request.url();

    let mut connection = state.pool.get().await.map_err(internal)?;
    store_flow(
        &mut connection,
        provider.key,
        &raw_token,
        &nonce,
        &verifier,
        destination.and_then(sanitize_destination),
    )
    .await?;

    Ok(location)
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    /// Present when the provider refused, e.g. `access_denied`.
    error: Option<String>,
    error_description: Option<String>,
}

async fn callback(
    State(state): State<AuthState>,
    Path(provider_key): Path<String>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    match finish_flow(&state, &provider_key, query).await {
        Ok((jar, destination)) => {
            let location = format!("{}{destination}", state.app_url);
            (jar, Redirect::to(&location)).into_response()
        }
        Err(failure) => failure.redirect(&state.app_url),
    }
}

/// Verifies one callback and returns the session cookie and where to land.
async fn finish_flow(
    state: &AuthState,
    provider_key: &str,
    query: CallbackQuery,
) -> Result<(axum_extra::extract::CookieJar, String), Failure> {
    let Some(config) = state.oauth.config(provider_key) else {
        tracing::warn!(
            oauth.provider = provider_key,
            "a callback arrived for {{oauth.provider}}, which is not a configured provider",
        );
        return Err(Failure::Unavailable);
    };

    if let Some(error) = query.error {
        tracing::info!(
            oauth.provider = provider_key,
            oauth.error = error,
            oauth.error_description = query.error_description.unwrap_or_default(),
            "{{oauth.provider}} refused the request: {{oauth.error}}",
        );
        return Err(Failure::Denied);
    }

    let (Some(code), Some(raw_token)) = (query.code, query.state) else {
        tracing::warn!(
            oauth.provider = provider_key,
            "a callback arrived from {{oauth.provider}} without a code and state",
        );
        return Err(Failure::Expired);
    };

    let mut connection = state.pool.get().await.map_err(internal)?;
    let flow = take_flow(&mut connection, config.provider().key, &raw_token).await?;

    let metadata = state.oauth.metadata(config).await?;
    let client = discovered_client(config, metadata, &state.app_url)?;

    let token_response = client
        .exchange_code(AuthorizationCode::new(code))
        .map_err(|error| {
            tracing::error!(
                oauth.provider = provider_key,
                error.message = %error,
                "{{oauth.provider}} published no token endpoint: {{error.message}}",
            );
            Failure::Unavailable
        })?
        .set_pkce_verifier(PkceCodeVerifier::new(flow.pkce_verifier))
        .request_async(state.oauth.http())
        .await
        .map_err(|error| {
            tracing::warn!(
                oauth.provider = provider_key,
                error.message = %error,
                "the code exchange with {{oauth.provider}} failed: {{error.message}}",
            );
            Failure::Failed
        })?;

    let id_token = token_response.id_token().ok_or_else(|| {
        tracing::warn!(
            oauth.provider = provider_key,
            "{{oauth.provider}} returned no ID token, so no identity was proven",
        );
        Failure::Failed
    })?;
    let claims: &CoreIdTokenClaims = id_token
        .claims(&client.id_token_verifier(), &Nonce::new(flow.nonce))
        .map_err(|error| {
            tracing::warn!(
                oauth.provider = provider_key,
                error.message = %error,
                "the ID token from {{oauth.provider}} failed verification: {{error.message}}",
            );
            Failure::Failed
        })?;

    let asserted = identity::read_claims(claims);
    let user = identity::sign_in(
        &mut connection,
        &state.hasher,
        config.provider().key,
        &asserted,
    )
    .await
    .map_err(|error| match error {
        identity::LinkError::EmailUnavailable => Failure::EmailUnavailable,
        identity::LinkError::EmailUnverified => Failure::EmailUnverified,
        identity::LinkError::Database(error) => internal(error),
        // A shed hash is load rather than a bug, so the sign-in page asks the
        // user to try again instead of reporting a failure they cannot act on.
        identity::LinkError::Password(error) if error.is_overloaded() => Failure::Unavailable,
        identity::LinkError::Password(error) => internal(error),
    })?;

    let jar = signed_in_jar(state, &mut connection, user.id)
        .await
        .map_err(|_error| Failure::Failed)?;

    Ok((
        jar,
        flow.destination
            .unwrap_or_else(|| DEFAULT_DESTINATION.to_owned()),
    ))
}

/// Builds the OpenID Connect client for one provider.
fn discovered_client(
    config: &OauthProviderConfig,
    metadata: CoreProviderMetadata,
    app_url: &str,
) -> Result<DiscoveredClient, Failure> {
    let redirect_uri = format!("{app_url}/auth/oauth/{}/callback", config.provider().key);
    let redirect_uri = RedirectUrl::new(redirect_uri).map_err(|error| {
        tracing::error!(
            error.message = %error,
            "APP_URL does not form a usable OAuth redirect URI: {{error.message}}",
        );
        Failure::Unavailable
    })?;

    Ok(CoreClient::from_provider_metadata(
        metadata,
        ClientId::new(config.client_id().to_owned()),
        Some(ClientSecret::new(config.client_secret().to_owned())),
    )
    .set_redirect_uri(redirect_uri))
}

/// The server-side half of one in-flight flow.
struct Flow {
    nonce: String,
    pkce_verifier: String,
    destination: Option<String>,
}

/// Stores an in-flight flow, keyed by the hash of the state token.
async fn store_flow(
    connection: &mut AsyncPgConnection,
    provider: &str,
    raw_token: &str,
    nonce: &Nonce,
    verifier: &PkceCodeVerifier,
    destination: Option<&str>,
) -> Result<(), Failure> {
    // Opportunistically sweep flows nobody came back for.
    diesel::delete(oauth_states::table.filter(oauth_states::expires_at.le(Utc::now())))
        .execute(connection)
        .await
        .map_err(internal)?;

    diesel::insert_into(oauth_states::table)
        .values((
            oauth_states::provider.eq(provider),
            oauth_states::token_hash.eq(token::hash(raw_token)),
            oauth_states::nonce.eq(nonce.secret()),
            oauth_states::pkce_verifier.eq(verifier.secret()),
            oauth_states::destination.eq(destination),
            oauth_states::expires_at.eq(Utc::now() + Duration::minutes(FLOW_TTL_MINUTES)),
        ))
        .execute(connection)
        .await
        .map_err(internal)?;

    Ok(())
}

/// Consumes an in-flight flow by its state token; a token is single-use.
async fn take_flow(
    connection: &mut AsyncPgConnection,
    provider: &str,
    raw_token: &str,
) -> Result<Flow, Failure> {
    let row: Option<(String, String, Option<String>)> = diesel::delete(
        oauth_states::table
            .filter(oauth_states::token_hash.eq(token::hash(raw_token)))
            .filter(oauth_states::provider.eq(provider))
            .filter(oauth_states::expires_at.gt(Utc::now())),
    )
    .returning((
        oauth_states::nonce,
        oauth_states::pkce_verifier,
        oauth_states::destination,
    ))
    .get_result(connection)
    .await
    .optional()
    .map_err(internal)?;

    let Some((nonce, pkce_verifier, destination)) = row else {
        tracing::warn!(
            oauth.provider = provider,
            "a callback from {{oauth.provider}} carried an unknown, used, or expired state",
        );
        return Err(Failure::Expired);
    };

    Ok(Flow {
        nonce,
        pkce_verifier,
        destination,
    })
}

/// Keeps a destination only when it points back into this application.
///
/// The value arrives in a query string, so it is attacker-controlled:
/// unchecked, `/auth/oauth/google/start?next=https://evil.example` would turn
/// sign-in into an open redirect. This is the server-side twin of
/// `sanitizeDestination` in the starter's `urls.ts`, and rejects the same
/// shapes: absolute URLs, protocol-relative `//host` and its `/\host` cousin,
/// and the control characters browsers strip before resolving a URL.
fn sanitize_destination(destination: &str) -> Option<&str> {
    let is_internal = destination.starts_with('/')
        && !destination.starts_with("//")
        && !destination.starts_with("/\\")
        && !destination.chars().any(char::is_control);

    if !is_internal {
        tracing::warn!(
            "an OAuth flow asked to land outside the application; the destination was dropped",
        );
        return None;
    }
    Some(destination)
}

/// Why a flow ended on the sign-in page instead of signed in.
///
/// Each variant carries the code the sign-in page renders a message for; the
/// detail stays in the logs, because the browser is the wrong place to
/// explain another system's failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// The provider is unknown, unconfigured, or could not be reached.
    Unavailable,
    /// The user, or the provider, refused the request.
    Denied,
    /// The state token was unknown, already used, or too old.
    Expired,
    /// The provider returned no email address.
    EmailUnavailable,
    /// The provider would not vouch for the address it returned.
    EmailUnverified,
    /// Anything else: the exchange, the ID token, or this application.
    Failed,
}

impl Failure {
    /// The `error` code the sign-in page reads.
    fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "oauth_unavailable",
            Self::Denied => "oauth_denied",
            Self::Expired => "oauth_expired",
            Self::EmailUnavailable => "oauth_email_unavailable",
            Self::EmailUnverified => "oauth_email_unverified",
            Self::Failed => "oauth_failed",
        }
    }

    /// The redirect a failed flow answers with.
    fn redirect(self, app_url: &str) -> Response {
        let location = format!("{app_url}{SIGN_IN_PATH}?{ERROR_PARAM}={}", self.code());
        Redirect::to(&location).into_response()
    }
}

/// Logs a failure this application owns and answers with the generic code.
fn internal(error: impl std::fmt::Display) -> Failure {
    tracing::error!(
        error.message = %error,
        "an OAuth sign-in failed inside the application: {{error.message}}",
    );
    Failure::Failed
}

#[cfg(test)]
mod tests {
    use super::{Failure, sanitize_destination};

    #[test]
    fn root_relative_destinations_survive_and_nothing_else_does() {
        assert_eq!(sanitize_destination("/members"), Some("/members"));
        assert_eq!(
            sanitize_destination("/claim-invitation?token=abc"),
            Some("/claim-invitation?token=abc"),
        );

        for hostile in [
            "https://evil.example",
            "//evil.example",
            "/\\evil.example",
            "/members\nSet-Cookie: x=y",
            "members",
            "",
        ] {
            assert_eq!(sanitize_destination(hostile), None, "for input {hostile:?}");
        }
    }

    #[test]
    fn every_failure_lands_on_sign_in_with_a_distinct_code() {
        let failures = [
            Failure::Unavailable,
            Failure::Denied,
            Failure::Expired,
            Failure::EmailUnavailable,
            Failure::EmailUnverified,
            Failure::Failed,
        ];

        let mut codes = failures.map(Failure::code).to_vec();
        codes.sort_unstable();
        let total = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), total, "failure codes must be distinct");

        for failure in failures {
            assert!(
                failure.code().starts_with("oauth_"),
                "{failure:?} must be namespaced",
            );
        }
    }
}

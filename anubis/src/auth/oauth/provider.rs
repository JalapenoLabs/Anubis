//! The registry of known OpenID Connect providers, and their configuration.
//!
//! A provider is a row in [`KNOWN_PROVIDERS`]: a URL key, a display name, the
//! issuer whose discovery document configures the flow, the environment
//! variables that carry its credentials, and the scopes it is asked for
//! beyond `openid`. Everything else about a provider (endpoints, signing keys,
//! supported algorithms) comes from the issuer's own
//! `/.well-known/openid-configuration`, so adding a provider is adding a row.
//!
//! # The OpenID Connect boundary
//!
//! The registry is deliberately OpenID Connect only. Every entry must publish
//! a discovery document and return a signed ID token, because that is what
//! lets the framework verify an identity rather than trust an access token.
//! Two consequences are worth stating plainly:
//!
//! - **GitHub is not an OpenID Connect provider.** It has no discovery
//!   document and issues no ID token; identity comes from calling
//!   `/user` with an access token. Supporting it needs a second, plain OAuth 2
//!   code path with a per-provider profile fetch, which is its own design and
//!   is not in the framework today.
//! - **Multi-tenant Microsoft Entra ID** serves `login.microsoftonline.com/common`
//!   with a per-tenant `iss` claim, which fails the issuer check every OpenID
//!   Connect client makes. A single-tenant issuer works today through
//!   `<PROVIDER>_OAUTH_ISSUER`; the `common` endpoint needs tenant-aware
//!   issuer validation first.

use std::fmt::{self, Formatter};

/// A known OpenID Connect provider.
///
/// # Examples
/// ```
/// let google = anubis::auth::oauth::find_provider("google").expect("google is known");
/// assert_eq!(google.issuer, "https://accounts.google.com");
/// assert_eq!(google.client_id_var, "GOOGLE_OAUTH_CLIENT_ID");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OauthProvider {
    /// The key in URLs (`/auth/oauth/{key}/start`) and CLI arguments.
    pub key: &'static str,
    /// The provider's name as a user reads it on a button.
    pub display_name: &'static str,
    /// The issuer whose discovery document configures the flow.
    pub issuer: &'static str,
    /// Environment variable carrying the OAuth client id.
    pub client_id_var: &'static str,
    /// Environment variable carrying the OAuth client secret.
    pub client_secret_var: &'static str,
    /// Environment variable overriding [`OauthProvider::issuer`].
    ///
    /// Set it for a provider whose issuer is per-installation (a self-hosted
    /// identity server, a single-tenant Entra ID directory), and in tests.
    pub issuer_var: &'static str,
    /// Scopes requested beyond `openid`, which the client always sends.
    pub scopes: &'static [&'static str],
}

/// Every provider the framework knows how to talk to.
///
/// # Examples
/// ```
/// let keys: Vec<_> = anubis::auth::oauth::known_providers()
///     .iter()
///     .map(|provider| provider.key)
///     .collect();
/// assert_eq!(keys, ["google"]);
/// ```
#[must_use]
pub fn known_providers() -> &'static [OauthProvider] {
    KNOWN_PROVIDERS
}

/// Looks a provider up by its key, as a URL or a CLI argument spells it.
#[must_use]
pub fn find_provider(key: &str) -> Option<&'static OauthProvider> {
    KNOWN_PROVIDERS.iter().find(|provider| provider.key == key)
}

/// The registry itself.
static KNOWN_PROVIDERS: &[OauthProvider] = &[OauthProvider {
    key: "google",
    display_name: "Google",
    // Google's issuer has no trailing slash, and the discovery document
    // returns this exact string, which the client checks.
    issuer: "https://accounts.google.com",
    client_id_var: "GOOGLE_OAUTH_CLIENT_ID",
    client_secret_var: "GOOGLE_OAUTH_CLIENT_SECRET",
    issuer_var: "GOOGLE_OAUTH_ISSUER",
    // `email` carries the address and its verified flag; `profile` carries the
    // names a fresh account is created with.
    scopes: &["email", "profile"],
}];

/// One provider's resolved credentials, read from the environment.
///
/// [`crate::config::AppConfig`] holds the enabled providers; a provider whose
/// client id and secret are both unset is simply not enabled.
#[derive(Clone, PartialEq, Eq)]
pub struct OauthProviderConfig {
    provider: &'static OauthProvider,
    client_id: String,
    client_secret: String,
    issuer: String,
}

impl OauthProviderConfig {
    /// Builds a configuration for `provider`.
    ///
    /// `issuer` overrides the registry's issuer when present, which is how a
    /// self-hosted or single-tenant deployment (and the test suite) points the
    /// same provider at another discovery document.
    ///
    /// # Errors
    /// Returns [`IssuerError`] when the issuer is not an absolute `http(s)`
    /// URL without a query string or fragment, which is what OpenID Connect
    /// requires of an issuer identifier.
    pub fn new(
        provider: &'static OauthProvider,
        client_id: String,
        client_secret: String,
        issuer: Option<String>,
    ) -> Result<Self, IssuerError> {
        let issuer = issuer.unwrap_or_else(|| provider.issuer.to_owned());
        let parsed = url::Url::parse(&issuer).map_err(|_error| IssuerError)?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(IssuerError);
        }

        Ok(Self {
            provider,
            client_id,
            client_secret,
            issuer,
        })
    }

    /// The registry entry this configuration belongs to.
    #[must_use]
    pub fn provider(&self) -> &'static OauthProvider {
        self.provider
    }

    /// The OAuth client id registered with the provider.
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// The OAuth client secret registered with the provider.
    #[must_use]
    pub fn client_secret(&self) -> &str {
        &self.client_secret
    }

    /// The issuer whose discovery document configures the flow.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
}

impl fmt::Debug for OauthProviderConfig {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("OauthProviderConfig")
            .field("provider", &self.provider.key)
            .field("client_id", &self.client_id)
            .field("client_secret", &"...")
            .field("issuer", &self.issuer)
            .finish()
    }
}

/// An issuer that is not a usable OpenID Connect issuer identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IssuerError;

impl fmt::Display for IssuerError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("expected an http(s) URL without a query string or fragment")
    }
}

impl std::error::Error for IssuerError {}

#[cfg(test)]
mod tests {
    use super::{OauthProviderConfig, find_provider, known_providers};

    #[test]
    fn every_registry_entry_is_findable_and_fully_specified() {
        for provider in known_providers() {
            assert_eq!(
                find_provider(provider.key).map(|found| found.key),
                Some(provider.key),
            );
            assert!(provider.issuer.starts_with("https://"), "{}", provider.key);
            assert!(
                provider.client_id_var.ends_with("_OAUTH_CLIENT_ID"),
                "{}",
                provider.key,
            );
            assert!(
                provider.client_secret_var.ends_with("_OAUTH_CLIENT_SECRET"),
                "{}",
                provider.key,
            );
            assert!(!provider.display_name.is_empty(), "{}", provider.key);
        }
    }

    #[test]
    fn an_unknown_key_is_not_a_provider() {
        assert!(find_provider("github").is_none());
        assert!(find_provider("").is_none());
        assert!(find_provider("GOOGLE").is_none());
    }

    #[test]
    fn the_issuer_override_replaces_the_registry_issuer() {
        let google = find_provider("google").expect("google is known");

        let defaulted =
            OauthProviderConfig::new(google, "id".to_owned(), "secret".to_owned(), None)
                .expect("the registry issuer is valid");
        assert_eq!(defaulted.issuer(), "https://accounts.google.com");

        let overridden = OauthProviderConfig::new(
            google,
            "id".to_owned(),
            "secret".to_owned(),
            Some("http://127.0.0.1:9999".to_owned()),
        )
        .expect("a local issuer is valid");
        assert_eq!(overridden.issuer(), "http://127.0.0.1:9999");
    }

    #[test]
    fn an_unusable_issuer_is_rejected() {
        let google = find_provider("google").expect("google is known");
        for issuer in [
            "not-a-url",
            "ftp://example.com",
            "https://example.com?tenant=1",
            "https://example.com#fragment",
        ] {
            assert!(
                OauthProviderConfig::new(
                    google,
                    "id".to_owned(),
                    "secret".to_owned(),
                    Some(issuer.to_owned()),
                )
                .is_err(),
                "for issuer {issuer}",
            );
        }
    }

    #[test]
    fn debug_output_never_leaks_the_client_secret() {
        let google = find_provider("google").expect("google is known");
        let config = OauthProviderConfig::new(
            google,
            "public-id".to_owned(),
            "super-secret-value".to_owned(),
            None,
        )
        .expect("valid config");

        let rendered = format!("{config:?}");
        assert!(rendered.contains("public-id"), "got: {rendered}");
        assert!(!rendered.contains("super-secret-value"), "got: {rendered}");
    }
}

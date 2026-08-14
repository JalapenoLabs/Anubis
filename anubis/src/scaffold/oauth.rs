//! Planning one `anubis scaffold oauth <provider>` run.
//!
//! This scaffolder generates nothing, and that is the design rather than a
//! gap. Every part of a provider is framework behavior: the routes, the flow,
//! the identity linking, and the account bootstrap live in
//! [`crate::auth::oauth`] and arrive with the dependency, and the sign-in page
//! renders one button per provider that `GET /auth/oauth/providers` reports,
//! so the page needs no per-provider code either. A provider is added by
//! setting two environment variables.
//!
//! What is left is the part only a person can do: registering an OAuth client
//! with the provider, under the exact redirect URI this deployment answers on.
//! This type computes that URI and carries the registry entry the CLI prints
//! the variable names from.
//!
//! Generating a button instead would put the provider list in two places, the
//! page and the environment, and the failure mode of that disagreement is a
//! button that always fails with `oauth_unavailable`. One source of truth is
//! worth more than one generated element.

use crate::auth::oauth::OauthProvider;

/// One provider's setup, as `anubis scaffold oauth` reports it.
///
/// # Examples
/// ```
/// use anubis::scaffold::OauthScaffold;
///
/// let provider = anubis::auth::oauth::find_provider("google").unwrap();
/// let scaffold = OauthScaffold::new(provider);
/// assert_eq!(
///     scaffold.redirect_uri("https://app.example.com"),
///     "https://app.example.com/auth/oauth/google/callback",
/// );
/// assert_eq!(scaffold.provider().client_id_var, "GOOGLE_OAUTH_CLIENT_ID");
/// ```
#[derive(Debug, Clone, Copy)]
pub struct OauthScaffold {
    provider: &'static OauthProvider,
}

impl OauthScaffold {
    /// Plans the run for one known provider.
    #[must_use]
    pub fn new(provider: &'static OauthProvider) -> Self {
        Self { provider }
    }

    /// The provider being added, including the variables that enable it.
    #[must_use]
    pub fn provider(self) -> &'static OauthProvider {
        self.provider
    }

    /// The redirect URI the provider must be registered with.
    ///
    /// `app_url` is the origin the browser sees, which in development is the
    /// dev server that proxies `/auth` to the backend.
    #[must_use]
    pub fn redirect_uri(self, app_url: &str) -> String {
        format!("{app_url}/auth/oauth/{}/callback", self.provider.key)
    }
}

#[cfg(test)]
mod tests {
    use super::OauthScaffold;
    use crate::auth::oauth::find_provider;

    fn google() -> OauthScaffold {
        OauthScaffold::new(find_provider("google").expect("google is known"))
    }

    #[test]
    fn the_redirect_uri_hangs_off_the_public_origin() {
        assert_eq!(
            google().redirect_uri("https://app.example.com"),
            "https://app.example.com/auth/oauth/google/callback",
        );
    }

    #[test]
    fn the_scaffold_carries_the_variables_that_enable_the_provider() {
        let provider = google().provider();

        assert_eq!(provider.client_id_var, "GOOGLE_OAUTH_CLIENT_ID");
        assert_eq!(provider.client_secret_var, "GOOGLE_OAUTH_CLIENT_SECRET");
        assert_eq!(provider.display_name, "Google");
    }
}

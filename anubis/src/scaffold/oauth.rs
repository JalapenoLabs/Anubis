//! Planning one `anubis scaffold oauth <provider>` run.
//!
//! This scaffolder is deliberately the thinnest in the family, because almost
//! all of the feature is framework behavior rather than generated code. The
//! routes, the flow, the identity linking, and the account bootstrap live in
//! [`crate::auth::oauth`] and arrive with the dependency; an application adds
//! a provider by setting two environment variables. What is left to generate
//! is the part an application owns: the button on its own sign-in page, and
//! the string on that button.
//!
//! Everything here is pure string transformation, like the rest of the
//! engine, so the CLI stays a filesystem shell over it.

use crate::auth::oauth::OauthProvider;

/// One provider's contribution to the application's sign-in page.
///
/// # Examples
/// ```
/// use anubis::scaffold::OauthScaffold;
///
/// let provider = anubis::auth::oauth::find_provider("google").unwrap();
/// let scaffold = OauthScaffold::new(provider);
/// assert!(scaffold.sign_in_button().contains("getOauthStartUrl('google', destination)"));
/// assert_eq!(
///     scaffold.locale_entries(),
///     vec![("google".to_owned(), "Continue with Google".to_owned())],
/// );
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

    /// The provider being added.
    #[must_use]
    pub fn provider(self) -> &'static OauthProvider {
        self.provider
    }

    /// The button inserted above the sign-in page's provider anchor.
    ///
    /// It is a link, not a form submit: starting a flow means leaving the SPA
    /// for the provider's consent screen, so the browser navigates rather than
    /// fetches. `destination` and `t` are the names the sign-in page already
    /// binds, which is what keeps the insertion a single element.
    #[must_use]
    pub fn sign_in_button(self) -> String {
        format!(
            "\
<Button
  as='a'
  variant='bordered'
  className='compact w-full'
  href={{getOauthStartUrl('{key}', destination)}}
>
  <span>{{
      t('auth.oauth.{key}')
    }}</span>
</Button>
",
            key = self.provider.key,
        )
    }

    /// The strings merged into the `auth.oauth` object of the base locale file.
    #[must_use]
    pub fn locale_entries(self) -> Vec<(String, String)> {
        vec![(
            self.provider.key.to_owned(),
            format!("Continue with {}", self.provider.display_name),
        )]
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
    fn the_button_links_at_the_provider_and_reads_from_the_locale_file() {
        let button = google().sign_in_button();

        assert!(
            button.contains("href={getOauthStartUrl('google', destination)}"),
            "{button}",
        );
        assert!(button.contains("t('auth.oauth.google')"), "{button}");
        // Every line has to fit the frontend's 120-column lint budget once the
        // anchor's indentation is added.
        for line in button.lines() {
            assert!(line.len() <= 100, "too long to indent: {line}");
        }
    }

    #[test]
    fn the_locale_entry_is_the_string_the_button_renders() {
        assert_eq!(
            google().locale_entries(),
            vec![("google".to_owned(), "Continue with Google".to_owned())],
        );
    }

    #[test]
    fn the_redirect_uri_hangs_off_the_public_origin() {
        assert_eq!(
            google().redirect_uri("https://app.example.com"),
            "https://app.example.com/auth/oauth/google/callback",
        );
    }
}

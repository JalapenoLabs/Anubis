//! Which redirect URIs a client may register, and when a presented one matches.
//!
//! The redirect URI is where an authorization code is delivered, so it is the
//! one thing about a client this server must never get wrong. Two rules:
//!
//! - **Registering.** An `https` URI on any host, or an `http` URI on a
//!   loopback host (`localhost`, `127.0.0.1`, `[::1]`), which is how a native
//!   application such as Claude Code or Codex receives its code. No fragment,
//!   no credentials, nothing else.
//! - **Matching.** Exact string comparison, with one exception RFC 8252 section
//!   7.3 requires: a loopback URI matches whatever port the presented URI
//!   names, because a native application binds a random port at sign-in time
//!   and registered the URI without one. Host, path, and query still match
//!   exactly, so `localhost` and `127.0.0.1` are different registrations, and
//!   both Claude Code and Codex register both.

use url::Url;

/// The longest redirect URI the server stores.
///
/// Browsers cap URLs near 2,000 characters in practice, and a redirect URI is
/// a short callback address; anything longer is not one somebody needs.
pub(crate) const MAX_REDIRECT_URI_LENGTH: usize = 2048;

/// The hosts RFC 8252 treats as the device itself.
const LOOPBACK_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "[::1]"];

/// Why a URI cannot be registered as a redirect URI, in words a client
/// developer can act on.
pub(crate) fn check_registrable(uri: &str) -> Result<(), &'static str> {
    if uri.len() > MAX_REDIRECT_URI_LENGTH {
        return Err("a redirect URI must be at most 2048 characters");
    }

    let Ok(parsed) = Url::parse(uri) else {
        return Err("a redirect URI must be an absolute URL");
    };
    if parsed.fragment().is_some() {
        return Err("a redirect URI must not carry a fragment");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("a redirect URI must not carry credentials");
    }

    match parsed.scheme() {
        "https" if parsed.host_str().is_some() => Ok(()),
        "http" if is_loopback(&parsed) => Ok(()),
        _ => Err("a redirect URI must use https, or http on a loopback host"),
    }
}

/// Whether `presented` is the redirect URI `registered` names.
pub(crate) fn matches(registered: &str, presented: &str) -> bool {
    if registered == presented {
        return true;
    }

    let (Ok(registered), Ok(presented)) = (Url::parse(registered), Url::parse(presented)) else {
        return false;
    };
    if !is_loopback(&registered) {
        return false;
    }

    // Everything but the port, compared exactly. A fragment cannot be
    // registered, so one presented here fails the comparison below.
    registered.scheme() == presented.scheme()
        && registered.host_str() == presented.host_str()
        && registered.path() == presented.path()
        && registered.query() == presented.query()
        && presented.fragment().is_none()
        && presented.username().is_empty()
        && presented.password().is_none()
}

/// Whether a parsed URI points at the device itself.
pub(crate) fn is_loopback(uri: &Url) -> bool {
    uri.host_str()
        .is_some_and(|host| LOOPBACK_HOSTS.contains(&host))
}

#[cfg(test)]
mod tests {
    use super::{check_registrable, matches};

    #[test]
    fn https_and_loopback_http_register() {
        for uri in [
            "https://claude.ai/api/mcp/auth_callback",
            "http://localhost/callback",
            "http://127.0.0.1/callback",
            "http://[::1]:8080/callback",
        ] {
            assert_eq!(check_registrable(uri), Ok(()), "{uri}");
        }
    }

    #[test]
    fn everything_else_is_refused() {
        for uri in [
            "http://example.com/callback",
            "https://example.com/callback#fragment",
            "https://user:secret@example.com/callback",
            "com.example.app:/callback",
            "javascript:alert(1)",
            "/relative/callback",
        ] {
            assert!(check_registrable(uri).is_err(), "{uri}");
        }
    }

    #[test]
    fn a_loopback_registration_matches_any_port() {
        // Claude Code and Codex register these port-less and bind a random
        // port at sign-in.
        assert!(matches(
            "http://localhost/callback",
            "http://localhost:53682/callback",
        ));
        assert!(matches(
            "http://127.0.0.1/callback",
            "http://127.0.0.1:1455/callback",
        ));
        assert!(matches(
            "http://[::1]:1/callback",
            "http://[::1]:2/callback"
        ));
    }

    #[test]
    fn a_loopback_registration_still_pins_host_path_and_query() {
        let registered = "http://localhost/callback";

        assert!(!matches(registered, "http://127.0.0.1:53682/callback"));
        assert!(!matches(registered, "http://localhost:53682/other"));
        assert!(!matches(registered, "http://localhost:53682/callback?x=1"));
        assert!(!matches(registered, "https://localhost:53682/callback"));
        assert!(!matches(registered, "http://localhost:53682/callback#x"));
    }

    #[test]
    fn a_remote_registration_matches_exactly_or_not_at_all() {
        let registered = "https://claude.ai/api/mcp/auth_callback";

        assert!(matches(registered, registered));
        assert!(!matches(
            registered,
            "https://claude.ai:443/api/mcp/auth_callback"
        ));
        assert!(!matches(
            registered,
            "https://claude.ai/api/mcp/auth_callback/"
        ));
        assert!(!matches(
            registered,
            "https://claude.ai.evil.example/api/mcp/auth_callback"
        ));
    }
}

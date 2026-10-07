//! The scopes an application lets a connected client ask for.
//!
//! A scope is the unit a person consents to, so the application declares them
//! (the framework cannot know what "read my scorecards" means) and the consent
//! screen shows each one's description beside its name. Nothing in the
//! framework hardcodes a scope: a token issued for no scopes still proves who
//! the person is, which is all the framework's own `whoami` tool needs.

use serde::Serialize;

/// The longest scope name the framework accepts.
///
/// Scope names travel in query strings, in token responses, and in every
/// `WWW-Authenticate` challenge, so a bound keeps all three small. Sixty-four
/// characters is generous for names like `samples:read`.
const MAX_SCOPE_NAME_LENGTH: usize = 64;

/// One permission a client may ask for, and the sentence that explains it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Scope {
    /// The wire name, such as `samples:read`.
    pub name: String,
    /// What granting it lets the client do, in words a person consents to.
    pub description: String,
}

/// The application's declared scopes, in the order the consent screen lists them.
///
/// ```
/// let scopes = anubis::oauth_server::Scopes::new()
///     .scope("samples:read", "Browse the sample library and download bundles")
///     .scope("entries:write", "Enter an archive in your name");
/// assert!(scopes.contains("samples:read"));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scopes {
    declared: Vec<Scope>,
}

impl Scopes {
    /// An empty set: tokens prove identity and grant nothing else.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares one more scope.
    ///
    /// # Panics
    /// Panics when `name` is not a valid RFC 6749 scope token (empty, too long,
    /// or holding a space, a quote, or a backslash), when it is
    /// `offline_access`, which the MCP specification keeps out of what a server
    /// advertises, or when it was already declared. Each is a mistake in the
    /// application's own source, caught the first time it boots.
    #[must_use]
    pub fn scope(mut self, name: impl Into<String>, description: impl Into<String>) -> Self {
        let name = name.into();
        assert!(
            is_scope_token(&name),
            "scope {name:?} must be 1 to {MAX_SCOPE_NAME_LENGTH} visible ASCII characters \
             without a space, a quote, or a backslash (RFC 6749 section 3.3)",
        );
        assert!(
            name != "offline_access",
            "offline_access is not a scope an MCP server advertises; refresh tokens are \
             issued on every grant",
        );
        assert!(!self.contains(&name), "scope {name:?} is declared twice");

        self.declared.push(Scope {
            name,
            description: description.into(),
        });
        self
    }

    /// Whether `name` was declared.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.declared.iter().any(|scope| scope.name == name)
    }

    /// The declared scopes, in declaration order.
    #[must_use]
    pub fn all(&self) -> &[Scope] {
        &self.declared
    }

    /// The scope named `name`, when it was declared.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&Scope> {
        self.declared.iter().find(|scope| scope.name == name)
    }

    /// Every declared name, in declaration order.
    pub(crate) fn names(&self) -> Vec<String> {
        self.declared
            .iter()
            .map(|scope| scope.name.clone())
            .collect()
    }

    /// Parses a space-separated `scope` parameter against the declared set.
    ///
    /// Answers the requested names deduplicated and in declaration order, so
    /// two requests for the same scopes store the same list, or the first name
    /// nobody declared.
    pub(crate) fn parse_request(&self, requested: Option<&str>) -> Result<Vec<String>, String> {
        let requested = requested.unwrap_or_default();
        for name in requested.split_ascii_whitespace() {
            if !self.contains(name) {
                return Err(name.to_owned());
            }
        }

        let granted = self
            .declared
            .iter()
            .filter(|scope| {
                requested
                    .split_ascii_whitespace()
                    .any(|name| name == scope.name)
            })
            .map(|scope| scope.name.clone())
            .collect();
        Ok(granted)
    }
}

/// Whether `name` is a scope token RFC 6749 section 3.3 allows, within bounds.
fn is_scope_token(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SCOPE_NAME_LENGTH
        && name.bytes().all(|byte| {
            byte == 0x21 || (0x23..=0x5B).contains(&byte) || (0x5D..=0x7E).contains(&byte)
        })
}

#[cfg(test)]
mod tests {
    use super::Scopes;

    fn declared() -> Scopes {
        Scopes::new()
            .scope("samples:read", "Browse samples")
            .scope("entries:write", "Enter archives")
    }

    #[test]
    fn a_request_is_deduplicated_into_declaration_order() {
        let granted = declared()
            .parse_request(Some("entries:write samples:read entries:write"))
            .expect("both scopes are declared");

        assert_eq!(granted, ["samples:read", "entries:write"]);
    }

    #[test]
    fn no_scope_parameter_grants_nothing_but_identity() {
        assert!(
            declared()
                .parse_request(None)
                .expect("no request is valid")
                .is_empty()
        );
    }

    #[test]
    fn an_undeclared_scope_is_named_in_the_refusal() {
        let refused = declared().parse_request(Some("samples:read admin"));

        assert_eq!(refused, Err("admin".to_owned()));
    }

    #[test]
    #[should_panic(expected = "declared twice")]
    fn declaring_a_scope_twice_is_a_bug() {
        let _scopes = declared().scope("samples:read", "again");
    }

    #[test]
    #[should_panic(expected = "RFC 6749")]
    fn a_scope_with_a_space_is_a_bug() {
        let _scopes = Scopes::new().scope("read samples", "Browse samples");
    }

    #[test]
    #[should_panic(expected = "offline_access")]
    fn offline_access_is_never_advertised() {
        let _scopes = Scopes::new().scope("offline_access", "Stay signed in");
    }
}

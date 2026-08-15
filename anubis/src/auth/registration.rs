//! Who may create an account here: everybody, nobody, or named domains.
//!
//! An application on a public hostname that must not accept public signups
//! needs a way to say so, and a rate limit is throttling rather than closing.
//! [`RegistrationMode`] is that switch. It is read once from
//! `ANUBIS_REGISTRATION` and `ANUBIS_REGISTRATION_DOMAINS` when configuration
//! loads (see [`crate::config`]) and carried on
//! [`crate::config::AppConfig::registration`]:
//!
//! | Mode | Who may create an account |
//! |---|---|
//! | `open` | Anybody. The default, and what an unset variable means |
//! | `invite_only` | Nobody. The way in is an invitation to an account that exists |
//! | `domain_allowlist` | Addresses whose domain the list names |
//!
//! The mode gates account **creation** and nothing else, so it is asked in the
//! two places an account is born: `POST /auth/register` and the OAuth callback
//! that meets a verified address no account owns yet
//! ([`crate::auth::oauth`]). Both ask before hashing a password, so a closed
//! deployment spends no argon2 budget on attempts it was always going to
//! refuse. Signing in, linking an OAuth identity to an account that already
//! exists, and claiming an invitation are untouched, because each of those is
//! authorization this deployment already granted.
//!
//! Matching is exact on the domain to the right of the `@`: `acme.com` admits
//! `ada@acme.com` and not `ada@mail.acme.com`, because a subdomain is a
//! different domain and a wildcard rule invented here would make the list mean
//! something nobody wrote. Name every domain that should be admitted.
//!
//! A refusal is a `403` in the standard error shape, and its message says what
//! to do next rather than which domains are admitted: the sign-up form is
//! reachable by anybody, and the allowlist is the deployment's configuration.
//! `GET /auth/registration` reports whether the form is worth rendering at
//! all, which is what keeps a closed deployment from showing one whose every
//! submission is refused.

/// The sentence a deployment that accepts no public signups answers with.
const INVITE_ONLY_REFUSAL: &str =
    "Registration is by invitation only. Ask an administrator to invite you.";

/// The sentence an address outside the allowlist is refused with.
const OUTSIDE_ALLOWLIST_REFUSAL: &str =
    "Registration is limited to approved email domains. Ask an administrator to invite you.";

/// Who a deployment lets create an account.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum RegistrationMode {
    /// Anybody may register. The default.
    #[default]
    Open,
    /// Nobody may register; an invitation is the only way in.
    InviteOnly,
    /// Only addresses in these domains, lowercased and matched exactly.
    ///
    /// Never empty: a mode that admits nobody is spelled [`Self::InviteOnly`],
    /// so configuration refuses to start on an allowlist naming no domains
    /// rather than locking everybody out silently.
    DomainAllowlist(Vec<String>),
}

impl RegistrationMode {
    /// Whether this deployment accepts new accounts from anybody at all.
    ///
    /// `false` only under [`RegistrationMode::InviteOnly`]. An allowlist still
    /// accepts registrations, just not from every address, so its sign-up form
    /// belongs on screen and its refusal belongs on submit. This is what
    /// `GET /auth/registration` answers with.
    #[must_use]
    pub fn accepts_registrations(&self) -> bool {
        !matches!(self, Self::InviteOnly)
    }

    /// Why this deployment will not create an account for `email`, if it will not.
    ///
    /// `None` admits the address; `Some` carries the sentence to answer with.
    /// `email` is expected in the form [`crate::auth::routes::validate_email`]
    /// produces, trimmed and lowercased, which is what both callers hold by
    /// the time they ask.
    #[must_use]
    pub fn refusal(&self, email: &str) -> Option<&'static str> {
        match self {
            Self::Open => None,
            Self::InviteOnly => Some(INVITE_ONLY_REFUSAL),
            Self::DomainAllowlist(domains) => {
                let admitted = email.rsplit_once('@').is_some_and(|(_local, domain)| {
                    domains.iter().any(|allowed| allowed == domain)
                });
                (!admitted).then_some(OUTSIDE_ALLOWLIST_REFUSAL)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RegistrationMode;

    fn allowlist() -> RegistrationMode {
        RegistrationMode::DomainAllowlist(vec!["acme.com".to_owned(), "acme.co.uk".to_owned()])
    }

    #[test]
    fn an_open_deployment_admits_every_address() {
        let mode = RegistrationMode::default();

        assert_eq!(mode, RegistrationMode::Open);
        assert!(mode.accepts_registrations());
        assert_eq!(mode.refusal("ada@example.com"), None);
    }

    #[test]
    fn an_invite_only_deployment_admits_nobody_and_says_why() {
        let mode = RegistrationMode::InviteOnly;

        assert!(!mode.accepts_registrations());
        let refusal = mode
            .refusal("ada@example.com")
            .expect("invite-only refuses every address");
        assert!(refusal.contains("invitation"), "got: {refusal}");
    }

    #[test]
    fn an_allowlist_admits_the_domains_it_names_and_no_others() {
        let mode = allowlist();

        assert!(
            mode.accepts_registrations(),
            "the sign-up form still belongs on screen",
        );
        assert_eq!(mode.refusal("ada@acme.com"), None);
        assert_eq!(mode.refusal("ada@acme.co.uk"), None);
    }

    #[test]
    fn an_allowlist_matches_the_domain_exactly() {
        let mode = allowlist();

        for email in [
            // A subdomain is a different domain.
            "ada@mail.acme.com",
            // So is a domain that merely ends in one.
            "ada@notacme.com",
            "ada@example.com",
            // An address the validator would have refused first.
            "acme.com",
        ] {
            assert!(
                mode.refusal(email).is_some(),
                "{email:?} must be refused by {mode:?}",
            );
        }
    }
}

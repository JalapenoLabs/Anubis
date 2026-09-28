//! What counts as a usable email address and a usable password.
//!
//! Registration asks these questions over HTTP and answers a `422`; the boot
//! that seeds an operator account asks them of two environment variables and
//! refuses to start. Both need the same answer, so the rules live here once
//! and each caller renders the outcome in its own vocabulary.

use crate::auth::password;

/// Upper bound from RFC 3696; anything longer cannot be a deliverable address.
const MAX_EMAIL_CHARS: usize = 320;

/// Normalizes an email address, returning `None` when it cannot be one.
///
/// Normalization is trim plus lowercase, and it happens before anything reads
/// or writes the address, so registration, sign-in, and the seeded operator
/// always agree on the stored form. The structural check is deliberately
/// shallow: one `@`, something on each side, no whitespace. Deciding whether
/// an address receives mail is what sending to it does.
pub(crate) fn normalize_email(raw: &str) -> Option<String> {
    let email = raw.trim().to_lowercase();

    if email.is_empty() || email.chars().count() > MAX_EMAIL_CHARS {
        return None;
    }
    let (local, domain) = email.split_once('@')?;
    if local.is_empty()
        || domain.is_empty()
        || domain.contains('@')
        || email.contains(char::is_whitespace)
    {
        return None;
    }

    Some(email)
}

/// The password policy rule a candidate broke.
///
/// Rendering it is `Display`, which reads as the requirement rather than the
/// failure ("at least 8 characters"), because both callers put it after the
/// word "expected".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PasswordPolicy {
    /// Shorter than [`password::MIN_PASSWORD_CHARS`].
    TooShort,
    /// Longer than [`password::MAX_PASSWORD_CHARS`].
    TooLong,
}

impl std::fmt::Display for PasswordPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort => write!(f, "at least {} characters", password::MIN_PASSWORD_CHARS),
            Self::TooLong => write!(f, "at most {} characters", password::MAX_PASSWORD_CHARS),
        }
    }
}

/// Checks a password against the length policy every entry point shares.
///
/// The upper bound is not cosmetic: argon2 hashes whatever it is given, so an
/// unbounded password is an unbounded amount of work a stranger can ask for.
///
/// # Errors
/// Returns the rule the candidate broke.
pub(crate) fn check_password_policy(candidate: &str) -> Result<(), PasswordPolicy> {
    let password_chars = candidate.chars().count();
    if password_chars < password::MIN_PASSWORD_CHARS {
        return Err(PasswordPolicy::TooShort);
    }
    if password_chars > password::MAX_PASSWORD_CHARS {
        return Err(PasswordPolicy::TooLong);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{MAX_EMAIL_CHARS, PasswordPolicy, check_password_policy, normalize_email};
    use crate::auth::password;

    #[test]
    fn addresses_are_trimmed_and_lowercased() {
        assert_eq!(
            normalize_email("  Ops@Acme.COM \n").as_deref(),
            Some("ops@acme.com"),
        );
    }

    #[test]
    fn shapes_that_cannot_be_an_address_are_refused() {
        for candidate in [
            "",
            "   ",
            "no-at-sign",
            "@acme.com",
            "ops@",
            "two@at@acme.com",
            "ops name@acme.com",
        ] {
            assert_eq!(normalize_email(candidate), None, "accepted {candidate:?}");
        }

        let far_too_long = format!("{}@acme.com", "a".repeat(MAX_EMAIL_CHARS));
        assert_eq!(normalize_email(&far_too_long), None);
    }

    #[test]
    fn the_password_policy_is_a_length_range() {
        assert_eq!(
            check_password_policy(&"a".repeat(password::MIN_PASSWORD_CHARS - 1)),
            Err(PasswordPolicy::TooShort),
        );
        assert_eq!(
            check_password_policy(&"a".repeat(password::MAX_PASSWORD_CHARS + 1)),
            Err(PasswordPolicy::TooLong),
        );
        assert_eq!(
            check_password_policy(&"a".repeat(password::MIN_PASSWORD_CHARS)),
            Ok(()),
        );
    }

    #[test]
    fn a_violation_reads_as_the_requirement() {
        assert_eq!(
            PasswordPolicy::TooShort.to_string(),
            format!("at least {} characters", password::MIN_PASSWORD_CHARS),
        );
    }
}

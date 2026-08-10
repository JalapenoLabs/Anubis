//! Turning verified provider claims into a signed-in Anubis user.
//!
//! Three cases, in order:
//!
//! 1. The `(provider, subject)` pair is already linked. That user signs in,
//!    whatever their email address is today.
//! 2. The provider asserts a **verified** email that an account already uses.
//!    The identity is linked to that account, so a user who registered with a
//!    password can start using the button without a second account appearing.
//! 3. Nobody owns the address. A user is created with the same bootstrap
//!    registration performs (personal organization, default team, admin
//!    memberships), and the identity is linked to it.
//!
//! An unverified email is never matched against an existing account: the
//! provider's assertion is the only proof of ownership there is, and without
//! it anyone who can register `you@example.com` at a sloppy identity provider
//! could claim your account. The same rule blocks account creation, because a
//! `users` row is keyed by an address the framework treats as verified.

use chrono::Utc;
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use openidconnect::core::CoreIdTokenClaims;

use crate::auth::model::User;
use crate::auth::{password, token};
use crate::schema::{oauth_identities, users};

/// What a provider asserted about the person who just signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderIdentity {
    /// The `sub` claim: stable for the life of the account at the provider.
    pub(crate) subject: String,
    /// The `email` claim, normalized the way registration normalizes it.
    pub(crate) email: Option<String>,
    /// Whether the provider vouches for the address it returned.
    pub(crate) email_verified: bool,
    /// The `given_name` claim in the provider's default locale.
    pub(crate) first_name: Option<String>,
    /// The `family_name` claim in the provider's default locale.
    pub(crate) last_name: Option<String>,
}

/// Reads the claims Anubis stores from a verified ID token.
///
/// Email is trimmed and lowercased exactly as [`crate::auth::routes`]
/// normalizes a registration, so the two agree on the stored form. Names are
/// taken from the provider's default locale; a provider that returns names
/// only under a language tag leaves them empty rather than guessing.
pub(crate) fn read_claims(claims: &CoreIdTokenClaims) -> ProviderIdentity {
    let email = claims
        .email()
        .map(|email| email.trim().to_lowercase())
        .filter(|email| !email.is_empty());

    ProviderIdentity {
        subject: claims.subject().to_string(),
        email,
        email_verified: claims.email_verified().unwrap_or(false),
        first_name: claims
            .given_name()
            .and_then(|name| name.get(None))
            .map(|name| name.to_string()),
        last_name: claims
            .family_name()
            .and_then(|name| name.get(None))
            .map(|name| name.to_string()),
    }
}

/// Signs the identity in, linking or creating the account it belongs to.
///
/// Runs in one transaction, so a half-linked account can never exist.
///
/// # Errors
/// Returns [`LinkError::EmailUnavailable`] when the provider returned no
/// address for an unlinked identity, [`LinkError::EmailUnverified`] when it
/// returned one it does not vouch for, and the database or hashing error
/// otherwise.
pub(crate) async fn sign_in(
    connection: &mut AsyncPgConnection,
    provider: &str,
    identity: &ProviderIdentity,
) -> Result<User, LinkError> {
    connection
        .transaction(async |transaction| {
            let linked: Option<User> = oauth_identities::table
                .inner_join(users::table)
                .filter(oauth_identities::provider.eq(provider))
                .filter(oauth_identities::subject.eq(&identity.subject))
                .select(User::as_select())
                .first(transaction)
                .await
                .optional()?;
            if let Some(user) = linked {
                return Ok(user);
            }

            let Some(email) = identity.email.as_deref() else {
                return Err(LinkError::EmailUnavailable);
            };
            if !identity.email_verified {
                return Err(LinkError::EmailUnverified);
            }

            let existing: Option<User> = users::table
                .filter(users::email.eq(email))
                .select(User::as_select())
                .first(transaction)
                .await
                .optional()?;

            let user = match existing {
                Some(user) => user,
                None => create_user(transaction, email, identity).await?,
            };

            diesel::insert_into(oauth_identities::table)
                .values((
                    oauth_identities::user_id.eq(user.id),
                    oauth_identities::provider.eq(provider),
                    oauth_identities::subject.eq(&identity.subject),
                    oauth_identities::email.eq(email),
                ))
                .execute(transaction)
                .await?;

            Ok(user)
        })
        .await
}

/// Creates the account an unknown verified address earns.
///
/// The password column is filled with the hash of a fresh random secret
/// nobody holds: the account has no password until its owner sets one through
/// the reset flow, and the column stays `NOT NULL` for every other account.
async fn create_user(
    connection: &mut AsyncPgConnection,
    email: &str,
    identity: &ProviderIdentity,
) -> Result<User, LinkError> {
    let password_hash = password::hash(token::generate())
        .await
        .map_err(LinkError::Password)?;

    let user: User = diesel::insert_into(users::table)
        .values((
            users::email.eq(email),
            users::password_hash.eq(&password_hash),
            // The provider vouched for the address, which is the same proof
            // the verification email collects.
            users::email_verified_at.eq(Some(Utc::now())),
            users::first_name.eq(identity.first_name.as_deref()),
            users::last_name.eq(identity.last_name.as_deref()),
        ))
        .returning(User::as_returning())
        .get_result(connection)
        .await?;

    crate::tenancy::create_personal_organization(connection, &user).await?;
    Ok(user)
}

/// Why an identity could not be signed in.
#[derive(Debug)]
pub(crate) enum LinkError {
    /// The provider returned no email address for an unlinked identity.
    EmailUnavailable,
    /// The provider returned an address it does not vouch for.
    EmailUnverified,
    /// The database refused the read or the write.
    Database(diesel::result::Error),
    /// The placeholder password could not be hashed.
    Password(password::Error),
}

impl From<diesel::result::Error> for LinkError {
    fn from(error: diesel::result::Error) -> Self {
        Self::Database(error)
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use openidconnect::core::{CoreGenderClaim, CoreIdTokenClaims};
    use openidconnect::{
        Audience, EmptyAdditionalClaims, EndUserEmail, EndUserFamilyName, EndUserGivenName,
        IssuerUrl, StandardClaims, SubjectIdentifier,
    };

    use super::read_claims;

    fn claims(standard: StandardClaims<CoreGenderClaim>) -> CoreIdTokenClaims {
        CoreIdTokenClaims::new(
            IssuerUrl::new("https://accounts.example.com".to_owned()).expect("a valid issuer"),
            vec![Audience::new("client-id".to_owned())],
            Utc::now() + Duration::minutes(5),
            Utc::now(),
            standard,
            EmptyAdditionalClaims {},
        )
    }

    #[test]
    fn claims_are_read_into_the_shape_anubis_stores() {
        let identity = read_claims(&claims(
            StandardClaims::new(SubjectIdentifier::new("109876".to_owned()))
                .set_email(Some(EndUserEmail::new("  Alex@Example.COM ".to_owned())))
                .set_email_verified(Some(true))
                .set_given_name(Some(EndUserGivenName::new("Alex".to_owned()).into()))
                .set_family_name(Some(EndUserFamilyName::new("Navarro".to_owned()).into())),
        ));

        assert_eq!(identity.subject, "109876");
        assert_eq!(identity.email.as_deref(), Some("alex@example.com"));
        assert!(identity.email_verified);
        assert_eq!(identity.first_name.as_deref(), Some("Alex"));
        assert_eq!(identity.last_name.as_deref(), Some("Navarro"));
    }

    #[test]
    fn absent_claims_stay_absent_and_verification_defaults_to_false() {
        let identity = read_claims(&claims(StandardClaims::new(SubjectIdentifier::new(
            "109876".to_owned(),
        ))));

        assert_eq!(identity.subject, "109876");
        assert_eq!(identity.email, None);
        assert!(!identity.email_verified);
        assert_eq!(identity.first_name, None);
        assert_eq!(identity.last_name, None);
    }

    #[test]
    fn a_blank_email_counts_as_no_email() {
        let identity = read_claims(&claims(
            StandardClaims::new(SubjectIdentifier::new("109876".to_owned()))
                .set_email(Some(EndUserEmail::new("   ".to_owned())))
                .set_email_verified(Some(true)),
        ));

        assert_eq!(identity.email, None);
    }
}

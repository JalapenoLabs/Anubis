//! What an operator does to other people's accounts.
//!
//! [`Accounts`] carries what these acts need from the deployment (the mailer,
//! the public URL a link points at, and a gate for the one password hash an
//! operator can ask for) and is built once, beside the auth router. Its
//! invitation methods live in [`super::invitations`]; this file holds the
//! service itself and the temporary password.
//!
//! # Temporary passwords
//!
//! [`Accounts::set_temporary_password`] is for the person who cannot sign in
//! and cannot reach their inbox either. It generates a password, sets it,
//! signs the account out everywhere, and marks the account so that, once
//! signed in with it, it may do nothing but choose its own; see
//! [`crate::auth::CurrentUser`]. The password is returned once, for the
//! operator to hand over by some channel they trust, and is never stored,
//! logged, mailed, or written to the audit log.

use std::fmt::{self, Debug, Formatter};
use std::num::NonZero;

use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::audit;
use crate::auth::password::Hasher;
use crate::auth::{User, session};
use crate::config::AppConfig;
use crate::guard::PlatformMember;
use crate::http::ApiError;
use crate::mail::Mailer;
use crate::schema::users;

/// How many argon2 computations the operator surface runs at once.
///
/// One. The auth router keeps its own gate, which an application cannot reach,
/// and setting a temporary password is a rare act a handful of people perform,
/// so a single permit adds at most one computation's 19 MiB to what the
/// deployment can be made to hold, and two operators acting in the same
/// instant wait tens of milliseconds for each other.
const OPERATOR_HASH_PERMITS: NonZero<usize> = NonZero::new(1).expect("one permit is not zero");

/// The operator's tools for bringing people in and letting them back in.
///
/// Cheap to clone. Build it once from the configuration and the mailer the
/// auth router was given, and hand it to the operator routes:
///
/// ```ignore
/// let accounts = anubis::platform::Accounts::new(&config, mailer.clone());
///
/// async fn invite(
///     operator: PlatformMember,
///     context: audit::Context,
///     State(state): State<OperatorState>,
///     Json(body): Json<InviteBody>,
/// ) -> Result<impl IntoResponse, ApiError> {
///     operator.require(Action::Create, "Invitation")?;
///     let mut connection = state.pool.get().await?;
///     let invitation = state
///         .accounts
///         .invite(&mut connection, &operator, &context, InviteRequest {
///             email: &body.email,
///             platform_role: body.platform_role.as_deref(),
///         })
///         .await?;
///     Ok(Json(invitation))
/// }
/// ```
///
/// Every method takes the [`PlatformMember`] the application's guard produced,
/// which is both the proof that the caller operates the deployment and who
/// the audit log names. Which action a method asks of `roles.yml` is the
/// application's decision, made with [`PlatformMember::require`] before the
/// call, exactly as at every other tier.
#[derive(Debug, Clone)]
pub struct Accounts {
    pub(super) mailer: Mailer,
    pub(super) app_url: String,
    hasher: Hasher,
}

impl Accounts {
    /// Builds the operator's account tools from the deployment's configuration.
    ///
    /// `mailer` is the one the auth router sends through, so an invitation
    /// reaches the same transport and template mapping as every other email.
    #[must_use]
    pub fn new(config: &AppConfig, mailer: Mailer) -> Self {
        Self {
            mailer,
            app_url: config.app_url.clone(),
            hasher: Hasher::new(OPERATOR_HASH_PERMITS),
        }
    }

    /// Gives an account a generated password it must replace at its next sign-in.
    ///
    /// In one transaction, holding the account: sets the password, marks the
    /// account as required to change it, deletes every session it has, and
    /// records [`audit::PASSWORD_TEMPORARY_SET`] with an empty change set,
    /// because the fact is the whole of what an auditor needs. The account
    /// signs in with the returned password by any path a password reaches,
    /// and a confirmed second factor is still asked for.
    ///
    /// # Errors
    /// - `404` when no account has that id.
    /// - `503` when the password cannot be hashed in time.
    /// - `500` when the database fails.
    pub async fn set_temporary_password(
        &self,
        connection: &mut AsyncPgConnection,
        operator: &PlatformMember,
        context: &audit::Context,
        user_id: Uuid,
    ) -> Result<TemporaryPassword, ApiError> {
        let password = TemporaryPassword::generate();
        let password_hash = self.hasher.hash(password.reveal().to_owned()).await?;

        connection
            .transaction::<(), ApiError, _>(async |transaction| {
                let user: User = users::table
                    .find(user_id)
                    .select(User::as_select())
                    .for_update()
                    .first(transaction)
                    .await
                    .optional()?
                    .ok_or_else(ApiError::not_found)?;

                diesel::update(users::table.find(user.id))
                    .set((
                        users::password_hash.eq(&password_hash),
                        users::password_change_required.eq(true),
                    ))
                    .execute(transaction)
                    .await?;

                // Whoever holds a session for this account now may be the
                // reason it needed rescuing.
                session::delete_all_for_user(transaction, user.id).await?;

                let label = audit::person_label(
                    user.first_name.as_deref(),
                    user.last_name.as_deref(),
                    &user.email,
                );
                audit::record(
                    transaction,
                    &context.by(&operator.user),
                    &audit::Event::new(audit::PASSWORD_TEMPORARY_SET, "User")
                        .subject(user.id)
                        .label(&label),
                )
                .await?;
                Ok(())
            })
            .await?;

        Ok(password)
    }
}

/// A generated temporary password, readable once and never printed.
///
/// `Debug` renders a placeholder, so the value cannot reach a log through a
/// `{:?}` somebody adds later, and the memory is zeroed when it is dropped.
/// Read it with [`TemporaryPassword::reveal`] at the one place it is handed to
/// the operator.
pub struct TemporaryPassword(Zeroizing<String>);

impl TemporaryPassword {
    /// The characters a temporary password is drawn from.
    ///
    /// Lowercase letters and digits without `i`, `l`, `o`, `0`, and `1`,
    /// because an operator reads this aloud or types it from a screen, and
    /// those are the characters people misread.
    const ALPHABET: &[u8; 31] = b"abcdefghjkmnpqrstuvwxyz23456789";

    /// Four groups of five: about 99 bits, far past guessing, and still short
    /// enough to read out.
    const GROUPS: usize = 4;
    const GROUP_LENGTH: usize = 5;

    /// Draws a fresh password from the operating system's random source.
    ///
    /// # Panics
    /// Panics when the operating system has no random source, which is a
    /// broken host rather than a condition anything could recover from.
    fn generate() -> Self {
        // The largest multiple of the alphabet's size that fits in a byte;
        // a byte at or above it would favour the first few characters.
        const REJECTION_THRESHOLD: u8 = 248;

        let length = Self::GROUPS * Self::GROUP_LENGTH;
        let mut password = Zeroizing::new(String::with_capacity(length + Self::GROUPS));
        let mut drawn = 0;
        while drawn < length {
            let mut byte = [0u8; 1];
            getrandom::fill(&mut byte).expect("the OS random source must be available");
            if byte[0] >= REJECTION_THRESHOLD {
                continue;
            }

            if drawn > 0 && drawn % Self::GROUP_LENGTH == 0 {
                password.push('-');
            }
            let index = usize::from(byte[0]) % Self::ALPHABET.len();
            password.push(char::from(Self::ALPHABET[index]));
            drawn += 1;
        }
        Self(password)
    }

    /// The password itself, for the one response that hands it over.
    #[must_use]
    pub fn reveal(&self) -> &str {
        &self.0
    }
}

impl Debug for TemporaryPassword {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("TemporaryPassword(...)")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::TemporaryPassword;
    use crate::auth::check_password_policy;

    #[test]
    fn a_temporary_password_satisfies_the_password_policy() {
        let password = TemporaryPassword::generate();
        assert_eq!(check_password_policy(password.reveal()), Ok(()));
    }

    #[test]
    fn a_temporary_password_reads_as_four_groups_of_unambiguous_characters() {
        let password = TemporaryPassword::generate();
        let groups: Vec<&str> = password.reveal().split('-').collect();

        assert_eq!(groups.len(), TemporaryPassword::GROUPS);
        for group in groups {
            assert_eq!(group.len(), TemporaryPassword::GROUP_LENGTH);
            assert!(
                group
                    .bytes()
                    .all(|character| TemporaryPassword::ALPHABET.contains(&character)),
                "{group:?} holds a character outside the alphabet",
            );
        }
    }

    #[test]
    fn temporary_passwords_do_not_repeat() {
        let drawn: HashSet<String> = (0..64)
            .map(|_| TemporaryPassword::generate().reveal().to_owned())
            .collect();
        assert_eq!(drawn.len(), 64);
    }

    #[test]
    fn debug_output_never_carries_the_password() {
        let password = TemporaryPassword::generate();
        let rendered = format!("{password:?}");

        assert_eq!(rendered, "TemporaryPassword(...)");
        assert!(!rendered.contains(password.reveal()));
    }
}

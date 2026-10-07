//! Invitations to open an account on the deployment, sent by an operator.
//!
//! An invitation is a row, not an account. The account is created when the
//! invitee accepts, by choosing a password, and never before: an address that
//! nobody answers for leaves no account, no personal organization, and no
//! placeholder password behind, and until it is accepted every other path
//! (registering, an emailed code, an OpenID Connect provider) treats the
//! address exactly as it treats one nobody invited. The alternative, creating
//! the account up front, would need a password nobody holds and would make
//! "pending" a property of an account rather than of an invitation, which is
//! what a list of pending invitations then has to filter millions of accounts
//! to find.
//!
//! The token discipline is the framework's: 256 random bits in the emailed
//! link, its SHA-256 at rest, single use. A resend replaces the hash, so the
//! previous link stops working the moment the new one is issued. A link lives
//! [`INVITATION_TTL_HOURS`].
//!
//! Every operator act here is audited with the tenancy invitation verbs
//! ([`audit::INVITATION_CREATED`], [`audit::INVITATION_RESENT`],
//! [`audit::INVITATION_REVOKED`], [`audit::INVITATION_CLAIMED`]) on the subject
//! type `PlatformInvitation`, and on neither a team nor an organization.

use chrono::{DateTime, Duration, Utc};
use diesel::prelude::*;
use diesel::result::DatabaseErrorKind;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use uuid::Uuid;

use crate::audit;
use crate::auth::model::NewUser;
use crate::auth::{User, normalize_email, token};
use crate::guard::PlatformMember;
use crate::http::{ApiError, ListParams, Pagination};
use crate::mail::{Email, EmailKind};
use crate::platform::{Accounts, record_grant};
use crate::roles::Scope;
use crate::schema::{platform_invitations, users};
use crate::tenancy;

/// How long an invitation link works, in hours.
///
/// A day: long enough to reach somebody in another time zone, short enough
/// that a forwarded or leaked link is not a standing way into the deployment.
/// An operator resends rather than waiting on a longer link.
pub const INVITATION_TTL_HOURS: i64 = 24;

/// The subject type every platform invitation audit row names.
const SUBJECT_TYPE: &str = "PlatformInvitation";

/// An invitation an operator sent, as stored.
///
/// The token is never part of it: the row holds only the token's hash, and
/// nothing reads that hash back out.
#[derive(Debug, Clone, PartialEq, Eq, Queryable, Selectable, Serialize)]
#[diesel(table_name = platform_invitations)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct PlatformInvitation {
    /// Primary key, which is what resending and revoking name.
    pub id: Uuid,
    /// The normalized address the invitation was sent to.
    pub email: String,
    /// The platform role the account is granted when it is created, if any.
    pub platform_role: Option<String>,
    /// The operator who sent it, while their account exists.
    pub invited_by: Option<Uuid>,
    /// How that operator read when they sent it, kept after they are gone.
    pub invited_by_name: String,
    /// When it was first sent.
    pub created_at: DateTime<Utc>,
    /// When the current link went out; a resend moves it.
    pub sent_at: DateTime<Utc>,
    /// When the current link stops working.
    pub expires_at: DateTime<Utc>,
    /// When the invitee accepted it, creating the account.
    pub accepted_at: Option<DateTime<Utc>>,
    /// The account accepting it created, while that account exists.
    pub user_id: Option<Uuid>,
    /// When an operator withdrew it.
    pub revoked_at: Option<DateTime<Utc>>,
}

impl PlatformInvitation {
    /// Whether the current link has stopped working.
    ///
    /// An expired invitation is still pending: nobody answered it, and a
    /// resend is what revives it.
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.expires_at <= Utc::now()
    }
}

/// Who to invite, and whether they arrive holding a platform role.
#[derive(Debug, Clone, Copy)]
pub struct InviteRequest<'a> {
    /// The address to invite, normalized before anything reads it.
    pub email: &'a str,
    /// A platform role to grant when the account is created, or `None` for an
    /// ordinary account. It must be a role `roles.yml` lets the platform tier
    /// grant.
    pub platform_role: Option<&'a str>,
}

/// One page of the invitations nobody has answered yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingInvitations {
    /// The invitations on this page, most recently sent first.
    pub invitations: Vec<PlatformInvitation>,
    /// Where this page sits in the whole list.
    pub pagination: Pagination,
}

/// Lists the invitations neither accepted nor revoked, most recently sent first.
///
/// Expired invitations are included and say so through
/// [`PlatformInvitation::is_expired`]: nobody answered them, and the operator
/// reading the list is the one who decides to resend or revoke. The query
/// reads a partial index holding only these rows, so it costs the number of
/// people still invited, not the number of accounts.
///
/// # Errors
/// Returns the query error when the database refuses the read.
pub async fn pending_invitations(
    connection: &mut AsyncPgConnection,
    params: &ListParams,
) -> QueryResult<PendingInvitations> {
    let total_items: i64 = platform_invitations::table
        .filter(platform_invitations::accepted_at.is_null())
        .filter(platform_invitations::revoked_at.is_null())
        .count()
        .get_result(connection)
        .await?;

    let invitations = platform_invitations::table
        .filter(platform_invitations::accepted_at.is_null())
        .filter(platform_invitations::revoked_at.is_null())
        .order((
            platform_invitations::sent_at.desc(),
            platform_invitations::id.desc(),
        ))
        .limit(params.limit())
        .offset(params.offset())
        .select(PlatformInvitation::as_select())
        .load(connection)
        .await?;

    Ok(PendingInvitations {
        invitations,
        pagination: Pagination::new(params, total_items),
    })
}

impl Accounts {
    /// Invites an address to open an account, optionally holding a platform role.
    ///
    /// Writes the invitation and its audit record in one transaction, then
    /// mails the link as [`EmailKind::PlatformInvitation`]. The mail goes out
    /// after the commit, so no row lock is held across a network call; a
    /// delivery failure is logged rather than undoing the invitation, and the
    /// operator resends.
    ///
    /// # Errors
    /// - `400` when the address cannot be one, or the role is not one the
    ///   platform tier may grant.
    /// - `409` when an account already holds the address, or an invitation to
    ///   it is still pending (resend that one instead).
    /// - `500` when the database fails.
    pub async fn invite(
        &self,
        connection: &mut AsyncPgConnection,
        operator: &PlatformMember,
        context: &audit::Context,
        request: InviteRequest<'_>,
    ) -> Result<PlatformInvitation, ApiError> {
        let email = normalize_email(request.email)
            .ok_or_else(|| ApiError::validation("Enter a valid email address."))?;
        if let Some(role) = request.platform_role
            && !operator.roles().is_grantable_at(role, Scope::Platform)
        {
            tracing::debug!(
                platform.role = role,
                "refused an invitation granting {{platform.role}}, which the platform cannot grant",
            );
            return Err(ApiError::validation(
                "That role cannot be granted to an account.",
            ));
        }

        let inviter = &operator.user;
        let inviter_name = audit::person_label(
            inviter.first_name.as_deref(),
            inviter.last_name.as_deref(),
            &inviter.email,
        );
        let raw_token = token::generate();
        let sent_at = Utc::now();

        let invitation = connection
            .transaction::<PlatformInvitation, ApiError, _>(async |transaction| {
                let taken: bool = diesel::select(diesel::dsl::exists(
                    users::table.filter(users::email.eq(&email)),
                ))
                .get_result(transaction)
                .await?;
                if taken {
                    return Err(ApiError::conflict(
                        "That email address already has an account.",
                    ));
                }

                let invitation: PlatformInvitation =
                    diesel::insert_into(platform_invitations::table)
                        .values((
                            platform_invitations::email.eq(&email),
                            platform_invitations::platform_role.eq(request.platform_role),
                            platform_invitations::invited_by.eq(inviter.id),
                            platform_invitations::invited_by_name.eq(&inviter_name),
                            platform_invitations::token_hash.eq(token::hash(&raw_token)),
                            platform_invitations::sent_at.eq(sent_at),
                            platform_invitations::expires_at
                                .eq(sent_at + Duration::hours(INVITATION_TTL_HOURS)),
                        ))
                        .returning(PlatformInvitation::as_returning())
                        .get_result(transaction)
                        .await
                        .map_err(|error| match error {
                            // The live-address index deciding two invites
                            // that raced: the loser reads the same sentence
                            // the check below would have given it.
                            diesel::result::Error::DatabaseError(
                                DatabaseErrorKind::UniqueViolation,
                                _details,
                            ) => ApiError::conflict(
                                "That address already has a pending invitation. Resend it instead.",
                            ),
                            other => ApiError::from(other),
                        })?;

                let mut changes =
                    audit::Changes::new().field("email", serde_json::Value::Null, email.as_str());
                if let Some(role) = request.platform_role {
                    changes = changes.field("platform_role", serde_json::Value::Null, role);
                }
                audit::record(
                    transaction,
                    &context.by(inviter),
                    &audit::Event::new(audit::INVITATION_CREATED, SUBJECT_TYPE)
                        .subject(invitation.id)
                        .label(&email)
                        .changes(changes),
                )
                .await?;

                Ok(invitation)
            })
            .await?;

        self.mail_invitation(&invitation, &raw_token).await;
        Ok(invitation)
    }

    /// Sends a pending invitation again, with a fresh link and a fresh day.
    ///
    /// The previous link stops working in the same write that issues the new
    /// one, so at most one link per invitation is ever live. The row is held
    /// for the decision, so a resend racing an acceptance or a revocation sees
    /// the outcome rather than reviving it.
    ///
    /// # Errors
    /// - `404` when no invitation has that id.
    /// - `409` when it was accepted or revoked, or when an account holds the
    ///   address now (somebody registered it meanwhile).
    /// - `500` when the database fails.
    pub async fn resend_invitation(
        &self,
        connection: &mut AsyncPgConnection,
        operator: &PlatformMember,
        context: &audit::Context,
        invitation_id: Uuid,
    ) -> Result<PlatformInvitation, ApiError> {
        let raw_token = token::generate();
        let sent_at = Utc::now();

        let invitation = connection
            .transaction::<PlatformInvitation, ApiError, _>(async |transaction| {
                let held = hold_pending(transaction, invitation_id).await?;

                let taken: bool = diesel::select(diesel::dsl::exists(
                    users::table.filter(users::email.eq(&held.email)),
                ))
                .get_result(transaction)
                .await?;
                if taken {
                    return Err(ApiError::conflict(
                        "That address has an account now. Revoke the invitation instead.",
                    ));
                }

                let invitation: PlatformInvitation =
                    diesel::update(platform_invitations::table.find(held.id))
                        .set((
                            platform_invitations::token_hash.eq(token::hash(&raw_token)),
                            platform_invitations::sent_at.eq(sent_at),
                            platform_invitations::expires_at
                                .eq(sent_at + Duration::hours(INVITATION_TTL_HOURS)),
                        ))
                        .returning(PlatformInvitation::as_returning())
                        .get_result(transaction)
                        .await?;

                audit::record(
                    transaction,
                    &context.by(&operator.user),
                    &audit::Event::new(audit::INVITATION_RESENT, SUBJECT_TYPE)
                        .subject(invitation.id)
                        .label(&invitation.email)
                        .changes(audit::Changes::new().field(
                            "expires_at",
                            held.expires_at.to_rfc3339(),
                            invitation.expires_at.to_rfc3339(),
                        )),
                )
                .await?;

                Ok(invitation)
            })
            .await?;

        self.mail_invitation(&invitation, &raw_token).await;
        Ok(invitation)
    }

    /// Withdraws a pending invitation, so its link stops working.
    ///
    /// The row is kept and marked rather than deleted, so a later resend of
    /// the same id is refused with a sentence rather than answered `404`, and
    /// the address is free to be invited again at once.
    ///
    /// # Errors
    /// - `404` when no invitation has that id.
    /// - `409` when it was already accepted or revoked.
    /// - `500` when the database fails.
    pub async fn revoke_invitation(
        &self,
        connection: &mut AsyncPgConnection,
        operator: &PlatformMember,
        context: &audit::Context,
        invitation_id: Uuid,
    ) -> Result<PlatformInvitation, ApiError> {
        connection
            .transaction::<PlatformInvitation, ApiError, _>(async |transaction| {
                let held = hold_pending(transaction, invitation_id).await?;

                let invitation: PlatformInvitation =
                    diesel::update(platform_invitations::table.find(held.id))
                        .set(platform_invitations::revoked_at.eq(Utc::now()))
                        .returning(PlatformInvitation::as_returning())
                        .get_result(transaction)
                        .await?;

                audit::record(
                    transaction,
                    &context.by(&operator.user),
                    &audit::Event::new(audit::INVITATION_REVOKED, SUBJECT_TYPE)
                        .subject(invitation.id)
                        .label(&invitation.email),
                )
                .await?;

                Ok(invitation)
            })
            .await
    }

    /// Mails the link for a token already written, logging a failed delivery.
    async fn mail_invitation(&self, invitation: &PlatformInvitation, raw_token: &str) {
        let link = format!("{}/accept-invitation?token={raw_token}", self.app_url);
        let mail = Email::new(
            EmailKind::PlatformInvitation,
            invitation.email.clone(),
            "You're invited",
            format!(
                "{} invited you to open an account.\n\n\
                 Choose your password within {INVITATION_TTL_HOURS} hours: {link}\n\n\
                 If you weren't expecting this, ignore this email.",
                invitation.invited_by_name,
            ),
        )
        .with_param("link", &link)
        .with_param("inviter", &invitation.invited_by_name)
        .with_param("hours", INVITATION_TTL_HOURS.to_string());

        if let Err(error) = self.mailer.send(mail).await {
            tracing::error!(
                error.message = %error,
                invitation.id = %invitation.id,
                "failed to deliver platform invitation {{invitation.id}}: {{error.message}}",
            );
        }
    }
}

/// Finds the live invitation a raw token belongs to, if it has one.
///
/// Live means pending and unexpired. Unknown, used, revoked, and expired
/// tokens all answer `None`, so the routes built on this cannot tell a caller
/// which of the four a token was.
///
/// # Errors
/// Returns the query error when the database refuses the read.
pub(crate) async fn live_invitation(
    connection: &mut AsyncPgConnection,
    raw_token: &str,
) -> QueryResult<Option<PlatformInvitation>> {
    let token_hash = token::hash(raw_token);
    live_by_hash(&token_hash, Utc::now())
        .select(PlatformInvitation::as_select())
        .first(connection)
        .await
        .optional()
}

/// What an invitee chose while accepting.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Acceptance<'a> {
    pub(crate) password_hash: &'a str,
    pub(crate) time_zone: Option<&'a str>,
    pub(crate) locale: Option<&'a str>,
}

/// Accepts the invitation behind `raw_token`, creating its account.
///
/// Runs on the caller's connection so the handler can issue the session in
/// the same transaction. The account is created the way the initial operator
/// is: email verified, because opening the link proved the address, and a
/// personal organization bootstrapped exactly as registration does it. The
/// invitation's role is granted and audited as a grant, and the acceptance is
/// audited as [`audit::INVITATION_CLAIMED`], both attributed to the new
/// account.
///
/// # Errors
/// - `400` when the token is unknown, used, revoked, or expired, all alike.
/// - `409` when an account took the address after the invitation was sent.
/// - `500` when the database fails.
pub(crate) async fn accept_invitation(
    connection: &mut AsyncPgConnection,
    context: &audit::Context,
    raw_token: &str,
    acceptance: Acceptance<'_>,
) -> Result<User, ApiError> {
    // Held so an acceptance racing a resend or a revocation is decided once:
    // whichever commits first, the other reads the outcome.
    let token_hash = token::hash(raw_token);
    let invitation: PlatformInvitation = live_by_hash(&token_hash, Utc::now())
        .select(PlatformInvitation::as_select())
        .for_update()
        .first(connection)
        .await
        .optional()?
        .ok_or_else(|| ApiError::validation(INVITATION_UNUSABLE))?;

    let platform_roles: Vec<String> = invitation.platform_role.iter().cloned().collect();
    let user: User = diesel::insert_into(users::table)
        .values((
            // A preference left out inserts the column default, the same rule
            // registration applies.
            NewUser {
                email: &invitation.email,
                password_hash: acceptance.password_hash,
                time_zone: acceptance.time_zone,
                locale: acceptance.locale,
            },
            users::email_verified_at.eq(Utc::now()),
            users::platform_roles.eq(&platform_roles),
        ))
        .returning(User::as_returning())
        .get_result(connection)
        .await
        .map_err(|error| match error {
            diesel::result::Error::DatabaseError(DatabaseErrorKind::UniqueViolation, _details) => {
                ApiError::conflict("That email address already has an account. Sign in instead.")
            }
            other => ApiError::from(other),
        })?;

    tenancy::create_personal_organization(connection, &user).await?;

    diesel::update(platform_invitations::table.find(invitation.id))
        .set((
            platform_invitations::accepted_at.eq(Utc::now()),
            platform_invitations::user_id.eq(user.id),
        ))
        .execute(connection)
        .await?;

    let actor = context.by(&user);
    if !platform_roles.is_empty() {
        record_grant(connection, &actor, &user, &[], &platform_roles).await?;
    }
    audit::record(
        connection,
        &actor,
        &audit::Event::new(audit::INVITATION_CLAIMED, SUBJECT_TYPE)
            .subject(invitation.id)
            .label(&invitation.email),
    )
    .await?;

    Ok(user)
}

/// The one sentence a token that cannot be used is refused with.
///
/// Unknown, used, revoked, and expired all read the same, from both the lookup
/// and the acceptance, so neither route says which of the four a token was.
pub(crate) const INVITATION_UNUSABLE: &str =
    "That invitation link is invalid or has expired. Ask for a new one.";

/// The pending, unexpired invitation a token hash names.
///
/// One definition for the lookup and the acceptance, so the two can never
/// disagree about which tokens are live.
#[diesel::dsl::auto_type(no_type_alias)]
fn live_by_hash<'hash>(token_hash: &'hash str, now: DateTime<Utc>) -> _ {
    platform_invitations::table
        .filter(platform_invitations::token_hash.eq(token_hash))
        .filter(platform_invitations::accepted_at.is_null())
        .filter(platform_invitations::revoked_at.is_null())
        .filter(platform_invitations::expires_at.gt(now))
}

/// Locks a pending invitation for an operator's decision about it.
async fn hold_pending(
    connection: &mut AsyncPgConnection,
    invitation_id: Uuid,
) -> Result<PlatformInvitation, ApiError> {
    let held: PlatformInvitation = platform_invitations::table
        .find(invitation_id)
        .select(PlatformInvitation::as_select())
        .for_update()
        .first(connection)
        .await
        .optional()?
        .ok_or_else(ApiError::not_found)?;

    if held.accepted_at.is_some() {
        return Err(ApiError::conflict("That invitation was already accepted."));
    }
    if held.revoked_at.is_some() {
        return Err(ApiError::conflict(
            "That invitation was revoked. Send a new one.",
        ));
    }
    Ok(held)
}

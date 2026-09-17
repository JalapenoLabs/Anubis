//! The platform tier: the deployment itself, above every organization in it.
//!
//! The tenancy tiers each have a roster, so an administrator is appointed by
//! somebody who is already one. The platform has none, which leaves a deploy
//! with nobody to make the first operator. Two environment variables close
//! that loop: `ANUBIS_INITIAL_ADMIN_EMAIL` names the account and
//! `ANUBIS_INITIAL_ADMIN_PASSWORD` is what the boot gives it if it has to
//! create one. [`ensure_initial_admin`] is what applies them, and applications
//! call it once, right after their migrations.
//!
//! ```ignore
//! db::run_app_migrations(database.url(), APP_MIGRATIONS).await?;
//! let pool = db::connect(database.url()).await?;
//! anubis::platform::ensure_initial_admin(&pool, &config, &roles).await?;
//! ```
//!
//! What a platform role *grants* is `roles.yml`'s business exactly as it is at
//! every other tier, and [`crate::guard::PlatformMember`] is what a handler
//! asks. This module only settles who holds one.
//!
//! # The seed is a grant, never a reset
//!
//! Running it is idempotent and narrow, so leaving both variables set in a
//! deployment is safe:
//!
//! - The address has no account: one is created, with its email already
//!   verified (the deployment vouched for it, and there is nobody to click a
//!   link), its personal organization bootstrapped exactly as registration
//!   does it, and the [`OPERATOR_ROLE`] granted.
//! - The address has an account without the role: the role is granted. The
//!   password is **not** touched, so the variable cannot quietly become a
//!   standing override of somebody's chosen password, and a stolen deployment
//!   config does not become a way into an existing account.
//! - The address has an account with the role: nothing happens, and nothing is
//!   written.
//!
//! Revoking is deliberately not here. Removing the variables leaves the
//! operator in place, because a role the deployment granted is still a role
//! somebody holds, and a boot that silently demoted the only operator would
//! lock the deployment out of itself.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};

use diesel::prelude::*;
use diesel_async::pooled_connection::deadpool::PoolError;
use diesel_async::{AsyncConnection, RunQueryDsl};

use crate::auth::password::Hasher;
use crate::auth::{User, password};
use crate::config::{AppConfig, InitialAdminConfig};
use crate::db::DbPool;
use crate::roles::{OPERATOR_ROLE, RoleSet, Scope};
use crate::schema::users;
use crate::{audit, tenancy};

/// Grants the configured account the platform operator role, creating it once.
///
/// Does nothing when the deployment names no initial admin, which is every
/// deployment that appoints its operators some other way. See the module docs
/// for exactly what each of the three states does.
///
/// # Errors
/// Returns an [`Error`] when `roles.yml` defines no platform-grantable
/// [`OPERATOR_ROLE`], when the password cannot be hashed, or when the database
/// is unreachable. All three stop the boot, because a deployment that asked
/// for an operator and did not get one is a deployment nobody can administer.
pub async fn ensure_initial_admin(
    pool: &DbPool,
    config: &AppConfig,
    roles: &RoleSet,
) -> Result<(), Error> {
    let Some(initial_admin) = config.initial_admin.as_ref() else {
        return Ok(());
    };

    // A key the file never defines would be a grant that admits nobody, and
    // the guard would answer 404 to the very account the deployment meant to
    // let in. Saying so at boot beats discovering it at the sign-in screen.
    if !roles.is_grantable_at(OPERATOR_ROLE, Scope::Platform) {
        return Err(Error::new(format!(
            "config/roles.yml defines no {OPERATOR_ROLE:?} role grantable at the platform tier, \
             so ANUBIS_INITIAL_ADMIN_EMAIL has nothing to grant. Add it:\n\
             \n  \
             {OPERATOR_ROLE}:\n    \
             scopes: [platform]\n    \
             models: {{}}\n"
        )));
    }

    let hasher = Hasher::new(config.password_hash_concurrency);
    seed(pool, initial_admin, &hasher).await
}

/// Applies the seed, having established that the role exists.
async fn seed(
    pool: &DbPool,
    initial_admin: &InitialAdminConfig,
    hasher: &Hasher,
) -> Result<(), Error> {
    let email = initial_admin.email();
    let mut connection = pool
        .get()
        .await
        .map_err(|source| Error::unreachable(&source))?;

    let existing: Option<User> = users::table
        .filter(users::email.eq(email))
        .select(User::as_select())
        .first(&mut connection)
        .await
        .optional()
        .map_err(|source| Error::query(&source))?;

    if let Some(user) = existing {
        if user.platform_roles.iter().any(|role| role == OPERATOR_ROLE) {
            tracing::debug!(
                user.email = email,
                "the initial admin already operates this deployment",
            );
            return Ok(());
        }

        let mut granted = user.platform_roles.clone();
        granted.push(OPERATOR_ROLE.to_owned());

        connection
            .transaction::<(), diesel::result::Error, _>(async |transaction| {
                diesel::update(users::table.find(user.id))
                    .set(users::platform_roles.eq(&granted))
                    .execute(transaction)
                    .await?;

                record_grant(transaction, &user, &user.platform_roles, &granted).await
            })
            .await
            .map_err(|source| Error::query(&source))?;

        tracing::warn!(
            user.email = email,
            platform.role = OPERATOR_ROLE,
            "granted the platform {{platform.role}} role to {{user.email}}",
        );
        return Ok(());
    }

    // Hashing is the slow part and it needs no transaction, so it happens
    // before one is opened rather than holding a connection for a second.
    let password_hash = hasher
        .hash(initial_admin.password().to_owned())
        .await
        .map_err(|source| Error::hash(&source))?;

    let created: User = connection
        .transaction(async |transaction| {
            let user: User = diesel::insert_into(users::table)
                .values((
                    users::email.eq(email),
                    users::password_hash.eq(&password_hash),
                    // The deployment vouched for the address, and there is no
                    // inbox to click a link in before the first operator can
                    // sign in.
                    users::email_verified_at.eq(chrono::Utc::now()),
                    users::platform_roles.eq(vec![OPERATOR_ROLE.to_owned()]),
                ))
                .returning(User::as_returning())
                .get_result(transaction)
                .await?;

            // The same bootstrap registration runs, so an operator's account
            // is an ordinary account that also operates: it has a personal
            // organization, and every screen a tenant member sees works.
            tenancy::create_personal_organization(transaction, &user).await?;

            record_grant(transaction, &user, &[], &user.platform_roles).await?;

            Ok::<User, diesel::result::Error>(user)
        })
        .await
        .map_err(|source| Error::query(&source))?;

    tracing::warn!(
        user.email = created.email,
        platform.role = OPERATOR_ROLE,
        "created {{user.email}} and granted it the platform {{platform.role}} \
         role; sign in and change the password",
    );
    Ok(())
}

/// Records the grant on the account it changed.
///
/// A platform role is account-level, so the row names neither a team nor an
/// organization, exactly as a password change does. The actor is the system:
/// the deployment's configuration is what granted this, not a person.
async fn record_grant(
    connection: &mut diesel_async::AsyncPgConnection,
    user: &User,
    before: &[String],
    after: &[String],
) -> Result<(), diesel::result::Error> {
    let label = audit::person_label(
        user.first_name.as_deref(),
        user.last_name.as_deref(),
        &user.email,
    );

    audit::record(
        connection,
        &audit::Context::system(),
        &audit::Event::new(PLATFORM_ROLES_CHANGED, "User")
            .subject(user.id)
            .label(&label)
            .changes(audit::Changes::new().field(
                "platform_roles",
                before.join(", "),
                after.join(", "),
            )),
    )
    .await?;
    Ok(())
}

/// Audit action recorded when an account's platform roles change.
pub const PLATFORM_ROLES_CHANGED: &str = "platform.roles_changed";

/// A failure to seed the configured operator account.
#[derive(Debug)]
pub struct Error {
    message: String,
    backtrace: Backtrace,
}

impl Error {
    fn new(message: String) -> Self {
        Self {
            message,
            backtrace: Backtrace::capture(),
        }
    }

    fn unreachable(source: &PoolError) -> Self {
        Self::new(format!("failed to reach the database: {source}"))
    }

    fn query(source: &diesel::result::Error) -> Self {
        Self::new(format!("failed to seed the initial admin: {source}"))
    }

    fn hash(source: &password::Error) -> Self {
        Self::new(format!(
            "failed to hash ANUBIS_INITIAL_ADMIN_PASSWORD: {source}"
        ))
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

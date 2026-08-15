//! The account a deployment with nobody in it is opened with.
//!
//! A fresh deployment has no seeded way in. With registration open and every
//! account getting a tenancy of its own, the first person to sign up
//! administers what they land in, which works by accident. Close registration
//! ([`crate::auth::registration`]) or point signup at one shared organization
//! ([`crate::tenancy::BootstrapMode`]) and the accident stops working: nobody
//! may register, and nobody holds [`ADMIN_ROLE`] anywhere to send the first
//! invitation.
//!
//! [`seed_first_administrator`] is that seed. An application calls it at
//! startup, after its migrations and before it serves, and it does nothing at
//! all unless `ANUBIS_BOOTSTRAP_ADMIN_EMAIL` and
//! `ANUBIS_BOOTSTRAP_ADMIN_PASSWORD` are both set *and* the deployment holds no
//! users. Those two conditions are the whole design:
//!
//! - **Both variables, or neither.** Configuration refuses to load a half of
//!   the pair, so a deployment never boots believing it has a way in that was
//!   never created. See [`crate::config`].
//! - **No users at all.** Emptiness is the one condition under which handing
//!   out an administrator gives nothing away, and it is decided under
//!   [`SEED_LOCK_KEY`] in the transaction that acts on it, so two instances
//!   booting together seed one administrator rather than racing. A deployment
//!   that already has accounts is left alone, loudly, because a variable that
//!   is inert is worth learning about before it is trusted.
//!
//! The account is created verified, since nothing can deliver a verification
//! email to a deployment nobody can sign into yet, and it goes through
//! [`bootstrap_account`], the same function both signup paths call, so it lands
//! where this deployment puts accounts rather than somewhere the seed invented.
//! It is granted [`ADMIN_ROLE`] where it lands, because inviting everybody else
//! is the entire reason it exists.
//!
//! The password is a provisioning credential: it was typed into a deployment
//! manifest, it is readable by whoever can read the environment, and it must
//! not survive as a credential. The account is therefore created already owing
//! a password change ([`crate::auth::account_status`]), so the first sign-in
//! can do exactly one thing, and that is replace it.
//!
//! Registration mode is deliberately not consulted. It governs who may create
//! an account through the API; this is the deployment creating its own first
//! account from its own configuration, and a closed deployment is precisely the
//! one that needs it.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};

use chrono::Utc;
use diesel::prelude::*;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use uuid::Uuid;

use crate::auth::User;
use crate::auth::password::Hasher;
use crate::config::AppConfig;
use crate::db::DbPool;
use crate::schema::{organization_memberships, team_memberships, users};
use crate::tenancy::bootstrap::{ADMIN_ROLE, bootstrap_account};
use crate::tenancy::model::{Organization, Team};

/// The advisory lock emptiness is decided and acted on under.
///
/// Two instances booting together would each find no users and each seed an
/// administrator, and the winner of that race is decided by a unique index
/// rather than by design: one instance would fail its boot on the email it was
/// told to create. A transaction-scoped lock orders them instead, so the second
/// instance asks its question after the first one's answer has committed and
/// finds the deployment populated.
///
/// The value is arbitrary but must never change: it is the name concurrent
/// transactions agree on, and a different value is a different lock. It follows
/// the migration lock documented at [`crate::db`] in the framework's own
/// series, the ASCII bytes of `ANUBIS` followed by a counter.
const SEED_LOCK_KEY: i64 = 0x414E_5542_4953_0003;

/// Creates the first administrator when the deployment is empty and configured.
///
/// Call it once at startup, after the migrations have applied and before the
/// server takes traffic:
///
/// ```ignore
/// anubis::db::run_pending_migrations(database.url()).await?;
/// let pool = anubis::db::connect(database.url()).await?;
/// anubis::tenancy::seed_first_administrator(&pool, &config).await?;
/// ```
///
/// A no-op unless [`AppConfig::bootstrap_admin`] is set and the `users` table
/// is empty, and safe to call from every instance of a deployment: see the
/// module docs and [`SEED_LOCK_KEY`]. What it did, or did not do and why, is
/// announced in the log.
///
/// # Errors
/// Returns an [`Error`] when the password cannot be hashed, or when the
/// database refuses the read or the write.
pub async fn seed_first_administrator(pool: &DbPool, config: &AppConfig) -> Result<(), Error> {
    let Some(administrator) = config.bootstrap_admin.as_ref() else {
        return Ok(());
    };

    // Hashed before a connection is taken: argon2 holds a CPU for tens of
    // milliseconds, and a pooled connection idling through that is one the rest
    // of the boot cannot use.
    let password_hash = Hasher::new(config.password_hash_concurrency)
        .hash(administrator.password().to_owned())
        .await
        .map_err(|source| {
            Error::new(
                "failed to hash the bootstrap administrator's password",
                source,
            )
        })?;

    let mut connection = pool.get().await.map_err(|source| {
        Error::new(
            "failed to reach the database to seed an administrator",
            source,
        )
    })?;

    let seeded = connection
        .transaction::<Option<Seeded>, diesel::result::Error, _>(async |transaction| {
            take_seed_lock(transaction).await?;

            // Asked under the lock and acted on inside the same transaction, so
            // "the deployment is empty" cannot stop being true between the two.
            let accounts: i64 = users::table.count().get_result(transaction).await?;
            if accounts > 0 {
                return Ok(None);
            }

            let user: User = diesel::insert_into(users::table)
                .values((
                    users::email.eq(administrator.email()),
                    users::password_hash.eq(&password_hash),
                    // Verified on creation: a verification email is delivered to
                    // somebody who can already reach the deployment, and nobody
                    // can reach this one yet.
                    users::email_verified_at.eq(Some(Utc::now())),
                    // And owing a change immediately, because the password came
                    // from the environment and a provisioning credential must
                    // not stay a credential.
                    users::password_change_required.eq(true),
                ))
                .returning(User::as_returning())
                .get_result(transaction)
                .await?;

            let (organization, team) =
                bootstrap_account(transaction, &config.bootstrap, &user).await?;
            grant_admin(transaction, &organization, &team, user.id).await?;

            Ok(Some(Seeded {
                organization: organization.name,
                team: team.name,
            }))
        })
        .await
        .map_err(|source| Error::new("failed to seed the first administrator", source))?;

    announce(administrator.email(), seeded.as_ref());
    Ok(())
}

/// What a seeding pass created, for the line it logs.
#[derive(Debug)]
struct Seeded {
    organization: String,
    team: String,
}

/// Says plainly what happened, at a level that survives a production filter.
///
/// Both outcomes are warnings. A seeded deployment is holding a provisioning
/// credential in its environment until somebody signs in and replaces it, and
/// a deployment that seeded nothing is running with variables its operator
/// believes did something.
///
/// The address is named, which addresses in logs normally are not. This one is
/// not a person's data the application happens to hold: it is a line of this
/// deployment's own configuration, written by whoever reads this log, and
/// naming it is what makes both sentences act on the right account. The
/// password is never logged, here or anywhere.
fn announce(email: &str, seeded: Option<&Seeded>) {
    let Some(created) = seeded else {
        tracing::warn!(
            tenancy.administrator.email = email,
            "ANUBIS_BOOTSTRAP_ADMIN_EMAIL is set on a deployment that already has accounts, so \
             no administrator was seeded and {{tenancy.administrator.email}} was not created. \
             The variables only apply to a deployment with no users at all; remove them.",
        );
        return;
    };

    tracing::warn!(
        tenancy.administrator.email = email,
        tenancy.administrator.organization = created.organization,
        tenancy.administrator.team = created.team,
        "seeded the first administrator {{tenancy.administrator.email}} as admin of organization \
         {{tenancy.administrator.organization}} and team {{tenancy.administrator.team}}, with its \
         email verified. Sign in and change the password: ANUBIS_BOOTSTRAP_ADMIN_PASSWORD is a \
         provisioning credential, and the account can do nothing else until it is replaced. \
         Remove both variables afterwards.",
    );
}

/// Takes [`SEED_LOCK_KEY`] for the rest of the caller's transaction.
///
/// The key is a compile-time integer constant, so rendering it into the
/// statement carries no input to inject.
async fn take_seed_lock(connection: &mut AsyncPgConnection) -> Result<(), diesel::result::Error> {
    diesel::sql_query(format!("SELECT pg_advisory_xact_lock({SEED_LOCK_KEY})"))
        .execute(connection)
        .await
        .map(|_rows| ())
}

/// Makes the seeded account an admin of the tenancy it landed in.
///
/// [`bootstrap_account`] decides *where* the account lands, and normally leaves
/// it an admin already, since whoever creates an organization administers it.
/// This decides what it holds there, unconditionally: under
/// [`BootstrapMode::Shared`](crate::tenancy::BootstrapMode::Shared) an
/// organization that somehow already exists would hand a joining account the
/// baseline role instead, and an administrator who cannot administer is the one
/// outcome this whole path exists to prevent.
async fn grant_admin(
    connection: &mut AsyncPgConnection,
    organization: &Organization,
    team: &Team,
    user: Uuid,
) -> Result<(), diesel::result::Error> {
    let admin_roles = vec![ADMIN_ROLE.to_owned()];

    diesel::update(
        organization_memberships::table
            .filter(organization_memberships::organization_id.eq(organization.id))
            .filter(organization_memberships::user_id.eq(user)),
    )
    .set(organization_memberships::roles.eq(&admin_roles))
    .execute(connection)
    .await?;

    diesel::update(
        team_memberships::table
            .filter(team_memberships::team_id.eq(team.id))
            .filter(team_memberships::user_id.eq(Some(user))),
    )
    .set(team_memberships::roles.eq(&admin_roles))
    .execute(connection)
    .await?;

    Ok(())
}

/// The first administrator could not be seeded.
#[derive(Debug)]
pub struct Error {
    context: &'static str,
    source: Box<dyn std::error::Error + Send + Sync>,
    backtrace: Backtrace,
}

impl Error {
    fn new(context: &'static str, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            context,
            source: Box::new(source),
            backtrace: Backtrace::capture(),
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.context, self.source)?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

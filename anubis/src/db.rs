//! Database connectivity for Anubis applications.
//!
//! Connections go through [`diesel_async`]'s tokio-postgres backend, which is
//! pure Rust: no libpq, no native libraries, identical builds on every
//! platform. [`connect`] builds a deadpool connection pool and verifies it can
//! actually reach the database; [`run_pending_migrations`] applies the
//! framework's embedded migrations at boot, so a freshly stamped application
//! migrates itself on first start.
//!
//! Applications own their domain tables, so they embed their own migration
//! directory and apply it with [`run_app_migrations`] right after the
//! framework's, which is the order the ownership chain requires: application
//! tables reference `teams`.
//!
//! Both entry points are safe to call concurrently. See [`MIGRATION_LOCK_KEY`].

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};

use diesel::RunQueryDsl;
use diesel_async::AsyncPgConnection;
use diesel_async::async_connection_wrapper::AsyncConnectionWrapper;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};

/// The framework's core-table migrations, compiled into the binary.
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("./migrations");

/// The advisory lock every migration pass holds while it applies.
///
/// Applying migrations is not safe to do twice at once: two appliers issuing
/// the same `CREATE TABLE` race inside Postgres' catalog and the loser fails
/// with a unique violation on `pg_type_typname_nsp_index`, far from anything
/// that reads like a migration problem. A session-scoped advisory lock over one
/// fixed key serializes every applier reaching the same database, so two
/// application instances booting together, or two test binaries starting
/// together, queue instead of colliding. Whoever waits then finds nothing
/// pending and returns.
///
/// The value is arbitrary but must never change: it is the name concurrent
/// processes agree on, and a different value is a different lock. These are the
/// ASCII bytes of `ANUBIS` followed by a counter, so a future lock of the
/// framework's own can take the next one without colliding with this.
const MIGRATION_LOCK_KEY: i64 = 0x414E_5542_4953_0001;

/// A shared pool of async Postgres connections.
pub type DbPool = Pool<AsyncPgConnection>;

/// The synchronous connection Diesel's migration harness runs on.
type MigrationConnection = AsyncConnectionWrapper<AsyncPgConnection>;

/// Builds a connection pool and verifies the database is reachable.
///
/// # Errors
/// Returns an [`Error`] when the pool cannot be built or the database cannot
/// be reached.
pub async fn connect(database_url: &str) -> Result<DbPool, Error> {
    let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(database_url);
    let pool = Pool::builder(manager)
        .build()
        .map_err(|source| Error::new("failed to build the connection pool", source))?;

    // Fail fast at boot instead of on the first request.
    let probe = pool
        .get()
        .await
        .map_err(|source| Error::new("failed to reach the database", source))?;
    drop(probe);

    Ok(pool)
}

/// Applies the framework's embedded migrations that have not run yet.
///
/// Safe to call concurrently: appliers serialize on [`MIGRATION_LOCK_KEY`].
///
/// # Errors
/// Returns an [`Error`] when the database cannot be reached or a migration
/// fails to apply.
pub async fn run_pending_migrations(database_url: &str) -> Result<(), Error> {
    apply(database_url, MIGRATIONS).await
}

/// Applies an application's own embedded migrations.
///
/// Run this after [`run_pending_migrations`]: application tables reference the
/// framework's, and the shared `set_updated_at()` trigger function must exist
/// before a generated migration attaches it.
///
/// ```ignore
/// use diesel_migrations::{EmbeddedMigrations, embed_migrations};
///
/// const APP_MIGRATIONS: EmbeddedMigrations = embed_migrations!("./migrations");
///
/// anubis::db::run_pending_migrations(url).await?;
/// anubis::db::run_app_migrations(url, APP_MIGRATIONS).await?;
/// ```
///
/// # Errors
/// Returns an [`Error`] when the database cannot be reached or a migration
/// fails to apply.
pub async fn run_app_migrations(
    database_url: &str,
    migrations: EmbeddedMigrations,
) -> Result<(), Error> {
    apply(database_url, migrations).await
}

/// Runs a migration set on a blocking thread, which is where Diesel's
/// migration harness lives.
async fn apply(database_url: &str, migrations: EmbeddedMigrations) -> Result<(), Error> {
    let url = database_url.to_owned();

    let outcome = tokio::task::spawn_blocking(move || {
        use diesel::Connection;

        let mut connection = MigrationConnection::establish(&url)
            .map_err(|source| Error::new("failed to connect for migrations", source))?;

        take_migration_lock(&mut connection)?;
        let applied = connection
            .run_pending_migrations(migrations)
            .map(|_versions| ())
            .map_err(|source| Error::from_boxed("failed to apply migrations", source));
        release_migration_lock(&mut connection);

        applied
    })
    .await;

    match outcome {
        Ok(result) => result,
        Err(source) => Err(Error::new("the migration task panicked", source)),
    }
}

/// Waits for [`MIGRATION_LOCK_KEY`], then holds it.
///
/// The key is a compile-time integer constant, so rendering it into the
/// statement carries no input to inject.
fn take_migration_lock(connection: &mut MigrationConnection) -> Result<(), Error> {
    diesel::sql_query(format!("SELECT pg_advisory_lock({MIGRATION_LOCK_KEY})"))
        .execute(connection)
        .map(|_rows| ())
        .map_err(|source| Error::new("failed to take the migration lock", source))
}

/// Releases [`MIGRATION_LOCK_KEY`].
///
/// A failure here is logged rather than raised: the lock is session scoped, so
/// this connection closing moments later releases it regardless, and reporting
/// an unlock failure over a migration failure would bury the one that matters.
fn release_migration_lock(connection: &mut MigrationConnection) {
    if let Err(source) =
        diesel::sql_query(format!("SELECT pg_advisory_unlock({MIGRATION_LOCK_KEY})"))
            .execute(connection)
    {
        tracing::warn!(
            error.message = %source,
            "failed to release the migration lock, which the closing connection \
             releases anyway: {{error.message}}",
        );
    }
}

/// A database connection, pool, or migration failure.
#[derive(Debug)]
pub struct Error {
    context: &'static str,
    source: Box<dyn std::error::Error + Send + Sync>,
    backtrace: Backtrace,
}

impl Error {
    fn new(context: &'static str, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::from_boxed(context, Box::new(source))
    }

    fn from_boxed(context: &'static str, source: Box<dyn std::error::Error + Send + Sync>) -> Self {
        Self {
            context,
            source,
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

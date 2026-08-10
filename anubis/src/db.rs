//! Database connectivity for Anubis applications.
//!
//! Connections go through [`diesel_async`]'s tokio-postgres backend, which is
//! pure Rust: no libpq, no native libraries, identical builds on every
//! platform. [`connect`] builds a deadpool connection pool and verifies it can
//! actually reach the database; [`run_pending_migrations`] applies the
//! framework's embedded migrations at boot, so a freshly stamped application
//! migrates itself on first start.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};

use diesel_async::AsyncPgConnection;
use diesel_async::async_connection_wrapper::AsyncConnectionWrapper;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};

/// The framework's core-table migrations, compiled into the binary.
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("./migrations");

/// A shared pool of async Postgres connections.
pub type DbPool = Pool<AsyncPgConnection>;

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

/// Applies any embedded migrations that have not run yet.
///
/// # Errors
/// Returns an [`Error`] when the database cannot be reached or a migration
/// fails to apply.
pub async fn run_pending_migrations(database_url: &str) -> Result<(), Error> {
    let url = database_url.to_owned();

    let outcome = tokio::task::spawn_blocking(move || {
        use diesel::Connection;

        let mut connection = AsyncConnectionWrapper::<AsyncPgConnection>::establish(&url)
            .map_err(|source| Error::new("failed to connect for migrations", source))?;
        connection
            .run_pending_migrations(MIGRATIONS)
            .map_err(|source| Error::from_boxed("failed to apply migrations", source))?;
        Ok(())
    })
    .await;

    match outcome {
        Ok(result) => result,
        Err(source) => Err(Error::new("the migration task panicked", source)),
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

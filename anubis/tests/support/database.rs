//! A private Postgres database per test, so a narrative owns what it reads.
//!
//! [`TestDatabase`] opens a maintenance connection to the server
//! `DATABASE_URL` names, creates a uniquely named database beside the one in
//! that URL, migrates it, and hands out its URL. Dropping the value drops the
//! database, so a test that panics cleans up as surely as one that passes.
//!
//! ```ignore
//! let Some(database) = support::TestDatabase::create("concurrency_flow").await else {
//!     return;
//! };
//! let pool = database.pool().await;
//! ```
//!
//! # Adopting it elsewhere
//!
//! Migrating the older narratives is mechanical and optional: replace the
//! `std::env::var("DATABASE_URL")` skip-gate and the `run_pending_migrations`
//! call with a `TestDatabase::create` call, then pass `database.url()` wherever
//! the narrative passed `database_url`. What it buys is isolation; what it
//! costs is the fraction of a second it takes to create and migrate a
//! database. Narratives that only read rows they wrote themselves are fine as
//! they are.
//!
//! Packages outside this one (the starter's own suite, for one) cannot import
//! this module, because an integration test's support module is private to its
//! package. They rely instead on the advisory lock every migration pass takes,
//! documented at `anubis::db`'s `MIGRATION_LOCK_KEY`, which is what makes
//! concurrent appliers against one shared database safe.

use std::thread;

use anubis::db::DbPool;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use url::Url;
use uuid::Uuid;

/// The database a maintenance connection opens, to create and drop others.
///
/// Every Postgres server has it, and connecting to the database being dropped
/// is exactly what `DROP DATABASE` refuses.
const MAINTENANCE_DATABASE: &str = "postgres";

/// Names every database this module creates, so a leaked one is recognizable.
const NAME_PREFIX: &str = "anubis_test_";

/// A freshly migrated Postgres database, dropped when this value is.
#[derive(Debug)]
pub struct TestDatabase {
    /// The URL of the database itself, for the code under test.
    url: String,
    /// The database's name, unquoted.
    name: String,
    /// Where [`Drop`] reconnects to issue the `DROP DATABASE`.
    maintenance_url: String,
}

impl TestDatabase {
    /// Creates and migrates a database of this test's own.
    ///
    /// Returns `None` when `DATABASE_URL` is unset, which is the signal for a
    /// test to log a skip and pass; the message names `narrative` so a skipped
    /// run says which suite was skipped. CI always provides the variable.
    ///
    /// # Panics
    /// Panics when the server refuses to create the database, or when the
    /// migrations fail: a test that cannot reach its own database has nothing
    /// left to assert.
    pub async fn create(narrative: &str) -> Option<Self> {
        let database = Self::create_bare(narrative).await?;

        // The database is already held by value, so a migration failure still
        // drops it on the way out.
        anubis::db::run_pending_migrations(&database.url)
            .await
            .expect("migrations must apply to a fresh database");

        Some(database)
    }

    /// Creates an empty database, leaving the migrations to the caller.
    ///
    /// [`TestDatabase::create`] is what a suite wants; this exists for the one
    /// test whose subject *is* applying the migrations.
    ///
    /// # Panics
    /// Panics when `DATABASE_URL` is not a URL, or when the server refuses to
    /// create the database.
    pub async fn create_bare(narrative: &str) -> Option<Self> {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            eprintln!("skipping {narrative}: DATABASE_URL is not set");
            return None;
        };

        let name = format!("{NAME_PREFIX}{}", Uuid::new_v4().simple());
        let maintenance_url = with_database(&database_url, MAINTENANCE_DATABASE);
        let url = with_database(&database_url, &name);

        let mut maintenance = AsyncPgConnection::establish(&maintenance_url)
            .await
            .expect("the maintenance database must be reachable");
        // The name is a constant prefix and a hex UUID, so it carries nothing
        // to inject; Postgres has no bind parameters in `CREATE DATABASE`.
        diesel::sql_query(format!("CREATE DATABASE \"{name}\""))
            .execute(&mut maintenance)
            .await
            .expect("the test database must be creatable");
        drop(maintenance);

        Some(Self {
            url,
            name,
            maintenance_url,
        })
    }

    /// The URL of the database, for whatever wants to connect to it.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// A connection pool onto the database.
    ///
    /// # Panics
    /// Panics when the database this value created is unreachable.
    pub async fn pool(&self) -> DbPool {
        anubis::db::connect(&self.url)
            .await
            .expect("the test database must be reachable")
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let name = self.name.clone();
        let teardown = {
            let maintenance_url = self.maintenance_url.clone();
            let name = name.clone();
            thread::spawn(move || drop_database(&maintenance_url, &name))
        };

        // Losing the database leaks one database and nothing else, while
        // panicking here during an unwind would abort the process, so a
        // failure is reported rather than raised.
        match teardown.join() {
            Ok(Ok(())) => {}
            Ok(Err(message)) => eprintln!("could not drop the test database {name}: {message}"),
            Err(_panicked) => eprintln!("the teardown of the test database {name} panicked"),
        }
    }
}

/// Drops one database, on a runtime of its own.
///
/// [`Drop`] is synchronous and may run on a runtime worker thread, so the
/// teardown brings its own current-thread runtime rather than blocking on the
/// ambient one.
fn drop_database(maintenance_url: &str, name: &str) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;

    runtime.block_on(async {
        let mut maintenance = AsyncPgConnection::establish(maintenance_url)
            .await
            .map_err(|error| error.to_string())?;
        // FORCE closes the connections a pool, or a panicking test, left open;
        // without it `DROP DATABASE` refuses while anything is still attached.
        diesel::sql_query(format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)"))
            .execute(&mut maintenance)
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    })
}

/// Points a connection URL at another database on the same server.
///
/// # Panics
/// Panics when `database_url` is not a URL, which is a broken environment
/// rather than a test failure.
fn with_database(database_url: &str, name: &str) -> String {
    let mut url: Url = database_url.parse().expect("DATABASE_URL must be a URL");
    url.set_path(&format!("/{name}"));
    url.into()
}

//! Password hashing and verification with argon2id, behind a bounded gate.
//!
//! Hashes use [`Argon2::default`], which is argon2id with the RustCrypto
//! defaults, encoded as PHC strings so parameters can evolve without breaking
//! stored hashes. Every computation runs on the blocking thread pool, and the
//! only way to reach one is a [`Hasher`], which admits a fixed number at a
//! time. There are no free functions on purpose: a bound that code can bypass
//! is a bound that code will bypass.
//!
//! # Why the gate exists
//!
//! argon2id at these defaults allocates 19 MiB for as long as a computation
//! runs, and tokio's blocking pool grows to 512 threads. Sign-in is the one
//! endpoint whose cost a stranger controls, and the per-client budgets in
//! [`crate::rate_limit`] do not see a burst spread across many client
//! addresses. Unbounded, such a burst reaches roughly 9.5 GiB resident and the
//! kernel kills the process, which is a much better outcome for an attacker
//! than any number of refused sign-ins. With the gate, the worst case is
//! arithmetic instead of a guess: permits times 19 MiB, and
//! [`DEFAULT_CONCURRENCY`] picks the permits.
//!
//! # Shedding, not queueing
//!
//! A caller that finds the gate full waits up to five seconds and is then
//! refused, which handlers answer as `503` through
//! [`ApiError::unavailable`](crate::http::ApiError::unavailable). Waiting
//! longer would trade one exhausted resource for another, because every
//! queued request holds a task, a database connection, and its buffers, and
//! the ones at the back are answered long after their clients gave up, so the
//! work is spent and the answer wasted. The short wait absorbs ordinary
//! contention invisibly; beyond it, refusing quickly is the honest answer and
//! the one that keeps the process alive to serve everyone else.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};
use std::num::NonZero;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use tokio::sync::Semaphore;

use crate::http::ApiError;

/// Shortest password accepted, per current OWASP guidance.
pub const MIN_PASSWORD_CHARS: usize = 8;

/// Longest password accepted, bounding the cost of hashing attacker input.
pub const MAX_PASSWORD_CHARS: usize = 512;

/// How many argon2 computations a [`Hasher`] admits at once, by default.
///
/// This is a memory budget written as a count. Each computation holds 19 MiB
/// (argon2's `m_cost` at the RustCrypto defaults) while it runs, so 64 permits
/// cap the hashing working set near 1.2 GiB, which leaves room in the 2 GiB
/// container a small deployment runs in and is far below the ~9.5 GiB an
/// unbounded blocking pool would reach. It is also comfortably above any real
/// sign-in rate: a verification takes tens of milliseconds, so 64 in flight is
/// thousands of sign-ins per second before anyone waits.
///
/// Raise it only alongside the memory limit it is derived from, and lower it
/// on a small instance. `PASSWORD_HASH_CONCURRENCY` sets it per deployment;
/// see [`crate::config`].
pub const DEFAULT_CONCURRENCY: NonZero<usize> =
    NonZero::new(64).expect("64 permits is not zero permits");

/// How long a caller waits for a permit before its request is shed.
///
/// Long enough to absorb ordinary contention, since a verification costs tens
/// of milliseconds and a full gate normally drains in one of them, and short
/// enough that a client under a real burst learns quickly rather than holding
/// a connection open for a page nobody is waiting for any more.
const PERMIT_WAIT: Duration = Duration::from_secs(5);

/// Runs argon2 work, capped at a fixed number of computations at once.
///
/// Cloning shares one gate, so every clone counts against the same budget;
/// build one per application and pass it around. See the module docs for why
/// the bound exists and why a full gate refuses rather than queues.
///
/// # Examples
///
/// ```
/// # use anubis::auth::password::{DEFAULT_CONCURRENCY, Hasher};
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let hasher = Hasher::new(DEFAULT_CONCURRENCY);
///
/// let stored = hasher.hash("correct horse battery staple".to_owned()).await?;
/// assert!(hasher.verify("correct horse battery staple".to_owned(), stored).await?);
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Hasher {
    /// One permit per admitted computation, held for exactly its duration.
    permits: Arc<Semaphore>,
}

impl Hasher {
    /// Builds a hasher admitting `concurrency` computations at once.
    ///
    /// There is deliberately no `Default`: the count is a memory budget, and
    /// [`DEFAULT_CONCURRENCY`] documents what budget it assumes, so passing it
    /// is a decision rather than an omission.
    #[must_use]
    pub fn new(concurrency: NonZero<usize>) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(concurrency.get())),
        }
    }

    /// Hashes a password on the blocking thread pool.
    ///
    /// # Errors
    /// Returns an [`Error`] when the gate stays full (see
    /// [`Error::is_overloaded`]), when hashing fails, or when the blocking
    /// task panics.
    pub async fn hash(&self, password: String) -> Result<String, Error> {
        self.admit(move || hash_sync(&password)).await
    }

    /// Verifies a password against a stored PHC hash on the blocking pool.
    ///
    /// Returns `Ok(false)` for a wrong password; `Err` means the request was
    /// shed or the stored hash itself could not be processed.
    ///
    /// # Errors
    /// Returns an [`Error`] when the gate stays full (see
    /// [`Error::is_overloaded`]), when the stored hash is malformed, or when
    /// the blocking task panics.
    pub async fn verify(&self, password: String, stored_hash: String) -> Result<bool, Error> {
        self.admit(move || verify_sync(&password, &stored_hash))
            .await
    }

    /// Burns the same CPU as a real verification without a real hash.
    ///
    /// Login must take the same time whether or not the account exists, or
    /// response timing leaks which emails are registered. Call this on the
    /// account-not-found path.
    ///
    /// # Errors
    /// Returns an [`Error`] only when the request is shed, which the caller
    /// must propagate: if the account-not-found path answered `401` while the
    /// account-exists path answered `503`, an overloaded server would become
    /// the account oracle the equal timing exists to prevent. A genuine
    /// hashing failure is logged and swallowed, because this hash guards
    /// nothing.
    pub(crate) async fn verify_against_dummy(&self, password: String) -> Result<(), Error> {
        let outcome = self
            .admit(move || {
                // Hashed once per process: it is a constant, and computing it
                // per call would double the cost of the path it equalizes.
                static DUMMY_HASH: OnceLock<String> = OnceLock::new();
                let dummy_hash = DUMMY_HASH
                    .get_or_init(|| hash_sync("anubis-timing-equalizer").unwrap_or_default());
                verify_sync(&password, dummy_hash)
            })
            .await;

        match outcome {
            Ok(_matched) => Ok(()),
            Err(error) if error.is_overloaded() => Err(error),
            Err(error) => {
                tracing::debug!(
                    error.message = %error,
                    "dummy password verification failed: {{error.message}}",
                );
                Ok(())
            }
        }
    }

    /// Runs one computation once the gate admits it, or sheds the request.
    async fn admit<Output>(
        &self,
        work: impl FnOnce() -> Result<Output, Error> + Send + 'static,
    ) -> Result<Output, Error>
    where
        Output: Send + 'static,
    {
        let waited = tokio::time::timeout(PERMIT_WAIT, Arc::clone(&self.permits).acquire_owned());
        let Ok(acquired) = waited.await else {
            tracing::warn!(
                password.hash.wait_seconds = PERMIT_WAIT.as_secs(),
                "password hashing is saturated: shedding a request after \
                 {{password.hash.wait_seconds}}s",
            );
            return Err(Error::overloaded());
        };
        let permit = acquired.expect("the hashing gate is never closed");

        let finished = tokio::task::spawn_blocking(move || {
            // The permit lives inside the computation, so it is released when
            // the memory is, not when the caller stops waiting: a client that
            // disconnects mid-hash must not admit a second computation on top
            // of the 19 MiB the first one still holds.
            let _permit = permit;
            work()
        })
        .await;

        match finished {
            Ok(result) => result,
            Err(source) => Err(Error::failed("the password hashing task panicked", source)),
        }
    }
}

fn hash_sync(password: &str) -> Result<String, Error> {
    let salt = SaltString::generate(&mut OsRng);
    let hashed = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|source| Error::failed("failed to hash the password", source))?;
    Ok(hashed.to_string())
}

fn verify_sync(password: &str, stored_hash: &str) -> Result<bool, Error> {
    let parsed = PasswordHash::new(stored_hash)
        .map_err(|source| Error::failed("the stored password hash is malformed", source))?;

    match Argon2::default().verify_password(password.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(source) => Err(Error::failed("failed to verify the password", source)),
    }
}

/// A password hashing or verification failure.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    backtrace: Backtrace,
}

/// What went wrong, kept private so callers ask rather than match.
#[derive(Debug)]
enum ErrorKind {
    /// argon2, the stored hash, or the blocking pool failed.
    Failed {
        context: &'static str,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The gate stayed full for longer than a caller can be asked to wait.
    Overloaded,
}

impl Error {
    /// Returns `true` when the request was shed rather than attempted.
    ///
    /// Shedding is load, not a bug: the work was never started, the caller may
    /// retry, and the right answer is `503` rather than `500`.
    #[must_use]
    pub fn is_overloaded(&self) -> bool {
        matches!(self.kind, ErrorKind::Overloaded)
    }

    fn failed(
        context: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::new(ErrorKind::Failed {
            context,
            source: Box::new(source),
        })
    }

    fn overloaded() -> Self {
        Self::new(ErrorKind::Overloaded)
    }

    fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            backtrace: Backtrace::capture(),
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ErrorKind::Failed { context, source } => write!(f, "{context}: {source}")?,
            ErrorKind::Overloaded => {
                f.write_str("password hashing is saturated, so the request was shed")?;
            }
        }
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            ErrorKind::Failed { source, .. } => Some(source.as_ref()),
            ErrorKind::Overloaded => None,
        }
    }
}

impl From<Error> for ApiError {
    /// Renders a hashing failure as the status the caller can act on.
    ///
    /// A shed request answers `503`, which says "not now" rather than "we
    /// broke", and is not logged here because the gate logged it already.
    /// Anything else is this application's fault: it logs, because
    /// [`ApiError::internal`]'s body carries nothing but the request id.
    fn from(error: Error) -> Self {
        if error.is_overloaded() {
            return Self::unavailable("The server is busy. Try again in a moment.");
        }

        tracing::error!(
            error.message = %error,
            "a password hashing operation failed: {{error.message}}",
        );
        Self::internal()
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;
    use std::sync::Arc;

    use axum::http::StatusCode;

    use super::{DEFAULT_CONCURRENCY, Hasher};
    use crate::http::ApiError;

    /// A gate wide enough that nothing in a test ever waits on it.
    fn hasher() -> Hasher {
        Hasher::new(DEFAULT_CONCURRENCY)
    }

    #[tokio::test]
    async fn round_trips_the_right_password() {
        let hasher = hasher();
        let stored = hasher
            .hash("correct horse battery staple".to_owned())
            .await
            .expect("hashing must succeed");

        assert!(stored.starts_with("$argon2id$"), "got: {stored}");

        let matched = hasher
            .verify("correct horse battery staple".to_owned(), stored)
            .await
            .expect("verification must succeed");
        assert!(matched);
    }

    #[tokio::test]
    async fn rejects_the_wrong_password() {
        let hasher = hasher();
        let stored = hasher
            .hash("correct horse battery staple".to_owned())
            .await
            .expect("hashing must succeed");

        let matched = hasher
            .verify("Tr0ub4dor&3".to_owned(), stored)
            .await
            .expect("verification must succeed");
        assert!(!matched);
    }

    #[tokio::test]
    async fn salts_make_equal_passwords_hash_differently() {
        let hasher = hasher();
        let first = hasher
            .hash("same password".to_owned())
            .await
            .expect("must hash");
        let second = hasher
            .hash("same password".to_owned())
            .await
            .expect("must hash");
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn malformed_stored_hashes_are_an_error_not_a_mismatch() {
        let error = hasher()
            .verify("anything".to_owned(), "not-a-phc-string".to_owned())
            .await
            .expect_err("malformed hashes must be an error");

        assert!(!error.is_overloaded());
        let rendered = error.to_string();
        assert!(rendered.contains("malformed"), "got: {rendered}");
        assert_eq!(
            ApiError::from(error).status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "a broken stored hash is this application's fault",
        );
    }

    /// A caller that cannot be admitted is refused instead of queued.
    ///
    /// Time is paused, so the wait expires as soon as the runtime has nothing
    /// else to run and the shed is deterministic rather than a race against
    /// however long argon2 takes here. The permit is held directly for the
    /// same reason: a paused clock does not advance while a blocking thread is
    /// working, so occupying the gate with a real hash would wait in real
    /// time.
    #[tokio::test(start_paused = true)]
    async fn a_full_gate_sheds_the_caller_it_cannot_admit() {
        let hasher = Hasher::new(NonZero::new(1).expect("one is not zero"));
        let occupied = Arc::clone(&hasher.permits)
            .try_acquire_owned()
            .expect("the gate starts open");

        let error = hasher
            .hash("shed me".to_owned())
            .await
            .expect_err("a full gate must refuse rather than queue");

        assert!(error.is_overloaded(), "got: {error}");
        assert_eq!(
            ApiError::from(error).status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a shed request is load, not a bug",
        );

        drop(occupied);
        let _admitted = hasher
            .hash("admit me".to_owned())
            .await
            .expect("a reopened gate must admit the next caller");
    }

    /// A permit lasts exactly as long as the computation it admitted.
    #[tokio::test]
    async fn a_computation_holds_its_permit_until_the_work_ends() {
        let hasher = Hasher::new(NonZero::new(1).expect("one is not zero"));

        let admitted = hasher.clone();
        let hashing = tokio::spawn(async move { admitted.hash("first".to_owned()).await });
        // One yield is enough for the spawned task to reach its first await,
        // which is inside the blocking computation, so the permit is taken.
        tokio::task::yield_now().await;
        assert_eq!(hasher.permits.available_permits(), 0);

        let _hashed = hashing
            .await
            .expect("the admitted task must not panic")
            .expect("the admitted hash must succeed");
        assert_eq!(
            hasher.permits.available_permits(),
            1,
            "the permit comes back with the memory it accounted for",
        );
    }

    #[tokio::test]
    async fn the_dummy_verification_answers_without_a_stored_hash() {
        hasher()
            .verify_against_dummy("anything".to_owned())
            .await
            .expect("an open gate must not shed");
    }
}

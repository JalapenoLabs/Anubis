//! Password hashing and verification with argon2id.
//!
//! Hashes use [`Argon2::default`], which is argon2id with the RustCrypto
//! defaults, encoded as PHC strings so parameters can evolve without breaking
//! stored hashes. Hashing is CPU-bound by design, so the async functions run
//! the work on the blocking thread pool; never call the hash inside a request
//! handler without them.

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Display, Formatter};
use std::sync::OnceLock;

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};

/// Shortest password accepted, per current OWASP guidance.
pub const MIN_PASSWORD_CHARS: usize = 8;

/// Longest password accepted, bounding the cost of hashing attacker input.
pub const MAX_PASSWORD_CHARS: usize = 512;

/// Hashes a password on the blocking thread pool.
///
/// # Errors
/// Returns an [`Error`] when hashing fails or the blocking task panics.
pub async fn hash(password: String) -> Result<String, Error> {
    run_blocking(move || hash_sync(&password)).await
}

/// Verifies a password against a stored PHC hash on the blocking thread pool.
///
/// Returns `Ok(false)` for a wrong password; `Err` means the stored hash
/// itself could not be processed.
///
/// # Errors
/// Returns an [`Error`] when the stored hash is malformed or the blocking
/// task panics.
pub async fn verify(password: String, stored_hash: String) -> Result<bool, Error> {
    run_blocking(move || verify_sync(&password, &stored_hash)).await
}

/// Burns the same CPU as a real verification without a real hash.
///
/// Login must take the same time whether or not the account exists, or
/// response timing leaks which emails are registered. Call this on the
/// account-not-found path.
pub(crate) async fn verify_against_dummy(password: String) {
    let outcome = run_blocking(move || {
        static DUMMY_HASH: OnceLock<String> = OnceLock::new();
        let dummy_hash =
            DUMMY_HASH.get_or_init(|| hash_sync("anubis-timing-equalizer").unwrap_or_default());
        verify_sync(&password, dummy_hash)
    })
    .await;

    if let Err(error) = outcome {
        tracing::debug!(
            error.message = %error,
            "dummy password verification failed: {{error.message}}",
        );
    }
}

fn hash_sync(password: &str) -> Result<String, Error> {
    let salt = SaltString::generate(&mut OsRng);
    let hashed = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|source| Error::new("failed to hash the password", source))?;
    Ok(hashed.to_string())
}

fn verify_sync(password: &str, stored_hash: &str) -> Result<bool, Error> {
    let parsed = PasswordHash::new(stored_hash)
        .map_err(|source| Error::new("the stored password hash is malformed", source))?;

    match Argon2::default().verify_password(password.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(source) => Err(Error::new("failed to verify the password", source)),
    }
}

async fn run_blocking<Output>(
    work: impl FnOnce() -> Result<Output, Error> + Send + 'static,
) -> Result<Output, Error>
where
    Output: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(source) => Err(Error::new("the password hashing task panicked", source)),
    }
}

/// A password hashing or verification failure.
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

#[cfg(test)]
mod tests {
    use super::{hash, verify};

    #[tokio::test]
    async fn round_trips_the_right_password() {
        let stored = hash("correct horse battery staple".to_owned())
            .await
            .expect("hashing must succeed");

        assert!(stored.starts_with("$argon2id$"), "got: {stored}");

        let matched = verify("correct horse battery staple".to_owned(), stored)
            .await
            .expect("verification must succeed");
        assert!(matched);
    }

    #[tokio::test]
    async fn rejects_the_wrong_password() {
        let stored = hash("correct horse battery staple".to_owned())
            .await
            .expect("hashing must succeed");

        let matched = verify("Tr0ub4dor&3".to_owned(), stored)
            .await
            .expect("verification must succeed");
        assert!(!matched);
    }

    #[tokio::test]
    async fn salts_make_equal_passwords_hash_differently() {
        let first = hash("same password".to_owned()).await.expect("must hash");
        let second = hash("same password".to_owned()).await.expect("must hash");
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn malformed_stored_hashes_are_an_error_not_a_mismatch() {
        let error = verify("anything".to_owned(), "not-a-phc-string".to_owned())
            .await
            .expect_err("malformed hashes must be an error");
        let rendered = error.to_string();
        assert!(rendered.contains("malformed"), "got: {rendered}");
    }
}

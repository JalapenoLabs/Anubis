//! `anubis secret generate`: mint the value `ANUBIS_SECRET_KEY` expects.
//!
//! The key is the only thing on stdout, so `ANUBIS_SECRET_KEY="$(anubis
//! secret generate)"` captures it cleanly; the guidance goes to stderr, where
//! a human reads it and a pipe does not.

use std::process::ExitCode;

use anubis::auth::secret_box::SecretKey;

/// Prints a fresh 32-byte key as base64, plus one line of guidance.
pub(crate) fn generate() -> ExitCode {
    println!("{}", SecretKey::generate().to_base64());
    eprintln!(
        "Set this as ANUBIS_SECRET_KEY. Rotating it makes every value sealed under the old key \
         unreadable, so read docs/architecture.md before replacing a live one."
    );
    ExitCode::SUCCESS
}

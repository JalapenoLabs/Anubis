//! Random opaque tokens and their at-rest hashes.
//!
//! Sessions, email verification, and password reset all use the same token
//! discipline: hand out a random 256-bit token, store only its SHA-256, and
//! compare by hashing the presented token. Nothing recoverable ever sits in
//! the database.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// Random bytes per token; 32 bytes = 256 bits of entropy.
const TOKEN_BYTES: usize = 32;

/// Generates a fresh random token for handing to a client.
pub(crate) fn generate() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).expect("the OS random source must be available");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Hashes a token for storage or lookup.
pub(crate) fn hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

#[cfg(test)]
mod tests {
    use super::{generate, hash};

    #[test]
    fn tokens_are_long_random_and_unique() {
        let first = generate();
        let second = generate();

        assert_ne!(first, second);
        // 32 bytes of base64url without padding is 43 characters.
        assert_eq!(first.len(), 43);
    }

    #[test]
    fn hashes_are_stable_and_do_not_reveal_the_token() {
        let token = generate();

        assert_eq!(hash(&token), hash(&token));
        assert_ne!(hash(&token), token);
        assert!(!hash(&token).contains(&token));
    }
}

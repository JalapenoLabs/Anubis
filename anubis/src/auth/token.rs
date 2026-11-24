//! Random opaque tokens and their at-rest hashes.
//!
//! Sessions, email verification, and password reset all use the same token
//! discipline: hand out a random 256-bit token, store only its SHA-256, and
//! compare by hashing the presented token. Nothing recoverable ever sits in
//! the database.
//!
//! [`readable`] is the other kind of secret: one a person reads off a screen
//! and types, such as an operator's temporary password or a device's user
//! code, drawn from an alphabet chosen to survive that.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

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

/// Draws a code a person types: `groups` runs of `group_length` characters
/// from `alphabet`, joined by dashes.
///
/// Every character is equally likely. A random byte maps to a character by
/// its remainder, so bytes at or past the largest multiple of the alphabet's
/// size that fits in a byte are discarded rather than reduced, which would
/// otherwise favour the alphabet's first characters. The working buffer and
/// the result are zeroed on drop, because one caller hands out a password.
///
/// # Panics
/// Panics when `alphabet` is empty or longer than 256 characters, which is a
/// mistake in the caller's constant, or when the operating system has no
/// random source, which is a broken host.
pub(crate) fn readable(alphabet: &[u8], groups: usize, group_length: usize) -> Zeroizing<String> {
    assert!(
        (1..=256).contains(&alphabet.len()),
        "a readable code's alphabet holds 1 to 256 characters",
    );
    let alphabet_size = u16::try_from(alphabet.len()).expect("the alphabet fits in 256");
    let rejection_threshold = 256 - 256 % alphabet_size;

    let length = groups * group_length;
    let mut code = Zeroizing::new(String::with_capacity(length + groups));
    let mut drawn = 0;
    // One draw usually covers the whole code; a draw whose bytes were mostly
    // rejected is simply followed by another.
    let mut entropy = Zeroizing::new([0u8; 32]);
    while drawn < length {
        getrandom::fill(entropy.as_mut()).expect("the OS random source must be available");
        for &byte in entropy.iter() {
            if drawn == length {
                break;
            }
            if u16::from(byte) >= rejection_threshold {
                continue;
            }

            if drawn > 0 && drawn % group_length == 0 {
                code.push('-');
            }
            code.push(char::from(alphabet[usize::from(byte) % alphabet.len()]));
            drawn += 1;
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::{generate, hash, readable};

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

    #[test]
    fn a_readable_code_is_grouped_and_drawn_from_its_alphabet() {
        let code = readable(b"BCDF", 3, 2);
        let groups: Vec<&str> = code.split('-').collect();

        assert_eq!(groups.len(), 3);
        for group in groups {
            assert_eq!(group.len(), 2);
            assert!(group.bytes().all(|character| b"BCDF".contains(&character)));
        }
    }

    #[test]
    fn every_character_of_an_alphabet_that_does_not_divide_a_byte_is_drawn() {
        // Twenty does not divide 256, so this exercises the rejection path;
        // a sampler that never reached the last characters would miss some.
        let alphabet = b"BCDFGHJKLMNPQRSTVWXZ";
        let drawn = readable(alphabet, 1, 4_000);

        assert!(
            alphabet
                .iter()
                .all(|character| drawn.as_bytes().contains(character))
        );
    }
}

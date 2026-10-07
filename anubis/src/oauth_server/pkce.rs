//! Proof Key for Code Exchange (RFC 7636), `S256` only.
//!
//! OAuth 2.1 makes PKCE mandatory and the MCP specification makes `S256` the
//! only method a client may use, so `plain` is not offered: a server that
//! accepted it would accept a challenge an eavesdropper on the authorization
//! request could answer.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// The only challenge method this server accepts.
pub(crate) const METHOD: &str = "S256";

/// The length of a base64url SHA-256 digest without padding.
const CHALLENGE_LENGTH: usize = 43;

/// RFC 7636 section 4.1 bounds a verifier to 43 through 128 characters.
const VERIFIER_LENGTHS: std::ops::RangeInclusive<usize> = 43..=128;

/// Whether `challenge` can be an `S256` challenge at all.
pub(crate) fn is_challenge(challenge: &str) -> bool {
    challenge.len() == CHALLENGE_LENGTH && challenge.bytes().all(is_base64url)
}

/// Whether `verifier` answers `challenge`.
pub(crate) fn verify(verifier: &str, challenge: &str) -> bool {
    if !VERIFIER_LENGTHS.contains(&verifier.len()) || !verifier.bytes().all(is_unreserved) {
        return false;
    }

    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())) == challenge
}

fn is_base64url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

/// RFC 3986's unreserved characters, which is what a verifier is made of.
fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

#[cfg(test)]
mod tests {
    use super::{is_challenge, verify};

    /// The example from RFC 7636 appendix B.
    const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    #[test]
    fn the_rfc_example_verifies() {
        assert!(is_challenge(CHALLENGE));
        assert!(verify(VERIFIER, CHALLENGE));
    }

    #[test]
    fn a_wrong_verifier_fails() {
        let wrong = VERIFIER.replace('d', "e");

        assert!(!verify(&wrong, CHALLENGE));
    }

    #[test]
    fn a_verifier_outside_the_bounds_fails_even_if_it_hashes() {
        // A plain challenge: the verifier itself, which S256 must not accept.
        assert!(!verify("short", "short"));
        assert!(!verify(&"a".repeat(129), CHALLENGE));
        assert!(!verify(&format!("{VERIFIER}!"), CHALLENGE));
    }

    #[test]
    fn a_challenge_must_be_a_digest_shape() {
        assert!(!is_challenge("plain-verifier"));
        assert!(!is_challenge(&format!("{CHALLENGE}=")));
    }
}

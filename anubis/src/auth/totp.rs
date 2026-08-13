//! Time-based one-time passwords (RFC 6238) for authenticator apps.
//!
//! The parameters are the ones every mainstream authenticator defaults to:
//! HMAC-SHA1, 6 digits, 30-second steps. Verification accepts one step of
//! clock skew in either direction. Secrets are 160 random bits, base32 per
//! RFC 4648 as `otpauth://` URIs expect.

use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

/// Seconds per code, the authenticator-app default.
const STEP_SECONDS: u64 = 30;

/// Digits per code, the authenticator-app default.
const DIGITS: u32 = 6;

/// Steps of clock skew tolerated on either side during verification.
const SKEW_STEPS: u64 = 1;

/// Generates a fresh 160-bit secret, base32-encoded for authenticator apps.
///
/// # Panics
/// Panics if the OS random source is unavailable, which makes any
/// security-sensitive operation unsafe to continue.
#[must_use]
pub fn generate_secret() -> String {
    let mut bytes = [0u8; 20];
    getrandom::fill(&mut bytes).expect("the OS random source must be available");
    BASE32_NOPAD.encode(&bytes)
}

/// The `otpauth://` provisioning URI an authenticator app enrolls from.
#[must_use]
pub fn otpauth_uri(issuer: &str, account: &str, secret: &str) -> String {
    let issuer = percent_encode(issuer);
    let account = percent_encode(account);
    format!(
        "otpauth://totp/{issuer}:{account}?secret={secret}&issuer={issuer}\
         &algorithm=SHA1&digits={DIGITS}&period={STEP_SECONDS}"
    )
}

/// The code a correct authenticator shows at `unix_time`.
///
/// Returns `None` when the secret is not valid base32.
#[must_use]
pub fn code_at(secret: &str, unix_time: u64) -> Option<String> {
    let key = BASE32_NOPAD
        .decode(secret.trim().to_ascii_uppercase().as_bytes())
        .ok()?;

    let counter = unix_time / STEP_SECONDS;
    let mut mac = Hmac::<Sha1>::new_from_slice(&key).ok()?;
    mac.update(&counter.to_be_bytes());
    let digest = mac.finalize().into_bytes();

    // Dynamic truncation per RFC 4226.
    let offset = usize::from(digest[19] & 0x0F);
    let binary = (u32::from(digest[offset] & 0x7F) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);
    let code = binary % 10u32.pow(DIGITS);

    Some(format!("{code:06}"))
}

/// Verifies a submitted code at `unix_time`, tolerating one step of skew.
#[must_use]
pub fn verify(secret: &str, submitted: &str, unix_time: u64) -> bool {
    let submitted = submitted.trim();
    if submitted.len() != DIGITS as usize || !submitted.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }

    let earliest = unix_time.saturating_sub(SKEW_STEPS * STEP_SECONDS);
    let mut checked = earliest;
    while checked <= unix_time + SKEW_STEPS * STEP_SECONDS {
        if code_at(secret, checked).is_some_and(|expected| expected == submitted) {
            return true;
        }
        checked += STEP_SECONDS;
    }
    false
}

/// Percent-encodes the label components of an `otpauth://` URI.
fn percent_encode(raw: &str) -> String {
    use std::fmt::Write;

    let mut encoded = String::new();
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char);
            }
            other => {
                let _ = write!(encoded, "%{other:02X}");
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use data_encoding::BASE32_NOPAD;

    use super::{code_at, generate_secret, otpauth_uri, verify};

    /// The RFC 6238 test secret: ASCII "12345678901234567890".
    fn rfc_secret() -> String {
        BASE32_NOPAD.encode(b"12345678901234567890")
    }

    #[test]
    fn matches_the_rfc_6238_sha1_vectors() {
        let secret = rfc_secret();
        for (time, expected) in [
            (59u64, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
        ] {
            // The RFC lists 8-digit codes; the 6-digit code is its suffix.
            assert_eq!(
                code_at(&secret, time).expect("the secret must decode"),
                expected,
                "at time {time}"
            );
        }
    }

    #[test]
    fn verification_tolerates_one_step_of_skew_only() {
        let secret = rfc_secret();
        let code = code_at(&secret, 1_111_111_109).expect("the secret must decode");

        assert!(verify(&secret, &code, 1_111_111_109), "exact step");
        assert!(verify(&secret, &code, 1_111_111_109 + 30), "one step late");
        assert!(verify(&secret, &code, 1_111_111_109 - 30), "one step early");
        assert!(!verify(&secret, &code, 1_111_111_109 + 90), "too late");
    }

    #[test]
    fn malformed_codes_and_secrets_never_verify() {
        let secret = rfc_secret();
        assert!(!verify(&secret, "12345", 59), "too short");
        assert!(!verify(&secret, "abcdef", 59), "not digits");
        assert!(!verify("not base32!!", "287082", 59), "bad secret");
    }

    #[test]
    fn generated_secrets_are_base32_and_unique() {
        let first = generate_secret();
        let second = generate_secret();

        assert_ne!(first, second);
        let decoded = BASE32_NOPAD
            .decode(first.as_bytes())
            .expect("secrets must be valid base32");
        assert_eq!(decoded.len(), 20);
    }

    #[test]
    fn provisioning_uris_escape_the_label() {
        let uri = otpauth_uri("Anubis", "alex@example.com", "SECRET");
        assert!(
            uri.starts_with("otpauth://totp/Anubis:alex%40example.com?secret=SECRET"),
            "got: {uri}"
        );
        assert!(uri.contains("issuer=Anubis"), "got: {uri}");
        assert!(uri.contains("digits=6"), "got: {uri}");
    }
}

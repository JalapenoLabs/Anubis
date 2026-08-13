//! The signature every outgoing delivery carries.
//!
//! A receiver cannot trust a request just because it arrived at the URL it
//! published: the URL is not a secret, and an attacker who learns it can post
//! anything. So every delivery carries an HMAC of its own body under the
//! endpoint's signing secret, and a receiver recomputes it before believing a
//! word of the payload.
//!
//! # The scheme
//!
//! Four headers ride on every POST:
//!
//! | Header | Value |
//! |---|---|
//! | [`ID_HEADER`] | The delivery's id, stable across retries of that delivery |
//! | [`EVENT_HEADER`] | The event type, e.g. `project.created` |
//! | [`TIMESTAMP_HEADER`] | Unix seconds at which the request was signed |
//! | [`SIGNATURE_HEADER`] | `v1=<hex>`, the signature below |
//!
//! The signed message is the timestamp, a literal `.`, and the exact response
//! body, concatenated:
//!
//! ```text
//! v1 = hex(HMAC-SHA256(secret, "<timestamp>.<body>"))
//! ```
//!
//! The timestamp is inside the MAC rather than beside it, which is what makes
//! it trustworthy: an attacker replaying a captured request cannot move its
//! timestamp forward without invalidating the signature. A receiver should
//! therefore reject a signature whose timestamp is far from now, and should
//! compare signatures in constant time. [`verify`] does both, and is what an
//! Anubis application receiving these deliveries uses.
//!
//! Signing the raw body, not a re-serialization of it, is the other half of the
//! contract: a receiver must MAC the bytes it read off the wire, because any
//! JSON library is free to reorder keys or change spacing on a round trip.

use std::time::Duration;

use data_encoding::HEXLOWER;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// Names the delivery, so a receiver can make handling idempotent.
pub const ID_HEADER: &str = "anubis-webhook-id";

/// Names the event, so a receiver can route without parsing the body.
pub const EVENT_HEADER: &str = "anubis-webhook-event";

/// Unix seconds the request was signed at.
pub const TIMESTAMP_HEADER: &str = "anubis-webhook-timestamp";

/// The signature itself, `v1=<hex>`.
pub const SIGNATURE_HEADER: &str = "anubis-webhook-signature";

/// Names the current signing scheme inside [`SIGNATURE_HEADER`].
///
/// A future scheme is published as `v2=` **beside** `v1=` in the same header,
/// so receivers that only know `v1` keep working while they migrate. Never
/// reuse this prefix for another scheme.
pub const SCHEME: &str = "v1";

/// How far a signature's timestamp may sit from now before [`verify`] rejects
/// it.
///
/// Wide enough to survive a slow retry and clocks that disagree by a few
/// minutes, narrow enough that a captured request stops being useful the same
/// day. Receivers outside this crate are free to choose their own window; this
/// is the framework's answer for the ones that do not.
pub const MAX_CLOCK_SKEW: Duration = Duration::from_mins(5);

/// Signs `body` for `timestamp` with an endpoint's signing secret.
///
/// Returns the whole [`SIGNATURE_HEADER`] value, scheme prefix included.
///
/// # Examples
/// ```
/// use anubis::webhooks::signature;
///
/// let header = signature::sign("whsec_example", 1_760_000_000, r#"{"id":"1"}"#);
/// assert!(header.starts_with("v1="));
/// ```
#[must_use]
pub fn sign(secret: &str, timestamp: i64, body: &str) -> String {
    format!(
        "{SCHEME}={}",
        HEXLOWER.encode(&mac(secret, timestamp, body))
    )
}

/// Returns `true` when `header` signs `body` for a timestamp close to `now`.
///
/// Rejects a signature whose timestamp is further from `now` than
/// [`MAX_CLOCK_SKEW`] in either direction, which is what stops a captured
/// request from being replayed forever. The comparison itself is constant
/// time, so a wrong signature leaks nothing about the right one.
///
/// `header` is the raw [`SIGNATURE_HEADER`] value and may carry several
/// schemes separated by commas; any one that matches accepts the request.
///
/// # Examples
/// ```
/// use anubis::webhooks::signature;
///
/// let body = r#"{"id":"1"}"#;
/// let header = signature::sign("whsec_example", 1_760_000_000, body);
///
/// assert!(signature::verify("whsec_example", 1_760_000_000, &header, body, 1_760_000_010));
/// assert!(!signature::verify("whsec_other", 1_760_000_000, &header, body, 1_760_000_010));
/// ```
#[must_use]
pub fn verify(secret: &str, timestamp: i64, header: &str, body: &str, now: i64) -> bool {
    let skew = now.saturating_sub(timestamp).unsigned_abs();
    if skew > MAX_CLOCK_SKEW.as_secs() {
        return false;
    }

    let expected = mac(secret, timestamp, body);
    header.split(',').any(|candidate| {
        let Some(hex) = candidate.trim().strip_prefix(&format!("{SCHEME}=")) else {
            return false;
        };
        HEXLOWER
            .decode(hex.as_bytes())
            .is_ok_and(|presented| constant_time_eq(&expected, &presented))
    })
}

/// The raw MAC over `<timestamp>.<body>`.
fn mac(secret: &str, timestamp: i64, body: &str) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts a key of any length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Compares two byte strings without leaking where they first differ.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let difference = left
        .iter()
        .zip(right)
        .fold(0u8, |accumulated, (left, right)| {
            accumulated | (left ^ right)
        });
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::{MAX_CLOCK_SKEW, SCHEME, sign, verify};

    const FIXTURE_SECRET: &str = "whsec_2hSVgKZ8sYqvVQeXK1oPq0RmA6c9tYJc";
    const FIXTURE_TIMESTAMP: i64 = 1_760_000_000;
    const FIXTURE_BODY: &str = r#"{"project":{"id":"9f4a","name":"Apollo"}}"#;

    /// The signature an independent HMAC-SHA256 implementation produces for the
    /// fixture above. Regenerating it would defeat its purpose: if this test
    /// fails, every receiver in the world broke and the change needs a `v2`
    /// scheme beside `v1` rather than an edit here.
    const FIXTURE_SIGNATURE: &str =
        "v1=d01516053453ebc258ed4fe86ba78e1e2a12d10e6db54824c4256bb0357ed63b";

    #[test]
    fn the_v1_wire_format_stays_stable() {
        assert_eq!(
            sign(FIXTURE_SECRET, FIXTURE_TIMESTAMP, FIXTURE_BODY),
            FIXTURE_SIGNATURE,
        );
        assert!(verify(
            FIXTURE_SECRET,
            FIXTURE_TIMESTAMP,
            FIXTURE_SIGNATURE,
            FIXTURE_BODY,
            FIXTURE_TIMESTAMP,
        ));
    }

    #[test]
    fn a_signature_names_its_scheme_and_hexes_a_sha256_mac() {
        let header = sign(FIXTURE_SECRET, FIXTURE_TIMESTAMP, FIXTURE_BODY);

        let hex = header
            .strip_prefix(&format!("{SCHEME}="))
            .expect("the scheme prefix is part of the contract");
        // SHA-256 is 32 bytes, and lowercase hex doubles that.
        assert_eq!(hex.len(), 64, "got: {header}");
        assert!(hex.chars().all(|character| character.is_ascii_hexdigit()));
    }

    #[test]
    fn the_signature_covers_the_secret_the_timestamp_and_the_body() {
        let baseline = sign(FIXTURE_SECRET, FIXTURE_TIMESTAMP, FIXTURE_BODY);

        assert_ne!(
            baseline,
            sign("whsec_other", FIXTURE_TIMESTAMP, FIXTURE_BODY)
        );
        assert_ne!(
            baseline,
            sign(FIXTURE_SECRET, FIXTURE_TIMESTAMP + 1, FIXTURE_BODY)
        );
        assert_ne!(baseline, sign(FIXTURE_SECRET, FIXTURE_TIMESTAMP, "{}"));
        // Deterministic, which is what lets a receiver recompute it at all.
        assert_eq!(
            baseline,
            sign(FIXTURE_SECRET, FIXTURE_TIMESTAMP, FIXTURE_BODY)
        );
    }

    #[test]
    fn verification_accepts_what_signing_produced() {
        let header = sign(FIXTURE_SECRET, FIXTURE_TIMESTAMP, FIXTURE_BODY);

        assert!(verify(
            FIXTURE_SECRET,
            FIXTURE_TIMESTAMP,
            &header,
            FIXTURE_BODY,
            FIXTURE_TIMESTAMP + 30,
        ));
        // A header carrying an unknown scheme beside the known one still
        // verifies, which is how a scheme migration stays non-breaking.
        assert!(verify(
            FIXTURE_SECRET,
            FIXTURE_TIMESTAMP,
            &format!("v2=deadbeef, {header}"),
            FIXTURE_BODY,
            FIXTURE_TIMESTAMP,
        ));
    }

    #[test]
    fn verification_refuses_a_wrong_secret_body_or_signature() {
        let header = sign(FIXTURE_SECRET, FIXTURE_TIMESTAMP, FIXTURE_BODY);
        let now = FIXTURE_TIMESTAMP;

        assert!(!verify(
            "whsec_other",
            FIXTURE_TIMESTAMP,
            &header,
            FIXTURE_BODY,
            now
        ));
        assert!(!verify(
            FIXTURE_SECRET,
            FIXTURE_TIMESTAMP,
            &header,
            "{}",
            now
        ));
        assert!(!verify(
            FIXTURE_SECRET,
            FIXTURE_TIMESTAMP,
            "v1=",
            FIXTURE_BODY,
            now
        ));
        assert!(!verify(
            FIXTURE_SECRET,
            FIXTURE_TIMESTAMP,
            "garbage",
            FIXTURE_BODY,
            now
        ));
        // A well-formed signature of the right length, for another message.
        let elsewhere = sign(FIXTURE_SECRET, FIXTURE_TIMESTAMP, "{\"other\":true}");
        assert!(!verify(
            FIXTURE_SECRET,
            FIXTURE_TIMESTAMP,
            &elsewhere,
            FIXTURE_BODY,
            now,
        ));
    }

    #[test]
    fn verification_refuses_a_replay_from_outside_the_skew_window() {
        let header = sign(FIXTURE_SECRET, FIXTURE_TIMESTAMP, FIXTURE_BODY);
        let window = i64::try_from(MAX_CLOCK_SKEW.as_secs()).expect("the window fits");

        assert!(verify(
            FIXTURE_SECRET,
            FIXTURE_TIMESTAMP,
            &header,
            FIXTURE_BODY,
            FIXTURE_TIMESTAMP + window,
        ));
        assert!(!verify(
            FIXTURE_SECRET,
            FIXTURE_TIMESTAMP,
            &header,
            FIXTURE_BODY,
            FIXTURE_TIMESTAMP + window + 1,
        ));
        // A clock that runs behind the sender's is refused just as far out.
        assert!(!verify(
            FIXTURE_SECRET,
            FIXTURE_TIMESTAMP,
            &header,
            FIXTURE_BODY,
            FIXTURE_TIMESTAMP - window - 1,
        ));
    }
}

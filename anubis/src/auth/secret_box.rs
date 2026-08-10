//! Authenticated encryption for secrets the application must read back.
//!
//! Almost every secret in Anubis is hashed and never recovered: passwords,
//! session tokens, and MFA recovery codes are compared by hash, so a stolen
//! database yields nothing usable. A few secrets cannot work that way. A TOTP
//! seed has to be read on every sign-in to compute the expected code, so it
//! must be stored recoverably. Those secrets live here, sealed with
//! AES-256-GCM under one application key, so a database dump on its own is not
//! enough to mint valid codes.
//!
//! [`encrypt`] returns a self-describing value:
//!
//! ```text
//! v1:<nonce>:<ciphertext>
//! ```
//!
//! `v1` names the scheme (AES-256-GCM with a fresh random 96-bit nonce per
//! message); both parts are base64url without padding, and the ciphertext
//! carries GCM's 16-byte authentication tag. A future scheme is added as `v2`
//! alongside `v1`, so stored values never need a migration to stay readable.
//!
//! The key travels through configuration rather than global state: load it
//! once from `ANUBIS_SECRET_KEY` into [`AppConfig::secret_key`], then hand a
//! [`SecretKey`] to each call.
//!
//! ```
//! use anubis::auth::secret_box::{self, SecretKey};
//!
//! let key = SecretKey::generate();
//! let sealed = secret_box::encrypt(&key, "JBSWY3DPEHPK3PXP");
//!
//! assert!(sealed.starts_with("v1:"));
//! assert_eq!(secret_box::decrypt(&key, &sealed)?, "JBSWY3DPEHPK3PXP");
//! # Ok::<(), anubis::auth::secret_box::Error>(())
//! ```
//!
//! [`AppConfig::secret_key`]: crate::config::AppConfig::secret_key

use std::backtrace::{Backtrace, BacktraceStatus};
use std::fmt::{self, Debug, Display, Formatter};

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_PAD_INDIFFERENT, URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

/// Bytes in an AES-256 key.
const KEY_BYTES: usize = 32;

/// Bytes in the per-message nonce. AES-GCM specifies 96 bits.
const NONCE_BYTES: usize = 12;

/// Names the current scheme in stored values: AES-256-GCM, random nonce.
///
/// Stored values start with this, so a later scheme can be introduced as `v2`
/// while `v1` values keep decrypting. Never reuse it for another scheme.
const VERSION: &str = "v1";

/// The literal the development key is derived from.
///
/// Fixed and public on purpose: development and test databases are disposable,
/// and a freshly stamped application must boot with no configuration. Anything
/// sealed under this key is readable by anyone holding the source, which is
/// why production refuses to start without `ANUBIS_SECRET_KEY`.
const DEVELOPMENT_SEED: &str = "anubis development secret key";

/// The application key that seals and opens recoverable secrets.
///
/// Loaded once at startup from `ANUBIS_SECRET_KEY`; see the module docs for
/// how it reaches the handlers that need it.
#[derive(Clone)]
pub struct SecretKey([u8; KEY_BYTES]);

impl SecretKey {
    /// Generates a fresh random key.
    ///
    /// Handy for tests and for producing the value an operator pastes into
    /// `ANUBIS_SECRET_KEY`, via [`SecretKey::to_base64`].
    ///
    /// # Panics
    /// Panics if the OS random source is unavailable, which makes every
    /// security-sensitive operation unsafe to continue.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0u8; KEY_BYTES];
        getrandom::fill(&mut bytes).expect("the OS random source must be available");
        Self(bytes)
    }

    /// Parses a base64-encoded key of exactly 32 bytes.
    ///
    /// The standard base64 alphabet is expected and padding is optional, so
    /// both `openssl rand -base64 32` output and unpadded generators work.
    ///
    /// # Errors
    /// Returns an [`Error`] when the input is not base64, or does not decode
    /// to exactly 32 bytes. The error never quotes the input, so it is safe to
    /// log.
    pub fn from_base64(encoded: &str) -> Result<Self, Error> {
        let decoded = STANDARD_PAD_INDIFFERENT
            .decode(encoded.trim())
            .map_err(|_error| Error::new("the key is not valid base64".to_owned()))?;

        let bytes: [u8; KEY_BYTES] = decoded.try_into().map_err(|decoded: Vec<u8>| {
            Error::new(format!(
                "the key must decode to exactly {KEY_BYTES} bytes, got {}",
                decoded.len()
            ))
        })?;

        Ok(Self(bytes))
    }

    /// Renders the key as padded base64, the form `ANUBIS_SECRET_KEY` takes.
    #[must_use]
    pub fn to_base64(&self) -> String {
        STANDARD.encode(self.0)
    }

    /// The built-in development key, derived from a fixed compile-time string.
    ///
    /// Every build produces the same key, so development databases survive
    /// restarts without any configuration. It is not a secret.
    #[must_use]
    pub fn development() -> Self {
        let digest = Sha256::digest(DEVELOPMENT_SEED.as_bytes());
        let mut bytes = [0u8; KEY_BYTES];
        bytes.copy_from_slice(&digest);
        Self(bytes)
    }

    /// Returns `true` when this is the built-in development key.
    ///
    /// Startup uses this to warn that stored secrets are protected by a public
    /// key.
    #[must_use]
    pub fn is_development(&self) -> bool {
        *self == Self::development()
    }
}

impl Debug for SecretKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(...)")
    }
}

impl PartialEq for SecretKey {
    /// Compares in constant time, so key comparison leaks nothing through
    /// timing.
    fn eq(&self, other: &Self) -> bool {
        let difference = self
            .0
            .iter()
            .zip(other.0)
            .fold(0u8, |accumulated, (left, right)| {
                accumulated | (left ^ right)
            });
        difference == 0
    }
}

impl Eq for SecretKey {}

/// Seals `plaintext` under `key` into the versioned storage form.
///
/// Every call draws a fresh random nonce, so sealing the same plaintext twice
/// yields different values. Sealed values are therefore never comparable for
/// equality; open them to compare.
///
/// # Panics
/// Panics if the OS random source is unavailable, or if `plaintext` exceeds
/// AES-GCM's message limit of roughly 64 GiB, which is a caller bug rather
/// than a runtime condition.
#[must_use]
pub fn encrypt(key: &SecretKey, plaintext: &str) -> String {
    let mut nonce = [0u8; NONCE_BYTES];
    getrandom::fill(&mut nonce).expect("the OS random source must be available");

    let ciphertext = cipher(key)
        .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
        .expect("AES-GCM only fails on messages beyond its 64 GiB limit");

    format!(
        "{VERSION}:{}:{}",
        URL_SAFE_NO_PAD.encode(nonce),
        URL_SAFE_NO_PAD.encode(&ciphertext)
    )
}

/// Opens a value produced by [`encrypt`].
///
/// # Errors
/// Returns an [`Error`] when `sealed` is not in the versioned form, names an
/// unknown scheme version, or fails authentication. That last case covers a
/// tampered value, a value sealed under a different key, and a value stored
/// before encryption existed; callers cannot recover any of them, so they
/// should treat the stored value as lost.
pub fn decrypt(key: &SecretKey, sealed: &str) -> Result<String, Error> {
    let mut parts = sealed.split(':');
    let (Some(version), Some(nonce), Some(ciphertext), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Error::new(
            "the value is not in the expected <version>:<nonce>:<ciphertext> form".to_owned(),
        ));
    };

    if version != VERSION {
        return Err(Error::new(format!(
            "unknown sealing version {version:?}, expected {VERSION:?}"
        )));
    }

    let nonce = URL_SAFE_NO_PAD
        .decode(nonce)
        .map_err(|_error| Error::new("the nonce is not valid base64".to_owned()))?;
    if nonce.len() != NONCE_BYTES {
        return Err(Error::new(format!(
            "the nonce must be {NONCE_BYTES} bytes, got {}",
            nonce.len()
        )));
    }

    let ciphertext = URL_SAFE_NO_PAD
        .decode(ciphertext)
        .map_err(|_error| Error::new("the ciphertext is not valid base64".to_owned()))?;

    let plaintext = cipher(key)
        .decrypt(Nonce::from_slice(&nonce), ciphertext.as_slice())
        .map_err(|_error| {
            Error::new(
                "the value failed authentication: it was tampered with, sealed under another \
                 key, or never sealed at all"
                    .to_owned(),
            )
        })?;

    String::from_utf8(plaintext)
        .map_err(|_error| Error::new("the opened value is not valid UTF-8".to_owned()))
}

fn cipher(key: &SecretKey) -> Aes256Gcm {
    Aes256Gcm::new_from_slice(&key.0).expect("a SecretKey is exactly one AES-256 key")
}

/// A key that does not parse, or a sealed value that cannot be opened.
///
/// Messages describe the shape of the failure and never include key material,
/// plaintext, or the offending input, so they are safe to log.
#[derive(Debug)]
pub struct Error {
    message: String,
    backtrace: Backtrace,
}

impl Error {
    fn new(message: String) -> Self {
        Self {
            message,
            backtrace: Backtrace::capture(),
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if self.backtrace.status() == BacktraceStatus::Captured {
            write!(f, "\n{}", self.backtrace)?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    use super::{NONCE_BYTES, SecretKey, decrypt, encrypt};

    /// A fixed key, so the fixture below stays reproducible.
    const FIXTURE_KEY: &str = "bkVLZLd1zHBqxWvKGKp5gRTZKcTf9UvHT5vXbHvWJ0M=";

    /// A value sealed by an earlier build of this module, kept to prove the
    /// `v1` format stays readable. Regenerating it would defeat its purpose:
    /// if this test fails, `v1` broke and needs a `v2` instead.
    const FIXTURE_SEALED: &str =
        "v1:biFd_HZ9vMtKhAxp:CvwyTd-bBuT2CvOq7V_DMxvrgqNwQWpdmpK_pJkweKkTzMjlHkLc0KMKt4ayVc51";

    /// The plaintext behind [`FIXTURE_SEALED`]: an RFC 6238 style TOTP seed.
    const FIXTURE_PLAINTEXT: &str = "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP";

    fn fixture_key() -> SecretKey {
        SecretKey::from_base64(FIXTURE_KEY).expect("the fixture key must parse")
    }

    #[test]
    fn sealed_values_open_back_to_the_plaintext() {
        let key = SecretKey::generate();

        for plaintext in ["", "JBSWY3DPEHPK3PXP", "unicode: ✅ ünïcödé"] {
            let sealed = encrypt(&key, plaintext);
            assert_eq!(
                decrypt(&key, &sealed).expect("our own value must open"),
                plaintext
            );
        }
    }

    #[test]
    fn sealing_twice_yields_different_values() {
        let key = SecretKey::generate();

        let first = encrypt(&key, "JBSWY3DPEHPK3PXP");
        let second = encrypt(&key, "JBSWY3DPEHPK3PXP");

        assert_ne!(first, second, "each message gets a fresh nonce");
        assert_eq!(decrypt(&key, &first).expect("opens"), "JBSWY3DPEHPK3PXP");
        assert_eq!(decrypt(&key, &second).expect("opens"), "JBSWY3DPEHPK3PXP");
    }

    #[test]
    fn the_sealed_form_is_versioned_and_self_describing() {
        let key = SecretKey::generate();
        let sealed = encrypt(&key, "JBSWY3DPEHPK3PXP");

        let parts: Vec<&str> = sealed.split(':').collect();
        assert_eq!(parts.len(), 3, "got: {sealed}");
        assert_eq!(parts[0], "v1");

        let nonce = URL_SAFE_NO_PAD
            .decode(parts[1])
            .expect("the nonce must be base64url");
        assert_eq!(nonce.len(), NONCE_BYTES);

        let ciphertext = URL_SAFE_NO_PAD
            .decode(parts[2])
            .expect("the ciphertext must be base64url");
        // GCM appends a 16-byte authentication tag to the plaintext length.
        assert_eq!(ciphertext.len(), "JBSWY3DPEHPK3PXP".len() + 16);

        assert!(!sealed.contains("JBSWY3DPEHPK3PXP"), "got: {sealed}");
    }

    #[test]
    fn the_v1_format_stays_readable() {
        let opened = decrypt(&fixture_key(), FIXTURE_SEALED).expect("v1 must keep opening");
        assert_eq!(opened, FIXTURE_PLAINTEXT);
    }

    #[test]
    fn another_key_cannot_open_the_value() {
        let sealed = encrypt(&SecretKey::generate(), "JBSWY3DPEHPK3PXP");

        let error = decrypt(&SecretKey::generate(), &sealed).expect_err("a wrong key must fail");
        assert!(error.to_string().contains("authentication"), "{error}");
    }

    #[test]
    fn tampering_is_detected() {
        let key = fixture_key();

        // Flip the last base64 character of the ciphertext.
        let mut tampered = FIXTURE_SEALED.to_owned();
        let last = tampered.pop().expect("the fixture is not empty");
        tampered.push(if last == 'A' { 'B' } else { 'A' });

        decrypt(&key, &tampered).expect_err("a tampered ciphertext must fail");

        // Swapping in a different (well-formed) nonce fails just as hard.
        let parts: Vec<&str> = FIXTURE_SEALED.split(':').collect();
        let other_nonce = URL_SAFE_NO_PAD.encode([0u8; NONCE_BYTES]);
        let renonced = format!("v1:{other_nonce}:{}", parts[2]);
        decrypt(&key, &renonced).expect_err("a swapped nonce must fail");
    }

    #[test]
    fn malformed_values_are_rejected_without_panicking() {
        let key = fixture_key();

        for malformed in [
            "",
            // What a pre-encryption development row looks like.
            "JBSWY3DPEHPK3PXP",
            "v1:only-two-parts",
            "v1:a:b:c",
            "v2:biFd_HZ9vMtKhAxp:CvwyTd-bBuT2CvOq7V_DMxvrgqNwQWpdmpK_pJkweKkTzMjlHkLc0KMKt4ayVc51",
            "v1:not base64!:CvwyTd-bBuT2",
            "v1:biFd_HZ9vMtKhAxp:not base64!",
            // A well-formed but too-short nonce.
            "v1:AAAA:CvwyTd-bBuT2",
        ] {
            decrypt(&key, malformed).expect_err(&format!("must reject {malformed:?}"));
        }
    }

    #[test]
    fn keys_parse_from_padded_and_unpadded_base64() {
        let padded = fixture_key();
        let unpadded = SecretKey::from_base64(FIXTURE_KEY.trim_end_matches('='))
            .expect("unpadded keys must parse");

        assert_eq!(padded, unpadded);
        assert_eq!(padded.to_base64(), FIXTURE_KEY);
    }

    #[test]
    fn keys_of_the_wrong_length_or_alphabet_are_rejected() {
        let short = SecretKey::from_base64("c2hvcnQ=").expect_err("16 bytes is not a key");
        assert!(short.to_string().contains("32 bytes"), "{short}");

        SecretKey::from_base64("not base64!!").expect_err("junk is not a key");
        SecretKey::from_base64("").expect_err("empty is not a key");
    }

    #[test]
    fn the_development_key_is_deterministic_and_recognizable() {
        assert_eq!(SecretKey::development(), SecretKey::development());
        assert!(SecretKey::development().is_development());
        assert!(!SecretKey::generate().is_development());
    }

    #[test]
    fn debug_output_never_leaks_key_material() {
        let key = fixture_key();
        let rendered = format!("{key:?}");

        assert!(rendered.contains("SecretKey"), "got: {rendered}");
        assert!(!rendered.contains(FIXTURE_KEY), "got: {rendered}");
        assert!(!rendered.contains("bkVLZ"), "got: {rendered}");
    }
}

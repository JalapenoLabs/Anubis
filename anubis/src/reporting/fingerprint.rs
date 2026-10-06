//! What two occurrences of one fault have in common, hashed.
//!
//! Deduplication is only as good as this. The same fault carries a different
//! request id, a different row id, and a different line and column in a freshly
//! hashed bundle every time it happens, so hashing what it said verbatim would
//! file a new issue per occurrence. [`fingerprint`] hashes a *normalized*
//! signature instead, and [`normalize`] is the part that decides what "the
//! same" means.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use super::Source;

/// Characters of the digest kept.
///
/// Sixteen hex characters is 64 bits, which will not collide across the
/// thousands of faults one deployment could ever record, and is short enough to
/// read out loud from an issue.
pub const FINGERPRINT_CHARS: usize = 16;

/// How many stack frames identify where a fault came from.
///
/// Three: enough that two call sites throwing the same words are two faults,
/// and few enough that a deeper frame differing (a different caller of the same
/// broken helper) does not split one fault into several issues.
pub const SIGNIFICANT_FRAMES: usize = 3;

/// Hashes `source`, `kind`, `subject` and the normalized `signature` together.
///
/// SHA-256, lowercase hex, truncated to [`FINGERPRINT_CHARS`]. Each part is
/// length-prefixed rather than joined by a separator, so a kind ending in the
/// separator cannot impersonate a different pairing.
///
/// **`subject` is hashed as written, never normalized.** It names the one thing
/// a fault is about when the same fault on two things is two problems, such as
/// a credential a vendor refused: normalization would mask its id and fold
/// every credential's refusal into one issue. Empty for most faults.
#[must_use]
pub fn fingerprint(source: Source, kind: &str, subject: &str, signature: &str) -> String {
    let mut hasher = Sha256::new();
    for part in [source.as_str(), kind, subject, &normalize(signature)] {
        hasher.update(part.len().to_le_bytes());
        hasher.update(part.as_bytes());
    }

    // Two hex characters per byte, so only the bytes that will be kept are
    // encoded at all.
    hasher.finalize()[..FINGERPRINT_CHARS / 2].iter().fold(
        String::with_capacity(FINGERPRINT_CHARS),
        |mut encoded, byte| {
            let _ = write!(encoded, "{byte:02x}");
            encoded
        },
    )
}

/// The first [`SIGNIFICANT_FRAMES`] non-empty lines of a stack, trimmed.
///
/// What a browser's or a panic's stack contributes to a signature: where the
/// fault was thrown, without the long tail of callers that varies by route.
#[must_use]
pub fn significant_frames(stack: &str) -> String {
    stack
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(SIGNIFICANT_FRAMES)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Strips the parts of a signature that change between occurrences.
///
/// Three substitutions, applied per token, where a token is a run of letters,
/// digits, hyphens and underscores:
///
/// | Token | Becomes |
/// |---|---|
/// | A uuid | `<id>` |
/// | Eight or more hex digits | `<hash>` |
/// | A run of digits inside anything else | `<n>` |
///
/// Masking digits also collapses distinct status codes and counts onto one
/// fingerprint. That is the intended trade: one issue for a family of related
/// faults rather than one issue each.
#[must_use]
pub fn normalize(signature: &str) -> String {
    let mut normalized = String::with_capacity(signature.len());
    let mut token = String::new();

    for character in signature.chars() {
        if character.is_alphanumeric() || character == '-' || character == '_' {
            token.push(character);
            continue;
        }

        push_token(&mut normalized, &token);
        token.clear();
        normalized.push(character);
    }

    push_token(&mut normalized, &token);
    normalized
}

/// Appends one token, masked if it looks like something that varies.
///
/// A uuid is judged whole, because its hyphens are part of its shape. Anything
/// else is judged piece by piece between hyphens and underscores, which is what
/// masks the hash in a bundle name like `index-a1b2c3d4.js` while leaving
/// `index` where somebody can read it.
fn push_token(normalized: &mut String, token: &str) {
    /// A uuid is 8-4-4-4-12 hex digits.
    const UUID_GROUPS: [usize; 5] = [8, 4, 4, 4, 12];

    if token.is_empty() {
        return;
    }

    let groups: Vec<&str> = token.split('-').collect();
    let is_uuid = groups.len() == UUID_GROUPS.len()
        && groups.iter().zip(UUID_GROUPS).all(|(group, length)| {
            group.len() == length && group.bytes().all(|byte| byte.is_ascii_hexdigit())
        });

    if is_uuid {
        normalized.push_str("<id>");
        return;
    }

    let mut piece = String::new();
    for character in token.chars() {
        if character == '-' || character == '_' {
            push_piece(normalized, &piece);
            piece.clear();
            normalized.push(character);
            continue;
        }

        piece.push(character);
    }

    push_piece(normalized, &piece);
}

/// Appends one hyphen-free piece of a token.
fn push_piece(normalized: &mut String, piece: &str) {
    /// Shortest run of hex digits treated as a hash rather than a word.
    ///
    /// Eight, because a short commit is seven and `deadbeef` is eight. Below
    /// this the false positives are ordinary words like `added` and `face`.
    const HEX_BLOB: usize = 8;

    if piece.is_empty() {
        return;
    }

    if piece.len() >= HEX_BLOB && piece.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        normalized.push_str("<hash>");
        return;
    }

    // Anything else keeps its shape with digit runs masked, so `line42` and
    // `line43` are one fault and `TypeError` stays itself.
    let mut in_digits = false;
    for character in piece.chars() {
        if character.is_ascii_digit() {
            if !in_digits {
                normalized.push_str("<n>");
                in_digits = true;
            }
            continue;
        }

        in_digits = false;
        normalized.push(character);
    }
}

#[cfg(test)]
mod tests {
    use super::{FINGERPRINT_CHARS, Source, fingerprint, normalize, significant_frames};

    #[test]
    fn the_same_fault_with_a_different_row_id_is_one_fingerprint() {
        // The entire point. These arrive from the same broken query, one per
        // request, and each carries the id of the row it choked on.
        let first = fingerprint(
            Source::Server,
            "http.request.failed",
            "",
            "no column `titel` on entry 019fe7a8-8b3c-7c21-9a44-1d2e3f405162",
        );
        let second = fingerprint(
            Source::Server,
            "http.request.failed",
            "",
            "no column `titel` on entry 019fea11-2c4d-7e88-b012-99aa88bb77cc",
        );

        assert_eq!(first, second);
    }

    #[test]
    fn a_different_fault_is_a_different_fingerprint() {
        assert_ne!(
            fingerprint(
                Source::Server,
                "http.request.failed",
                "",
                "no column `titel`"
            ),
            fingerprint(
                Source::Server,
                "http.request.failed",
                "",
                "no column `nmae`"
            ),
        );
    }

    #[test]
    fn the_source_and_the_kind_are_part_of_the_identity() {
        assert_ne!(
            fingerprint(Source::Server, "http.request.failed", "", "boom"),
            fingerprint(Source::Browser, "http.request.failed", "", "boom"),
        );
        assert_ne!(
            fingerprint(Source::Server, "http.request.failed", "", "boom"),
            fingerprint(Source::Server, "panic", "", "boom"),
        );
    }

    #[test]
    fn a_fingerprint_is_short_lowercase_hex_and_stable() {
        let value = fingerprint(Source::Worker, "panic", "", "boom");

        assert_eq!(value.len(), FINGERPRINT_CHARS);
        assert!(
            value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );
        assert_eq!(value, fingerprint(Source::Worker, "panic", "", "boom"));
    }

    #[test]
    fn a_subject_splits_a_fault_per_thing_and_is_never_normalized() {
        // Two credentials refused are two rotations. Normalized, both ids
        // would read `<id>` and fold into one issue.
        let first = fingerprint(
            Source::Worker,
            "rating.credential.refused",
            "019fe7a8-8b3c-7c21-9a44-1d2e3f405162",
            "refused as invalid",
        );
        let second = fingerprint(
            Source::Worker,
            "rating.credential.refused",
            "019fea11-2c4d-7e88-b012-99aa88bb77cc",
            "refused as invalid",
        );

        assert_ne!(first, second);
    }

    #[test]
    fn concatenation_cannot_be_forged_across_the_parts() {
        // Without length prefixes, a kind of "a" with a signature of "bc" and
        // a kind of "ab" with a signature of "c" hash identically.
        assert_ne!(
            fingerprint(Source::Server, "a", "", "bc"),
            fingerprint(Source::Server, "ab", "", "c"),
        );
    }

    #[test]
    fn normalize_masks_what_changes_between_occurrences() {
        assert_eq!(
            normalize("failed at line 42, column 7"),
            "failed at line <n>, column <n>"
        );
        assert_eq!(
            normalize("entry 019fe7a8-8b3c-7c21-9a44-1d2e3f405162 vanished"),
            "entry <id> vanished"
        );
        assert_eq!(
            normalize("chunk index-a1b2c3d4.js"),
            "chunk index-<hash>.js",
            "a bundle hash changes on every deploy"
        );
    }

    #[test]
    fn a_deploy_does_not_refile_every_open_fault() {
        // Every frame in a browser stack names the bundle, and Vite hashes it
        // on every build. Without the hash rule one deploy files a fresh issue
        // for every fault that was already open.
        let before = fingerprint(
            Source::Browser,
            "browser.render.failed",
            "",
            "TypeError: boom\n    at Row (/assets/index-a1b2c3d4.js:4:1128)",
        );
        let after = fingerprint(
            Source::Browser,
            "browser.render.failed",
            "",
            "TypeError: boom\n    at Row (/assets/index-99ffee11.js:4:2044)",
        );

        assert_eq!(before, after);
    }

    #[test]
    fn normalize_leaves_the_words_that_identify_a_fault() {
        // Over-normalizing is the opposite failure: every fault collapses onto
        // one issue and the reporting says nothing.
        assert_eq!(
            normalize("TypeError: pipeline.steps is not a function"),
            "TypeError: pipeline.steps is not a function"
        );
    }

    #[test]
    fn normalize_does_not_split_a_multibyte_character() {
        // A message can carry anything a browser threw, and byte slicing here
        // would panic on the one path that must not add a failure of its own.
        assert_eq!(normalize("échec 12 ✓"), "échec <n> ✓");
    }

    #[test]
    fn significant_frames_keep_the_top_of_the_stack() {
        let stack = "\n  at Row (index.js:4:1)\n\n  at Table (index.js:9:2)\n  at Page\n  at App\n";

        assert_eq!(
            significant_frames(stack),
            "at Row (index.js:4:1)\nat Table (index.js:9:2)\nat Page"
        );
    }
}

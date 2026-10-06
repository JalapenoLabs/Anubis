//! Masking secret-shaped text before a report is stored or filed.
//!
//! An error message is written by whatever failed, and what failed is often
//! holding a credential: a refused request quotes its `Authorization` header, a
//! database error quotes the connection string, a vendor quotes the key it
//! rejected. A report is stored in this deployment's database and then filed in
//! an issue tracker that more people read than run the deployment, so every
//! piece of text a [`super::Report`] holds passes through [`redact`] on the way
//! in, and nothing reaches either place unmasked.
//!
//! # What is masked
//!
//! | Shape | Example | Becomes |
//! |---|---|---|
//! | A PEM private key | `-----BEGIN PRIVATE KEY-----...` | `[redacted private key]` |
//! | A credential header | `Authorization: Bearer abc` | `Authorization: [redacted]` |
//! | Credentials in a URL | `postgres://app:hunter2@db/x` | `postgres://app:[redacted]@db/x` |
//! | A secret-named field | `password=hunter2`, `"token": "abc"` | `password=[redacted]` |
//! | A bearer or basic credential | `Bearer abc.def` | `Bearer [redacted]` |
//! | A vendor key | `ghp_...`, `github_pat_...`, `sk-ant-...`, `sk-...`, `AIza...`, `xoxb-...`, `AKIA...` | `[redacted secret]` |
//! | A JSON web token | `eyJ...eyJ...sig` | `[redacted secret]` |
//! | An email address | `ada@example.com` | `[redacted email]` |
//!
//! **Email addresses are always masked.** Nothing about a fault is easier to
//! fix for knowing whose address was involved, and an issue tracker is not
//! somewhere a privacy policy has promised to keep one.
//!
//! # What it cannot do
//!
//! It recognizes shapes, so a secret with no recognizable shape (a password
//! printed bare in the middle of a sentence) passes through. The first line of
//! defence is still the one the framework already asks for: never put a secret
//! into an error message. This is the second.

use std::sync::LazyLock;

use regex::Regex;

/// Every rule, in the order they are applied.
///
/// The order matters in two places. A private key goes first because its body
/// would otherwise be shredded into fragments by the token rules and partly
/// survive. Credentials in a URL go before email addresses, because
/// `app:hunter2@db.internal` is shaped like an address and masking it as one
/// would hide the host somebody needs to see.
///
/// Compiled once per process: a regex is expensive to build and cheap to run,
/// and this runs on every report.
static RULES: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
            "[redacted private key]",
        ),
        (
            r"(?i)\b(authorization|proxy-authorization|cookie|set-cookie|x-api-key|api-key)(\s*[:=]\s*)[^\r\n]+",
            "${1}${2}[redacted]",
        ),
        (
            r"(?i)\b([a-z][a-z0-9+.\-]*://[^\s:/@]+):[^\s@/]+@",
            "${1}:[redacted]@",
        ),
        (
            r#"(?i)\b(password|passwd|pwd|secret|client_secret|token|access_token|refresh_token|id_token|api_?key|session|session_?id|csrf)\b(["']?\s*[:=]\s*["']?)[^\s"'&,;}]+"#,
            "${1}${2}[redacted]",
        ),
        (
            r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=\-]{8,}",
            "${1} [redacted]",
        ),
        (
            r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-ant-[A-Za-z0-9_\-]{10,}|sk-[A-Za-z0-9_\-]{20,}|AIza[0-9A-Za-z_\-]{30,}|xox[abprs]-[A-Za-z0-9\-]{10,}|AKIA[0-9A-Z]{16})",
            "[redacted secret]",
        ),
        (
            r"\beyJ[A-Za-z0-9_\-]{5,}\.[A-Za-z0-9_\-]{5,}\.[A-Za-z0-9_\-]{5,}",
            "[redacted secret]",
        ),
        (
            r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}",
            "[redacted email]",
        ),
    ]
    .into_iter()
    .map(|(pattern, replacement)| {
        let rule = Regex::new(pattern).expect("every redaction rule is a valid pattern");
        (rule, replacement)
    })
    .collect()
});

/// Masks every secret-shaped span of `text`; see the module docs for which.
///
/// # Examples
/// ```
/// use anubis::reporting::redact;
///
/// let masked = redact("GET /x failed: Authorization: Bearer sk-ant-oat01-abc");
/// assert_eq!(masked, "GET /x failed: Authorization: [redacted]");
/// ```
#[must_use]
pub fn redact(text: &str) -> String {
    RULES
        .iter()
        .fold(text.to_owned(), |masked, (rule, replacement)| {
            rule.replace_all(&masked, *replacement).into_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn an_authorization_header_loses_its_value() {
        assert_eq!(
            redact("request failed\nAuthorization: Bearer abc.def.ghi\nAccept: */*"),
            "request failed\nAuthorization: [redacted]\nAccept: */*"
        );
    }

    #[test]
    fn a_cookie_loses_its_value() {
        assert_eq!(
            redact("cookie: anubis_session=6b1f0c2a9d; theme=dark"),
            "cookie: [redacted]"
        );
    }

    #[test]
    fn a_bare_bearer_credential_is_masked() {
        assert_eq!(
            redact("sent Bearer 0123456789abcdef to the vendor"),
            "sent Bearer [redacted] to the vendor"
        );
    }

    #[test]
    fn a_password_in_a_connection_string_is_masked_and_the_host_survives() {
        assert_eq!(
            redact("could not connect to postgres://benchmark:hunter2@10.0.0.4:5432/benchmark"),
            "could not connect to postgres://benchmark:[redacted]@10.0.0.4:5432/benchmark"
        );
    }

    #[test]
    fn secret_named_fields_are_masked_in_queries_and_in_json() {
        assert_eq!(
            redact("GET /callback?code=1&access_token=abc123&state=x"),
            "GET /callback?code=1&access_token=[redacted]&state=x"
        );
        assert_eq!(
            redact(r#"{"password": "hunter2", "user": "ada"}"#),
            r#"{"password": "[redacted]", "user": "ada"}"#
        );
    }

    #[test]
    fn vendor_keys_are_masked_wherever_they_appear() {
        for secret in [
            "ghp_0123456789abcdefghijABCDEFGHIJ",
            "github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyz",
            "sk-ant-oat01-Wc3pQ-example-token",
            "sk-proj-0123456789abcdefghijklmnop",
            "AIzaSyD-0123456789abcdefghijklmnopqrstu",
            "xoxb-123456789012-abcdefghij",
            "AKIAIOSFODNN7EXAMPLE",
        ] {
            let masked = redact(&format!("the vendor refused {secret} today"));
            assert_eq!(
                masked, "the vendor refused [redacted secret] today",
                "{secret} must be masked"
            );
        }
    }

    #[test]
    fn a_json_web_token_is_masked() {
        let token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";

        assert_eq!(
            redact(&format!("session {token} expired")),
            "session [redacted secret] expired"
        );
    }

    #[test]
    fn a_private_key_is_masked_whole() {
        let pem = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\nBKcwggSjAgEAAoIBAQC7\n-----END PRIVATE KEY-----";

        assert_eq!(
            redact(&format!("loaded {pem} from disk")),
            "loaded [redacted private key] from disk"
        );
    }

    #[test]
    fn an_email_address_is_always_masked() {
        assert_eq!(
            redact("no account for ada.lovelace+test@example.co.uk"),
            "no account for [redacted email]"
        );
    }

    #[test]
    fn ordinary_error_text_is_left_alone() {
        // Over-masking is its own failure: an issue whose text is all
        // brackets tells nobody what broke.
        for text in [
            "relation \"submissions\" does not exist",
            "TypeError: Cannot read properties of undefined (reading 'score')",
            "the token budget for today is spent",
            "connection refused (os error 111)",
            "GET /operator/llm-credentials/{id} answered 500",
        ] {
            assert_eq!(redact(text), text);
        }
    }
}

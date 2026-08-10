//! A Rust-first SaaS framework modeled on Bullet Train's developer experience.
//!
//! Anubis provides the plumbing that is the same in every SaaS product: teams-first
//! multi-tenancy, roles and permissions, authentication, a versioned REST API, and a
//! full-stack code generator. Applications are stamped from the starter template with
//! `anubis new` and grown with `anubis scaffold`.
//!
//! This crate is both the framework library and the `anubis` CLI binary. The library
//! surface grows milestone by milestone; the roadmap lives in the repository's
//! `docs/architecture.md`.

pub mod config;
pub mod telemetry;

/// The version of the Anubis framework, matching the crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_is_semver_shaped() {
        let mut parts = VERSION.split('.');
        let count = parts.clone().count();
        assert_eq!(count, 3, "expected MAJOR.MINOR.PATCH, got {VERSION}");
        assert!(
            parts.all(|part| part.parse::<u64>().is_ok()),
            "expected numeric semver components, got {VERSION}"
        );
    }
}

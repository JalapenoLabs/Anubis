//! Diesel schema for the framework-owned core tables.
//!
//! These definitions mirror the SQL migrations shipped in the crate's
//! `migrations/` directory and are kept in sync by hand. Application tables
//! live in the application's own schema module; both sides can join against
//! these tables freely.

diesel::table! {
    /// Registered user accounts. See the auth design in `docs/api.md`.
    users (id) {
        id -> Uuid,
        email -> Text,
        password_hash -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

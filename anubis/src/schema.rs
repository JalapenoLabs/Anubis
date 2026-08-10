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

diesel::table! {
    /// Browser sessions. Rows hold a hash of the session token, never the token.
    sessions (id) {
        id -> Uuid,
        user_id -> Uuid,
        token_hash -> Text,
        created_at -> Timestamptz,
        expires_at -> Timestamptz,
    }
}

diesel::joinable!(sessions -> users (user_id));
diesel::allow_tables_to_appear_in_same_query!(sessions, users);

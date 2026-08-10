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
        email_verified_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    /// Single-use tokens for email verification and password reset.
    /// Rows hold a hash of the token, never the token.
    user_tokens (id) {
        id -> Uuid,
        user_id -> Uuid,
        purpose -> Text,
        token_hash -> Text,
        created_at -> Timestamptz,
        expires_at -> Timestamptz,
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
diesel::joinable!(user_tokens -> users (user_id));
diesel::allow_tables_to_appear_in_same_query!(sessions, users);
diesel::allow_tables_to_appear_in_same_query!(user_tokens, users);

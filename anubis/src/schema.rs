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
        first_name -> Nullable<Text>,
        last_name -> Nullable<Text>,
        time_zone -> Text,
        locale -> Text,
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
        payload -> Nullable<Text>,
        attempts -> Int4,
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

diesel::table! {
    /// Top-level tenants. Every team belongs to exactly one organization.
    organizations (id) {
        id -> Uuid,
        name -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    /// Working tenants. All domain resources chain ownership back to a team.
    teams (id) {
        id -> Uuid,
        organization_id -> Uuid,
        name -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    /// Joins users to organizations, carrying org-level role keys.
    organization_memberships (id) {
        id -> Uuid,
        organization_id -> Uuid,
        user_id -> Uuid,
        roles -> Array<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    /// Joins users to teams, carrying team-level role keys. `user_id` is
    /// null for invited people who have not claimed the membership yet.
    team_memberships (id) {
        id -> Uuid,
        team_id -> Uuid,
        user_id -> Nullable<Uuid>,
        roles -> Array<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    /// Pending invitations to a team or an organization. Rows hold a hash of
    /// the invitation token, never the token.
    invitations (id) {
        id -> Uuid,
        email -> Text,
        organization_id -> Uuid,
        team_id -> Nullable<Uuid>,
        team_membership_id -> Nullable<Uuid>,
        roles -> Array<Text>,
        invited_by -> Nullable<Uuid>,
        token_hash -> Text,
        created_at -> Timestamptz,
        expires_at -> Timestamptz,
    }
}

diesel::table! {
    /// Per-team API credentials ("Developers" section). Tokens live in
    /// `platform_tokens`; this row is the named application.
    platform_applications (id) {
        id -> Uuid,
        team_id -> Uuid,
        name -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    /// Bearer tokens for platform applications. Rows hold a hash of the
    /// token, never the token.
    platform_tokens (id) {
        id -> Uuid,
        platform_application_id -> Uuid,
        token_hash -> Text,
        created_at -> Timestamptz,
        expires_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    /// Optimized avatar images, one per user, served at
    /// `/users/{user_id}/avatar`.
    user_avatars (user_id) {
        user_id -> Uuid,
        image -> Bytea,
        content_type -> Text,
        etag -> Text,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    /// TOTP enrollment, one per user. Only confirmed rows gate login.
    user_mfa (user_id) {
        user_id -> Uuid,
        totp_secret -> Text,
        confirmed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    /// Single-use MFA recovery codes. Rows hold a hash of the code.
    user_recovery_codes (id) {
        id -> Uuid,
        user_id -> Uuid,
        code_hash -> Text,
        used_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    /// Registered passkeys (WebAuthn credentials), any number per user.
    user_passkeys (id) {
        id -> Uuid,
        user_id -> Uuid,
        name -> Text,
        credential -> Jsonb,
        created_at -> Timestamptz,
        last_used_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    /// In-flight WebAuthn ceremonies. Rows hold a hash of the state token.
    webauthn_states (id) {
        id -> Uuid,
        purpose -> Text,
        user_id -> Nullable<Uuid>,
        token_hash -> Text,
        state -> Jsonb,
        created_at -> Timestamptz,
        expires_at -> Timestamptz,
    }
}

diesel::table! {
    /// Links a user account to its subject at an OpenID Connect provider.
    oauth_identities (id) {
        id -> Uuid,
        user_id -> Uuid,
        provider -> Text,
        subject -> Text,
        email -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    /// In-flight authorization-code flows. Rows hold a hash of the state token.
    oauth_states (id) {
        id -> Uuid,
        provider -> Text,
        token_hash -> Text,
        nonce -> Text,
        pkce_verifier -> Text,
        destination -> Nullable<Text>,
        created_at -> Timestamptz,
        expires_at -> Timestamptz,
    }
}

diesel::table! {
    /// Background work waiting to run. See the queue design in `docs/jobs.md`.
    jobs (id) {
        id -> Uuid,
        queue -> Text,
        kind -> Text,
        payload -> Jsonb,
        attempts -> Int4,
        max_attempts -> Int4,
        run_at -> Timestamptz,
        locked_at -> Nullable<Timestamptz>,
        locked_by -> Nullable<Text>,
        last_error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    /// Jobs that exhausted their attempts, kept for operators to inspect.
    dead_jobs (id) {
        id -> Uuid,
        queue -> Text,
        kind -> Text,
        payload -> Jsonb,
        attempts -> Int4,
        last_error -> Nullable<Text>,
        enqueued_at -> Timestamptz,
        failed_at -> Timestamptz,
    }
}

diesel::joinable!(sessions -> users (user_id));
diesel::joinable!(user_avatars -> users (user_id));
diesel::joinable!(user_mfa -> users (user_id));
diesel::joinable!(user_recovery_codes -> users (user_id));
diesel::joinable!(user_passkeys -> users (user_id));
diesel::joinable!(oauth_identities -> users (user_id));
diesel::joinable!(platform_applications -> teams (team_id));
diesel::joinable!(platform_tokens -> platform_applications (platform_application_id));
diesel::joinable!(invitations -> organizations (organization_id));
diesel::joinable!(invitations -> teams (team_id));
diesel::joinable!(user_tokens -> users (user_id));
diesel::joinable!(teams -> organizations (organization_id));
diesel::joinable!(organization_memberships -> organizations (organization_id));
diesel::joinable!(organization_memberships -> users (user_id));
diesel::joinable!(team_memberships -> teams (team_id));
diesel::joinable!(team_memberships -> users (user_id));
diesel::allow_tables_to_appear_in_same_query!(sessions, users);
diesel::allow_tables_to_appear_in_same_query!(oauth_identities, users);
diesel::allow_tables_to_appear_in_same_query!(user_tokens, users);
diesel::allow_tables_to_appear_in_same_query!(
    organizations,
    teams,
    organization_memberships,
    team_memberships,
    invitations,
    users,
);
diesel::allow_tables_to_appear_in_same_query!(platform_applications, platform_tokens, teams);

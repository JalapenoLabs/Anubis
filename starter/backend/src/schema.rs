//! Diesel schema for the application's own tables.
//!
//! The framework's core tables live in [`anubis::schema`]; this module holds
//! everything the application owns, mirroring the SQL in `backend/migrations`.
//! `anubis scaffold model` maintains it, inserting above the magic anchors,
//! so keep the anchors where they are.
//!
//! Application tables never appear in the same Diesel query as framework
//! tables. `allow_tables_to_appear_in_same_query!` and `joinable!` emit
//! trait implementations that Rust's orphan rules forbid across crates, so
//! the ownership chain is walked with a follow-up query against
//! `team_memberships` instead of a join. One extra indexed lookup buys a
//! schema that any application can own without patching the framework.

diesel::table! {
    /// The parent living template: a team-owned record.
    creative_concepts (id) {
        id -> Uuid,
        team_id -> Uuid,
        name -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    /// The child living template: belongs to a creative concept, which is how
    /// it reaches its owning team.
    tangible_things (id) {
        id -> Uuid,
        creative_concept_id -> Uuid,
        name -> Text,
        description -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// 🐺 anubis:tables

diesel::joinable!(tangible_things -> creative_concepts (creative_concept_id));
// 🐺 anubis:joins

diesel::allow_tables_to_appear_in_same_query!(creative_concepts, tangible_things);
// 🐺 anubis:same-query

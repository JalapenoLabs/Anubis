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
    /// One creative concept, owned by a team.
    creative_concepts (id) {
        id -> Uuid,
        team_id -> Uuid,
        name -> Text,
        description -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    /// One tangible thing, owned through its creative concept.
    tangible_things (id) {
        id -> Uuid,
        creative_concept_id -> Uuid,
        name -> Text,
        description -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    /// One peripheral notion, owned by a team.
    peripheral_notions (id) {
        id -> Uuid,
        team_id -> Uuid,
        name -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    /// One creative concept linked to one peripheral notion.
    incidental_linkages (id) {
        id -> Uuid,
        creative_concept_id -> Uuid,
        peripheral_notion_id -> Uuid,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// 🐺 anubis:tables

diesel::joinable!(tangible_things -> creative_concepts (creative_concept_id));
diesel::joinable!(incidental_linkages -> creative_concepts (creative_concept_id));
diesel::joinable!(incidental_linkages -> peripheral_notions (peripheral_notion_id));
// 🐺 anubis:joins

diesel::allow_tables_to_appear_in_same_query!(creative_concepts, tangible_things);
diesel::allow_tables_to_appear_in_same_query!(creative_concepts, incidental_linkages);
diesel::allow_tables_to_appear_in_same_query!(peripheral_notions, incidental_linkages);
// 🐺 anubis:same-query

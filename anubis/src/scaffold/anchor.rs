//! The magic anchor comments scaffolders insert above.
//!
//! An anchor is `🐺 anubis:<name>` inside the host language's comment syntax.
//! [`insert_above_anchor`](super::insert_above_anchor) matches the first
//! occurrence, so a spelling appears at most once per file, and anchors that
//! would otherwise repeat carry a suffix naming the section they belong to
//! (the role grants in `roles.yml`).
//!
//! These constants are the single source of the vocabulary that
//! `docs/scaffolding.md` documents and that every application file carries.
//! The comment marker itself is not part of an anchor: the same name matches
//! `// 🐺 anubis:routes` in Rust and `# 🐺 anubis:models:editor` in YAML.

/// `backend/src/lib.rs`: module declarations.
pub const MODULES: &str = "🐺 anubis:modules";

/// `backend/src/lib.rs`: router mounts in `account_router`.
pub const ROUTES: &str = "🐺 anubis:routes";

/// `backend/src/schema.rs`: `diesel::table!` blocks.
pub const TABLES: &str = "🐺 anubis:tables";

/// `backend/src/schema.rs`: `diesel::joinable!` declarations.
pub const JOINS: &str = "🐺 anubis:joins";

/// `backend/src/schema.rs`: `allow_tables_to_appear_in_same_query!` declarations.
pub const SAME_QUERY: &str = "🐺 anubis:same-query";

/// `config/roles.yml`: the `default` role's model grants.
pub const ROLES_DEFAULT: &str = "🐺 anubis:models:default";

/// `config/roles.yml`: the `editor` role's model grants.
pub const ROLES_EDITOR: &str = "🐺 anubis:models:editor";

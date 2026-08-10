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

/// `frontend/src/urls.ts`: `UrlTree` entries.
pub const URLS: &str = "🐺 anubis:urls";

/// `frontend/src/urls.ts`: link factory functions.
pub const URL_FACTORIES: &str = "🐺 anubis:url-factories";

/// `frontend/src/App.tsx`: page imports.
///
/// The `<Route>` elements themselves go above [`ROUTES`], which `App.tsx`
/// spells as a JSX comment and `lib.rs` as a Rust one.
pub const PAGE_IMPORTS: &str = "🐺 anubis:page-imports";

/// `frontend/src/components/AppShell.tsx`: navigation entries.
pub const NAV: &str = "🐺 anubis:nav";

/// `frontend/src/i18n.ts`: per-model locale imports.
pub const LOCALE_IMPORTS: &str = "🐺 anubis:locale-imports";

/// `frontend/src/i18n.ts`: per-model locale spreads.
pub const LOCALES: &str = "🐺 anubis:locales";

/// A show page: imports of the section components its children render through.
pub const CHILD_IMPORTS: &str = "🐺 anubis:child-imports";

/// A show page: the section elements of the models it owns.
pub const CHILDREN: &str = "🐺 anubis:children";

/// Marks a line that belongs to the living template alone.
///
/// The template's parent page renders the template's own child, which no other
/// model owns, so those lines are dropped when the page is stamped. It is a
/// marker rather than an insertion point: nothing is ever written above it.
pub const TEMPLATE_ONLY: &str = "🐺 anubis:template-only";

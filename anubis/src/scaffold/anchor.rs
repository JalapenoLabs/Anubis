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
//!
//! The vocabulary comes in two halves. The model anchors sit in files the
//! whole application shares, and `anubis scaffold model` inserts a model's
//! lines above them. The field anchors sit in a model's own artifacts, once
//! per list of columns, and `anubis scaffold field` inserts one field's lines
//! above them. Every artifact a scaffold stamps inherits the field anchors
//! from the living template it came from, so a generated model stays
//! field-scaffoldable forever.
//!
//! Two lists carry no anchor, because they repeat inside one file and an
//! anchor spelling may not: a `diesel::table!` block's columns and a locale
//! file's strings, which are JSON and cannot hold comments at all. Both are
//! written structurally instead, inside the block that names the model.

/// `backend/src/lib.rs`: module declarations.
pub const MODULES: &str = "🐺 anubis:modules";

/// `backend/src/lib.rs`: router mounts in `account_router`.
pub const ROUTES: &str = "🐺 anubis:routes";

/// `backend/src/lib.rs`: router mounts in `api_v1_router`.
pub const API_ROUTES: &str = "🐺 anubis:api-routes";

/// `backend/src/lib.rs`: the per-model merges the application's document takes.
pub const API_DOCS: &str = "🐺 anubis:api-docs";

/// `backend/src/lib.rs`: router mounts in `webhooks_router`.
///
/// Its own anchor rather than [`ROUTES`], because the two routers answer
/// different questions: `/account` is mounted behind the session guards and
/// `/webhooks` is deliberately in front of them.
pub const WEBHOOK_ROUTES: &str = "🐺 anubis:webhook-routes";

/// `backend/src/lib.rs`: job registrations in `register_jobs`.
pub const JOBS: &str = "🐺 anubis:jobs";

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

/// A model's `model.rs`: the inherent methods an association adds.
///
/// A `belongs_to` needs two of them, its `valid_*` scoping method and the label
/// lookup a page of records is serialized through, so the model's own `impl`
/// block carries an insertion point the way its struct definitions do.
pub const MODEL_METHODS: &str = "🐺 anubis:model-methods";

/// A model's `model.rs`: the record struct's columns.
pub const RECORD_FIELDS: &str = "🐺 anubis:record-fields";

/// A model's `model.rs`: the insertable struct's columns.
pub const INSERT_FIELDS: &str = "🐺 anubis:insert-fields";

/// A model's `model.rs`: the changeset struct's columns.
pub const CHANGESET_FIELDS: &str = "🐺 anubis:changeset-fields";

/// A model's `model.rs`: the changeset's "was anything submitted" test.
pub const CHANGESET_EMPTY: &str = "🐺 anubis:changeset-empty";

/// A model's `routes.rs`: the account routes an association mounts.
///
/// It sits between the model's own `.route(...)` calls and `.with_state(...)`,
/// so an inserted mount joins the builder chain already formatted.
pub const ACCOUNT_ROUTES: &str = "🐺 anubis:account-routes";

/// A model's `routes.rs`: the handlers an association's routes dispatch to.
pub const HANDLERS: &str = "🐺 anubis:handlers";

/// A model's `routes.rs`: the create request body's members.
pub const CREATE_BODY: &str = "🐺 anubis:create-body";

/// A model's `routes.rs`: the update request body's members.
pub const UPDATE_BODY: &str = "🐺 anubis:update-body";

/// A model's `routes.rs`: the create handler's normalization bindings.
pub const CREATE_NORMALIZE: &str = "🐺 anubis:create-normalize";

/// A model's `routes.rs`: the insertable struct literal the create handler builds.
pub const INSERT_VALUES: &str = "🐺 anubis:insert-values";

/// A model's `routes.rs`: the update handler's normalization bindings.
pub const UPDATE_NORMALIZE: &str = "🐺 anubis:update-normalize";

/// A model's `routes.rs`: the changeset struct literal the update handler builds.
pub const CHANGESET_VALUES: &str = "🐺 anubis:changeset-values";

/// A model's `routes.rs`: the association reconciliations the create handler runs.
pub const CREATE_ASSOCIATIONS: &str = "🐺 anubis:create-associations";

/// A model's `routes.rs`: the association reconciliations the update handler runs.
pub const UPDATE_ASSOCIATIONS: &str = "🐺 anubis:update-associations";

/// A model's `routes.rs`: the view struct's association members.
pub const VIEW_FIELDS: &str = "🐺 anubis:view-fields";

/// A model's `routes.rs`: the association loads a page of records needs.
pub const VIEW_LOAD: &str = "🐺 anubis:view-load";

/// A model's `routes.rs`: the view struct literal one record is built into.
pub const VIEW_VALUES: &str = "🐺 anubis:view-values";

/// A model's integration test: the create request's payload.
pub const TEST_CREATE: &str = "🐺 anubis:test-create";

/// A model's integration test: the assertions on the created record.
pub const TEST_CREATED: &str = "🐺 anubis:test-created";

/// A model's integration test: the update request's payload.
pub const TEST_UPDATE: &str = "🐺 anubis:test-update";

/// A model's integration test: the assertions on the updated record.
pub const TEST_UPDATED: &str = "🐺 anubis:test-updated";

/// A model's integration test: the narrative an association adds.
///
/// The four anchors above extend a request that already exists; an
/// association's options endpoint and its assignment are requests of their
/// own, so they need statement position rather than a payload member.
pub const TEST_ASSOCIATIONS: &str = "🐺 anubis:test-associations";

/// A model's route module: the wire type's members.
pub const WIRE_FIELDS: &str = "🐺 anubis:wire-fields";

/// A model's route module: the request functions an association adds.
pub const ROUTE_FUNCTIONS: &str = "🐺 anubis:route-functions";

/// A model's route module: the create request type's members.
pub const CREATE_REQUEST: &str = "🐺 anubis:create-request";

/// A model's route module: the update request type's members.
pub const UPDATE_REQUEST: &str = "🐺 anubis:update-request";

/// A model's form: the field components it imports from the field library.
pub const FIELD_IMPORTS: &str = "🐺 anubis:field-imports";

/// A model's form: the application modules a field's control reads from.
pub const FORM_IMPORTS: &str = "🐺 anubis:form-imports";

/// A model's form: the hooks a field's control needs, such as its options.
pub const FORM_HOOKS: &str = "🐺 anubis:form-hooks";

/// A model's form: the zod schema's members.
pub const FORM_SCHEMA: &str = "🐺 anubis:form-schema";

/// A model's form: the form values built from the record being edited.
pub const FORM_VALUES: &str = "🐺 anubis:form-values";

/// A model's form: the payload the form submits.
pub const FORM_PAYLOAD: &str = "🐺 anubis:form-payload";

/// A model's form: the field components it renders.
pub const FORM_FIELDS: &str = "🐺 anubis:form-fields";

/// A table of the model's records: the column headers.
pub const LIST_COLUMNS: &str = "🐺 anubis:list-columns";

/// A table of the model's records: the cells of one row.
pub const LIST_CELLS: &str = "🐺 anubis:list-cells";

/// A show page: the record's own attributes.
pub const SHOW_FIELDS: &str = "🐺 anubis:show-fields";

/// Marks a line that belongs to the living template alone.
///
/// The template's parent page renders the template's own child, which no other
/// model owns, so those lines are dropped when the page is stamped. It is a
/// marker rather than an insertion point: nothing is ever written above it.
pub const TEMPLATE_ONLY: &str = "🐺 anubis:template-only";

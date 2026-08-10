# Scaffolding

Scaffolding is the crown jewel of Anubis, a 1:1 match of Bullet Train's Super Scaffolding philosophy: one command produces a production-ready, permission-scoped, API-backed, fully tested CRUD feature across the entire stack.

## Philosophy: living templates

Templates are real, functional, compiling code, not a DSL. The generator transforms template files into your model's names and namespaces, and the output is standard Rust and standard React that you own and edit freely.

Generated files contain magic anchor comments (`// 🐺 anubis:has-many`, `{/* 🐺 anubis:nav */}`) that later scaffold commands use as insertion targets. Do not delete them. This is exactly Bullet Train's magic-comment mechanism, and it is what makes `scaffold field` able to keep editing files you have customized.

The template models mirror Bullet Train's naming for the same reason Bullet Train chose it: `scaffolding::absolutely_abstract::CreativeConcept` (parent) and `scaffolding::completely_concrete::TangibleThing` (child) carry enough namespacing fidelity to transform into any real-world combination of parent and child namespaces. They live in the starter host app as compiling, CI-tested code, so the templates can never rot.

## The template host app

The templates are ordinary application code in `starter/`, exercised by `starter/backend/tests/scaffolding_flow.rs` against a real Postgres. `CreativeConcept` belongs to a Team and carries a `name`; `TangibleThing` belongs to a `CreativeConcept` and carries a `name` and a nullable `description`. Between them they cover every artifact one `scaffold model` run produces, which is what makes them a specification rather than a demo.

The starter backend is a library plus a thin binary. `main.rs` is the composition root; the application itself (models, routes, schema, migrations, role constants) lives in `lib.rs` and its modules, so integration tests drive the real routers.

### Anchor vocabulary

Anchors are `🐺 anubis:<name>` inside the host language's comment syntax. `insert_above_anchor` finds the first occurrence only, so an anchor spelling appears at most once per file.

| Anchor | File | Insertion point |
|---|---|---|
| `// 🐺 anubis:modules` | `backend/src/lib.rs` | module declarations |
| `// 🐺 anubis:routes` | `backend/src/lib.rs` | router mounts in `account_router` |
| `// 🐺 anubis:tables` | `backend/src/schema.rs` | `diesel::table!` blocks |
| `// 🐺 anubis:joins` | `backend/src/schema.rs` | `diesel::joinable!` declarations |
| `// 🐺 anubis:same-query` | `backend/src/schema.rs` | `allow_tables_to_appear_in_same_query!` declarations |
| `# 🐺 anubis:models:default` | `config/roles.yml` | the `default` role's model grants |
| `# 🐺 anubis:models:editor` | `config/roles.yml` | the `editor` role's model grants |
| `// 🐺 anubis:urls` | `frontend/src/urls.ts` | `UrlTree` entries |
| `// 🐺 anubis:url-factories` | `frontend/src/urls.ts` | link factory functions |
| `{/* 🐺 anubis:routes */}` | `frontend/src/App.tsx` | `<Route>` elements |
| `{/* 🐺 anubis:nav */}` | `frontend/src/components/AppShell.tsx` | navigation entries |
| `// 🐺 anubis:locale-imports` | `frontend/src/i18n.ts` | per-model locale imports |
| `// 🐺 anubis:locales` | `frontend/src/i18n.ts` | per-model locale spreads |

Each `roles.yml` anchor names its role: grants differ per role, and one insertion point can only be found once. `default` gets `read` and `editor` gets `manage`; `billing` and `admin` inherit and need no entries. A role added by hand carries its own anchor.

`account_router` mounts one router per statement rather than one long method chain, so an inserted line is already `rustfmt`-clean.

### App-owned migrations and schema

Application migrations live in `starter/backend/migrations/`, are embedded with `diesel_migrations::embed_migrations!`, and are applied at boot by `anubis::db::run_app_migrations` **after** `run_pending_migrations`: application tables reference `teams`, and the shared `set_updated_at()` trigger function must already exist. Every generated table attaches that trigger.

`starter/backend/src/schema.rs` holds the application's `table!` blocks. Application tables never appear in the same Diesel query as framework tables: `allow_tables_to_appear_in_same_query!` and `joinable!` emit trait implementations that Rust's orphan rules forbid across crates. The ownership chain therefore ends in one extra indexed lookup, `anubis::tenancy::TeamMembership::for_user(connection, user_id, team_id)`, rather than a join. Application tables join each other freely.

### Authorization in generated handlers

Collection routes hang off the team (`/account/teams/{team_id}/creative-concepts`) and use the `TeamMember` guard directly. Member routes are shallow (`/account/creative-concepts/{id}`, `/account/tangible-things/{id}`), so they resolve the chain with the model's own `load_for_member`, then authorize against the compiled `RoleSet` held in router state. Both paths answer `404` for records the caller cannot reach, so an id probe cannot tell a missing record from another tenant's.

### Frontend slice

The app's own endpoints get a ky client in `frontend/src/api/index.ts` and one route module per model in `frontend/src/api/routes/`. Wire types keep snake_case field names, because those names are the contract.

JSON carries no comments, so each scaffolded model gets its own locale file at `frontend/src/locales/models/<models>.<locale>.json`, and `i18n.ts` carries the anchors that import and merge them. The base application strings stay in `locales/en-US.json`.

### Deferred: `/api/v1` for application models

A scaffolded model currently generates account handlers only. Extending the framework-owned v1 OpenAPI document from an application is its own design problem (who owns the document, how an application merges paths into it, how versions freeze per application), and it is settled with the `scaffold model` generator itself rather than here. Until then the template stays honest: no `/api/v1` handlers, no half-built merge hook.

## CLI surface

| Command | Purpose |
|---|---|
| `anubis new <name>` | Stamp a new application from the starter template |
| `anubis scaffold model <Model> <ParentChain> <field:type ...>` | Full-stack CRUD scaffold |
| `anubis scaffold field <Model> <field:type>` | Add a field to an existing model, propagated everywhere |
| `anubis scaffold join <JoinModel> <a_id{class=A}> <b_id{class=B}>` | Join model for has-many-through |
| `anubis scaffold oauth <provider>` | Add an OAuth login provider (the one-line Google Auth moment) |
| `anubis scaffold webhook <name>` | Incoming webhook endpoint |
| `anubis routes` | Print the route table |
| `anubis eject <component>` | Copy a framework frontend component into the app to own it |
| `anubis doctor` | Verify toolchain, database, and config health |

`anubis new`, `anubis routes`, and `anubis doctor` are implemented; the `scaffold` family and `eject` are the rest of M4 and M5.

Field types map to the field component library: `text_field`, `text_area`, `number_field`, `email_field`, `phone_field`, `password_field`, `boolean`, `buttons`, `options`, `super_select`, `date_field`, `date_and_time_field`, `color_picker`, `emoji_field`, `rich_text`, `code_editor`, `file_field`, `image`, `address_field`. Modifiers follow Bullet Train: `{readonly}`, `{multiple}`, `{class_name=...}`, `{source=...}`.

## The stamping engine

All scaffolders share one pure engine, `anubis::scaffold`:

- **Names**: one model name in, every casing and plural variant out (`TangibleThing`, `tangible_things`, `tangible-thing`, `Tangible Things`, ...). Pluralization covers standard English rules plus a table of common irregulars.
- **Replacements**: ordered find-and-replace over paths and file bodies, longest pattern first so `tangible_things` wins over `tangible_thing`. `Replacements::between(template, target)` maps every variant pair at once.
- **Anchor insertion**: `insert_above_anchor` adds generated lines above a magic anchor comment, matching its indentation, and is idempotent so re-running a scaffold never duplicates lines.

The engine does no file I/O; the CLI is its thin filesystem shell. That split keeps every transform unit-testable as plain strings.

## How `anubis new` works

The starter tree is embedded into the `anubis` binary at build time, so stamping is offline and always matches the installed framework version. Stamping rewrites the app name across every path and file, then overlays the files that make the result a standalone repository: a workspace `Cargo.toml` carrying the framework's lint bar, a standalone `backend/Cargo.toml`, a root `package.json`, `.yarnrc.yml`, `.gitignore`, `README.md`, and the toolchain and clippy pins. Until the crate and npm package are published, stamped apps depend on the framework from its git repository (Cargo git dependency; yarn `#workspace=` git protocol). A drift-gate test pins the overlay's dependency versions to the framework workspace.

## Route visibility

`anubis routes` prints the framework's mounted surface from two sources: a curated manifest in `anubis::manifest` (drift-gated by a test that composes the real routers and probes every entry) and the OpenAPI document, which contributes every versioned `/api/v1` operation automatically. Application-defined routes live in the application's router and are not visible to the CLI.

## What one `scaffold model` produces

Backend:

- Diesel migration and `schema.rs` update
- Model struct with the ownership chain, validations, and stubbed `valid_*` scoping methods
- Entry in `roles.yml` permission grants
- Account CRUD handlers and `/api/v1` handlers (separate, like Bullet Train's account vs api controllers)
- Serializer registered with utoipa (OpenAPI 3.1), shared by the API and outgoing webhooks
- Routes wired into the router, integration tests

Frontend:

- Generated ky route functions and SWR hooks from the refreshed OpenAPI document
- List page (table), show page, and create/edit form pages built from field components
- Navigation entry and breadcrumbs
- Per-model i18next locale file (labels, headings, placeholders, help text, option lists)
- Vitest unit tests and Playwright E2E tests

`scaffold field` propagates a new attribute through every one of those artifacts, which is the feature that makes the framework compound over time.

## Locked conventions the generator stamps

- **List endpoints** follow the page/limit, sort, and filter conventions in [api.md](api.md); the scaffolder maintains each model's sortable and filterable whitelists. `anubis::http::ListParams` and `anubis::http::Pagination` implement the convention once, so every generated endpoint pages and sorts identically.
- **Scoping methods** (`valid_*`): for every association field, the scaffolder generates an inherent method on the model, `valid_<association>(connection, team) -> QueryResult<Vec<_>>`, stubbed with a `todo!`-style prompt for the developer to fill. The same method populates the select options endpoint and validates submitted ids on create and update, so a form can never smuggle in another tenant's record. One definition, both duties.
- **Timestamps**: `created_at`/`updated_at` come from the database. `updated_at` is maintained by the shared `set_updated_at()` trigger, attached to every generated table; application code never sets either.

## Workflow

Domain modeling comes first and scaffolding makes it cheap, so follow Bullet Train's method: write the scaffold commands in a scratch file, review them with people before running them, run them, commit the generated code in its own commit, then polish the UX. Tearing down and re-scaffolding is cheap; live with a wrong domain model is not.

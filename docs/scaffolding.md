# Scaffolding

Scaffolding is the crown jewel of Anubis, a 1:1 match of Bullet Train's Super Scaffolding philosophy: one command produces a production-ready, permission-scoped, API-backed, fully tested CRUD feature across the entire stack.

## Philosophy: living templates

Templates are real, functional, compiling code, not a DSL. The generator transforms template files into your model's names and namespaces, and the output is standard Rust and standard React that you own and edit freely.

Generated files contain magic anchor comments (`// 🐺 anubis:has-many`, `{/* 🐺 anubis:nav */}`) that later scaffold commands use as insertion targets. Do not delete them. This is exactly Bullet Train's magic-comment mechanism, and it is what makes `scaffold field` able to keep editing files you have customized.

The template models mirror Bullet Train's naming for the same reason Bullet Train chose it: `scaffolding::absolutely_abstract::CreativeConcept` (parent) and `scaffolding::completely_concrete::TangibleThing` (child) carry enough namespacing fidelity to transform into any real-world combination of parent and child namespaces. They live in the starter host app as compiling, CI-tested code, so the templates can never rot.

## The template host app

The templates are ordinary application code in `starter/`. `CreativeConcept` belongs to a Team; `TangibleThing` belongs to a `CreativeConcept`. Both carry a required `name` and a nullable `description`, so the two ownership depths differ only in ownership. Between them they cover every artifact one `scaffold model` run produces, which is what makes them a specification rather than a demo.

Each depth has its own narrative test, `starter/backend/tests/creative_concepts_flow.rs` and `starter/backend/tests/tangible_things_flow.rs`, running against a real Postgres. They are templates too: one scaffold stamps the matching narrative for the generated model, so a new model arrives with the same proof its template carries. The plumbing they share (booting the router, registering an account, inviting a teammate) lives in `starter/backend/tests/support/mod.rs`, which is application code the scaffolder never rewrites.

The starter backend is a library plus a thin binary. `main.rs` is the composition root; the application itself (models, routes, schema, migrations, role constants) lives in `lib.rs` and its modules, so integration tests drive the real routers. Model modules are public, because an application's library is what its binary and its tests build on.

Template files are written so that name-for-name transformation is enough: every local identifier and every sentence uses the model's own vocabulary (`CreativeConceptState`, `creative_concept_id`, "Name the creative concept."), never an abbreviation. An abbreviation would survive the transform and land in generated code as a name from another model.

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
| `anubis scaffold model <Model> <ParentChain> <field:type ...>` | Full-stack CRUD scaffold (backend today) |
| `anubis scaffold field <Model> <field:type>` | Add a field to an existing model, propagated everywhere |
| `anubis scaffold join <JoinModel> <a_id{class=A}> <b_id{class=B}>` | Join model for has-many-through |
| `anubis scaffold oauth <provider>` | Add an OAuth login provider (the one-line Google Auth moment) |
| `anubis scaffold webhook <name>` | Incoming webhook endpoint |
| `anubis routes` | Print the route table |
| `anubis eject <component>` | Copy a framework frontend component into the app to own it |
| `anubis doctor` | Verify toolchain, database, and config health |

`anubis new`, `anubis routes`, `anubis doctor`, and the backend half of `anubis scaffold model` are implemented; the frontend half of `scaffold model`, the rest of the `scaffold` family, and `eject` are the remainder of M4 and M5.

Field types map to the [field component library](#the-field-component-library): `text_field`, `text_area`, `number_field`, `email_field`, `phone_field`, `password_field`, `boolean`, `buttons`, `options`, `super_select`, `date_field`, `date_and_time_field`, `color_picker`, `emoji_field`, `rich_text`, `code_editor`, `file_field`, `image`, `address_field`. Modifiers follow Bullet Train: `{readonly}`, `{multiple}`, `{class_name=...}`, `{source=...}`.

The generator accepts the types the living templates prove. Each row knows its column type, its Diesel schema type, its Rust type, and whether the column is nullable; later issues extend the table rather than the code around it, and an unsupported type is refused by name with the supported list.

| Field type | Column | Schema type | Rust type | Nullable |
|---|---|---|---|---|
| `text_field` | `TEXT` | `Text` | `String` | no |
| `text_area` | `TEXT` | `Text` | `Option<String>` | yes |

## `anubis scaffold model`: the backend slice

```
anubis scaffold model Project Team name:text_field
anubis scaffold model Goal Project,Team name:text_field description:text_area
```

The command runs inside an application, which is a directory holding `backend/`, `frontend/`, and `config/roles.yml`; the search walks up from the working directory, so it works from anywhere inside one, and inside this repository it finds `starter/`. Anywhere else it stops and says so.

The ownership chain ends in `Team`, because every application record reaches a team. `Team` selects the team-owned template and `<Parent>,Team` the nested one. Deeper chains are refused with a pointer at the roadmap rather than generated half-right.

One run produces:

- a timestamped migration (`up.sql` and `down.sql`) with the table, its ownership index, and the shared `set_updated_at()` trigger
- a `diesel::table!` block in `backend/src/schema.rs`, plus the `joinable!` and `allow_tables_to_appear_in_same_query!` declarations for a nested model
- the model's module (`mod.rs`, `model.rs`, `routes.rs`) under `backend/src/<models>/`, carrying the ownership chain, the list conventions, the `valid_*` scoping methods, and account CRUD handlers
- the module declaration and router mount in `backend/src/lib.rs`
- `read` and `manage` grants in `config/roles.yml`, and a regenerated `frontend/src/roles.generated.ts`
- the model's own integration test in `backend/tests/<models>_flow.rs`

Every artifact is a transformation of the application's own files: the migration comes from the migration that created the template's table, the schema block from the template's `table!` block, the module from the template module, the test from the template's narrative. Improving a template improves every later scaffold.

The run is planned before anything is written, so a missing template, a missing anchor, or an existing module stops the command with the application untouched. Anchor insertions are idempotent, and a model whose module already exists is refused rather than overwritten. Generated Rust is formatted with `rustfmt` when it is on `PATH`: transformation cannot preserve line widths, since a shorter model name lets a wrapped statement fit again, and the formatter settles it.

### Fields today, and the honest gap

Every scaffolded model carries the template's own columns: `name` (required text) and `description` (optional text), wired end to end through the model, the handlers, and the test. Naming either in the field list is the identity case; naming one with the other type is refused.

Any other field reaches the migration and `schema.rs` as a nullable column, and nothing else. Nullable is deliberate: nothing writes the column yet, and a `NOT NULL` column with no writer would fail every insert. The generated `model.rs` opens with a `TODO(anubis)` comment naming each field and every place it still needs (the record, insert, and changeset structs, the request bodies, the create and update handlers), and the command says the same at the end of its output. `anubis scaffold field` closes this gap by adding the per-field anchors those files need.

Cosmetic limitation: prose in doc comments is transformed word for word, not rewrapped, so a much shorter or much longer model name leaves a ragged comment line. Comments never affect `cargo fmt --check`.

## The field component library

Bullet Train's field partials are its forms backbone. Ours are React components in `@jalapenolabs/anubis`, one per scaffolder field type, exported by name from the package root. A generated form is one component per model attribute with nothing in between.

### One wrapper, one contract

Every field composes `FieldWrapper`, which owns the label, the required marker, the control, and one line of help or error text. Fields render their label through the wrapper rather than through the control's own label prop, so a text input, a switch, and a radio group line up on the same grid; controls take `aria-label` for their accessible name. The error message replaces the help text while a field is invalid.

| Wrapper prop | Meaning |
|---|---|
| `htmlFor` | The control id the label points at |
| `label` | The label text, already translated |
| `isRequired` | Renders the required marker |
| `help` | Hint text, shown while the field is valid |
| `error` | Message shown in place of the help text |
| `className` | Extra classes on the wrapper, not the control |

Every field accepts `AnubisFieldProps`: `control` and `name` for react-hook-form, then `label`, `help`, `placeholder`, `error`, `isRequired`, `isDisabled`, `isReadOnly`, `autoFocus`, `id`, and `className`. `isReadOnly` is the `{readonly}` modifier. Choice fields add `options: FieldOption[]`.

The prop names mirror the locale keys the scaffolder emits, so a generated form reads `label={t('tangibleThings.name')}` and `help={t('tangibleThings.nameHelp')}` straight from the model's locale file.

### react-hook-form and i18n

Each field binds itself with `useController` through the shared `useFieldState` hook, so a form passes `control` and `name` and nothing else. The hook resolves the display state in one place: an explicit `error` prop wins over the resolver's message, and a field with a message is invalid. Applications that add their own field types call `useFieldState` and `FieldWrapper` to inherit the same behavior.

The library never imports i18next. The application owns translation and passes `t(...)` results down as plain strings. Resolver messages surface automatically, and a form that needs a translated message passes `error` instead.

### What ships today

| Field type | Component | Control |
|---|---|---|
| `text_field` | `TextField` | HeroUI Input |
| `text_area` | `TextAreaField` | HeroUI Textarea |
| `number_field` | `NumberField` | HeroUI Input, holding a `number` or `null` |
| `email_field` | `EmailField` | HeroUI Input, email keyboard and autofill |
| `password_field` | `PasswordField` | HeroUI Input, masked |
| `phone_field` | `PhoneField` | HeroUI Input, dial keyboard |
| `boolean` | `BooleanField` | HeroUI Switch |
| `buttons` | `ButtonsField` | HeroUI ButtonGroup as a segmented single choice |
| `options` | `OptionsField` | HeroUI Select, or RadioGroup with `variant='radio'` |
| `super_select` | `SuperSelectField` | HeroUI Autocomplete, single or multiple with chips |
| `date_field` | `DateField` | HeroUI Input, storing `YYYY-MM-DD` verbatim |
| `date_and_time_field` | `DateAndTimeField` | HeroUI Input, storing UTC and editing local |
| `color_picker` | `ColorPickerField` | HeroUI Input plus a native color swatch |

A boolean is a switch, not a checkbox: a boolean column is a setting that is on or off, and a switch says so at a glance. Checkboxes stay with selection lists, where "include this one" is the meaning.

Dates carry no timezone, so `DateField` stores the calendar date exactly as typed. Timestamps do, so `DateAndTimeField` converts in both directions and the question is answered once, in the field, instead of in every generated form.

### Deferred

These field types have no component yet, and each waits on something specific:

- `emoji_field`, `rich_text`, `code_editor`: each needs a heavy editor dependency (Emoji Mart, a rich text editor, Monaco). They belong behind a lazy import so applications that never scaffold one never ship one.
- `file_field`, `image`: these need the upload endpoint and storage decision first. A picker with nowhere to put the bytes is not a field.
- `address_field`: needs the country and region dataset, and dependent-select behavior, which is the same shape `phone_field` wants for country codes.
- `phone_field` international formatting: the field ships as a telephone input today and stores the number as typed. Country selection and E.164 normalization arrive with the country dataset.
- `super_select` async options: options are passed in today. Fetching them from the select options endpoint as the user types changes nothing about the contract.

### Styling

The package ships TypeScript source, so a consuming application's Tailwind build must scan it. Add `@source '../node_modules/@jalapenolabs/anubis/src/**/*.{ts,tsx}';` to the application stylesheet next to the HeroUI globs. The fields also use the vertical rhythm helpers (`compact`, `relaxed`) and the HeroUI theme scale, both of which the starter stylesheet defines.

## The stamping engine

All scaffolders share one pure engine, `anubis::scaffold`:

- **Names**: one model name in, every casing and plural variant out (`TangibleThing`, `tangible_things`, `tangible-thing`, `Tangible Things`, `tangible thing`, ...). Pluralization covers standard English rules plus a table of common irregulars.
- **Replacements**: ordered find-and-replace over paths and file bodies, longest pattern first so `tangible_things` wins over `tangible_thing`. `Replacements::between(template, target)` maps every variant pair at once, and sets compose, which is how a nested model rewrites its own name and its parent's in one pass.
- **Anchor insertion**: `insert_above_anchor` adds generated lines above a magic anchor comment, matching its indentation, and is idempotent so re-running a scaffold never duplicates lines. The `anubis::scaffold::anchor` module names every anchor the framework recognizes.
- **Extraction**: `table_block` and `line_containing` read declarations back out of an application's own files, so a generated table inherits the template's shape instead of a shape hard-coded in the framework.
- **Field types**: `FieldType` and `Field` map a `name:type` argument to a column, a schema type, and a Rust type.
- **Planning**: `ModelScaffold` turns one command's arguments into every decision the generator makes: which template, which replacements, which module, table, migration, and inserted lines.

The engine does no file I/O; the CLI is its thin filesystem shell. That split keeps every transform unit-testable as plain strings.

## How `anubis new` works

The starter tree is embedded into the `anubis` binary at build time, so stamping is offline and always matches the installed framework version. Stamping rewrites the app name across every path and file, then overlays the files that make the result a standalone repository: a workspace `Cargo.toml` carrying the framework's lint bar, a standalone `backend/Cargo.toml`, a root `package.json`, `.yarnrc.yml`, `.gitignore`, `README.md`, and the toolchain and clippy pins. Until the crate and npm package are published, stamped apps depend on the framework from its git repository (Cargo git dependency; yarn `#workspace=` git protocol). A drift-gate test pins the overlay's dependency versions to the framework workspace.

## Route visibility

`anubis routes` prints the framework's mounted surface from two sources: a curated manifest in `anubis::manifest` (drift-gated by a test that composes the real routers and probes every entry) and the OpenAPI document, which contributes every versioned `/api/v1` operation automatically. Application-defined routes live in the application's router and are not visible to the CLI.

## What one `scaffold model` produces

Backend, implemented today:

- Diesel migration and `schema.rs` update
- Model struct with the ownership chain, validations, and `valid_*` scoping methods
- Entry in `roles.yml` permission grants, and the regenerated frontend permissions module
- Account CRUD handlers, routes wired into the router, and the model's integration test

Backend, still to come:

- `/api/v1` handlers (separate, like Bullet Train's account vs api controllers), once [the ownership question](#deferred-apiv1-for-application-models) is settled
- Serializer registered with utoipa (OpenAPI 3.1), shared by the API and outgoing webhooks

Frontend:

- Generated ky route functions and SWR hooks from the refreshed OpenAPI document
- List page (table), show page, and create/edit form pages built from field components
- Navigation entry and breadcrumbs
- Per-model i18next locale file (labels, headings, placeholders, help text, option lists)
- Vitest unit tests and Playwright E2E tests

`scaffold field` propagates a new attribute through every one of those artifacts, which is the feature that makes the framework compound over time.

## Locked conventions the generator stamps

- **List endpoints** follow the page/limit, sort, and filter conventions in [api.md](api.md); the scaffolder maintains each model's sortable and filterable whitelists. `anubis::http::ListParams` and `anubis::http::Pagination` implement the convention once, so every generated endpoint pages and sorts identically.
- **Scoping methods** (`valid_*`): for every association, the scaffolder generates an inherent method on the model, `valid_<associations>(connection, team_id) -> QueryResult<Vec<_>>`, returning the team's own records ordered by name. The same method populates the select options endpoint and validates submitted ids on create and update, so a form can never smuggle in another tenant's record. One definition, both duties.
- **Timestamps**: `created_at`/`updated_at` come from the database. `updated_at` is maintained by the shared `set_updated_at()` trigger, attached to every generated table; application code never sets either.

## Workflow

Domain modeling comes first and scaffolding makes it cheap, so follow Bullet Train's method: write the scaffold commands in a scratch file, review them with people before running them, run them, commit the generated code in its own commit, then polish the UX. Tearing down and re-scaffolding is cheap; live with a wrong domain model is not.

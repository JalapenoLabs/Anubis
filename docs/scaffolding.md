# Scaffolding

Scaffolding is the crown jewel of Anubis, a 1:1 match of Bullet Train's Super Scaffolding philosophy: one command produces a production-ready, permission-scoped, API-backed, fully tested CRUD feature across the entire stack.

## Philosophy: living templates

Templates are real, functional, compiling code, not a DSL. The generator transforms template files into your model's names and namespaces, and the output is standard Rust and standard React that you own and edit freely.

Generated files contain magic anchor comments (`// 🐺 anubis:record-fields`, `{/* 🐺 anubis:nav */}`) that later scaffold commands use as insertion targets. Do not delete them. This is exactly Bullet Train's magic-comment mechanism, and it is what makes `scaffold field` able to keep editing files you have customized.

The template models mirror Bullet Train's naming for the same reason Bullet Train chose it: `scaffolding::absolutely_abstract::CreativeConcept` (parent) and `scaffolding::completely_concrete::TangibleThing` (child) carry enough namespacing fidelity to transform into any real-world combination of parent and child namespaces. They live in the starter host app as compiling, CI-tested code, so the templates can never rot.

## The template host app

The templates are ordinary application code in `starter/`. `CreativeConcept` belongs to a Team; `TangibleThing` belongs to a `CreativeConcept`. Both carry a required `name` and a nullable `description`, so the two ownership depths differ only in ownership. Between them they cover every artifact one `scaffold model` run produces, which is what makes them a specification rather than a demo.

The frontend halves mirror the same split. A team-owned model owns a list page, a show page, a form component, a route module, and a locale file. A nested model owns a form component, a route module, a locale file, and one section component (`TangibleThingsSection`) holding its table and its form, which the parent's show page renders. Reducing a child's whole slice to one element is what lets a later scaffold attach a child to a page an earlier scaffold wrote, by inserting a single line.

Each depth has its own narrative test, `starter/backend/tests/creative_concepts_flow.rs` and `starter/backend/tests/tangible_things_flow.rs`, running against a real Postgres. They are templates too: one scaffold stamps the matching narrative for the generated model, so a new model arrives with the same proof its template carries. The plumbing they share (booting the router, registering an account, inviting a teammate) lives in `starter/backend/tests/support/mod.rs`, which is application code the scaffolder never rewrites.

The starter backend is a library plus a thin binary. `main.rs` is the composition root; the application itself (models, routes, schema, migrations, role constants) lives in `lib.rs` and its modules, so integration tests drive the real routers. Model modules are public, because an application's library is what its binary and its tests build on.

Template files are written so that name-for-name transformation is enough: every local identifier and every sentence uses the model's own vocabulary (`CreativeConceptState`, `creative_concept_id`, "Name the creative concept."), never an abbreviation. An abbreviation would survive the transform and land in generated code as a name from another model.

### Anchor vocabulary

Anchors are `🐺 anubis:<name>` inside the host language's comment syntax. `insert_above_anchor` finds the first occurrence only, so an anchor spelling appears at most once per file.

The vocabulary comes in two halves. **Model anchors** sit in files the whole application shares, and `scaffold model` inserts a model's lines above them. **Field anchors** sit in a model's own artifacts, one per list of columns, and `scaffold field` inserts one field's lines above them. Every artifact a scaffold stamps is a copy of a template that already carries the field anchors, so a generated model is field-scaffoldable forever, and so is the model generated from it a year from now.

#### Model anchors

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
| `// 🐺 anubis:page-imports` | `frontend/src/App.tsx` | page imports |
| `{/* 🐺 anubis:routes */}` | `frontend/src/App.tsx` | `<Route>` elements |
| `{/* 🐺 anubis:nav */}` | `frontend/src/components/AppShell.tsx` | navigation entries |
| `// 🐺 anubis:locale-imports` | `frontend/src/i18n.ts` | per-model locale imports |
| `// 🐺 anubis:locales` | `frontend/src/i18n.ts` | per-model locale spreads |
| `// 🐺 anubis:child-imports` | every show page | imports of child section components |
| `{/* 🐺 anubis:children */}` | every show page | child section elements |

#### Field anchors

Each one closes a list of columns. A model's artifacts carry them wherever a field has to appear, which is what makes `scaffold field` a text insertion rather than a rewrite.

| Anchor | File | Insertion point |
|---|---|---|
| `// 🐺 anubis:record-fields` | `backend/src/<models>/model.rs` | the record struct's columns |
| `// 🐺 anubis:insert-fields` | `backend/src/<models>/model.rs` | the insertable struct's columns |
| `// 🐺 anubis:changeset-fields` | `backend/src/<models>/model.rs` | the changeset struct's columns |
| `// 🐺 anubis:changeset-empty` | `backend/src/<models>/model.rs` | `<Model>Changes::is_empty` |
| `// 🐺 anubis:create-body` | `backend/src/<models>/routes.rs` | the create request body |
| `// 🐺 anubis:update-body` | `backend/src/<models>/routes.rs` | the update request body |
| `// 🐺 anubis:create-normalize` | `backend/src/<models>/routes.rs` | the create handler's bindings |
| `// 🐺 anubis:insert-values` | `backend/src/<models>/routes.rs` | the insertable struct literal |
| `// 🐺 anubis:update-normalize` | `backend/src/<models>/routes.rs` | the update handler's bindings |
| `// 🐺 anubis:changeset-values` | `backend/src/<models>/routes.rs` | the changeset struct literal |
| `// 🐺 anubis:test-create` | `backend/tests/<models>_flow.rs` | the create request's payload |
| `// 🐺 anubis:test-created` | `backend/tests/<models>_flow.rs` | the assertions on the created record |
| `// 🐺 anubis:test-update` | `backend/tests/<models>_flow.rs` | the update request's payload |
| `// 🐺 anubis:test-updated` | `backend/tests/<models>_flow.rs` | the assertions on the updated record |
| `// 🐺 anubis:wire-fields` | `frontend/src/api/routes/<model>Routes.ts` | the wire type |
| `// 🐺 anubis:create-request` | `frontend/src/api/routes/<model>Routes.ts` | the create request type |
| `// 🐺 anubis:update-request` | `frontend/src/api/routes/<model>Routes.ts` | the update request type |
| `// 🐺 anubis:field-imports` | `frontend/src/components/<Model>Form.tsx` | the field components imported |
| `// 🐺 anubis:form-schema` | `frontend/src/components/<Model>Form.tsx` | the zod object |
| `// 🐺 anubis:form-values` | `frontend/src/components/<Model>Form.tsx` | `toFormValues` |
| `// 🐺 anubis:form-payload` | `frontend/src/components/<Model>Form.tsx` | the submitted payload |
| `{/* 🐺 anubis:form-fields */}` | `frontend/src/components/<Model>Form.tsx` | the field components rendered |
| `{/* 🐺 anubis:list-columns */}` | a list page or a section component | the table's column headers |
| `{/* 🐺 anubis:list-cells */}` | a list page or a section component | one row's cells |
| `{/* 🐺 anubis:show-fields */}` | `frontend/src/pages/<Model>Page.tsx` | the record's attribute list |

Two lists carry no anchor, because they repeat inside one file and an anchor spelling may not. A `diesel::table!` block's columns are found structurally, by the block that names the model's table, and the column joins them above `created_at`. A locale file is JSON and cannot hold a comment at all: the strings are merged into the model's own object, described below.

The form's four anchors follow from one rule. The template builds its values in a single `toFormValues` function and its payload in a single object, so the initial values, the reset when the edited record changes, the reset after a create, and both write calls all read one list. A field is added to the form in four places rather than seven.

Each `roles.yml` anchor names its role: grants differ per role, and one insertion point can only be found once. `default` gets `read` and `editor` gets `manage`; `billing` and `admin` inherit and need no entries. A role added by hand carries its own anchor.

`anubis:routes` appears in two files, `backend/src/lib.rs` and `frontend/src/App.tsx`, spelled in each one's comment syntax. The rule is one spelling per file, not one per repository.

The two show-page anchors are what make a scaffolded page a host for later scaffolds: `scaffold model Goal Project,Team` inserts `import { GoalsSection } ...` and `<GoalsSection projectId={projectId} />` into `ProjectPage.tsx`, which `scaffold model Project Team` wrote. A nested model whose parent has no page is refused by name rather than generated half-wired.

`account_router` mounts one router per statement rather than one long method chain, so an inserted line is already `rustfmt`-clean.

#### The contract: do not delete an anchor

Anchors are the price of a framework that keeps editing code you own, exactly as in Bullet Train. Customize a generated file freely, but leave its anchors where they are. A command that cannot find one names the file and the anchor and stops before writing anything:

```
error: frontend/src/components/ProjectForm.tsx no longer carries the anchor
`🐺 anubis:form-fields`. Scaffolding inserts a field's lines above it, so
restore the anchor comment and run this again.
```

Insertion is idempotent within the list an anchor closes, so a line already present is never duplicated. The test is deliberately that narrow: a record struct and the insertable struct beside it declare the same column with the same doc comment, and a whole-file test would read the first as proof the second was already written.

### The template-only marker

`🐺 anubis:template-only` is a marker, not an insertion point: nothing is ever written above it, and every line carrying it is dropped when the page is stamped. The template's show page renders the template's own child, which belongs to no other model, so those two lines (the import and the element) are the template's alone. Keeping such content to one line per marker is the whole of the rule.

### App-owned migrations and schema

Application migrations live in `starter/backend/migrations/`, are embedded with `diesel_migrations::embed_migrations!`, and are applied at boot by `anubis::db::run_app_migrations` **after** `run_pending_migrations`: application tables reference `teams`, and the shared `set_updated_at()` trigger function must already exist. Every generated table attaches that trigger.

`starter/backend/src/schema.rs` holds the application's `table!` blocks. Application tables never appear in the same Diesel query as framework tables: `allow_tables_to_appear_in_same_query!` and `joinable!` emit trait implementations that Rust's orphan rules forbid across crates. The ownership chain therefore ends in one extra indexed lookup, `anubis::tenancy::TeamMembership::for_user(connection, user_id, team_id)`, rather than a join. Application tables join each other freely.

### Authorization in generated handlers

Collection routes hang off the team (`/account/teams/{team_id}/creative-concepts`) and use the `TeamMember` guard directly. Member routes are shallow (`/account/creative-concepts/{id}`, `/account/tangible-things/{id}`), so they resolve the chain with the model's own `load_for_member`, then authorize against the compiled `RoleSet` held in router state. Both paths answer `404` for records the caller cannot reach, so an id probe cannot tell a missing record from another tenant's.

### Frontend slice

The app's own endpoints get a ky client in `frontend/src/api/index.ts` and one route module per model in `frontend/src/api/routes/`. Wire types keep snake_case field names, because those names are the contract.

JSON carries no comments, so each scaffolded model gets its own locale file at `frontend/src/locales/models/<models>.<locale>.json`, and `i18n.ts` carries the anchors that import and merge them. The base application strings stay in `locales/en-US.json`.

A field's own strings live in a `fields` object inside the model's object, and a scaffolder merges them there structurally: it finds that object, appends the new keys after the last one, and leaves every other byte of the file alone. Keys are `camelCase` (`dueDate`, `dueDateHelp`) even though the column and the form control keep the wire's `snake_case` name, so a generated form reads `label={t('tickets.fields.dueDate')}`. The nesting is not decoration: a column may be called anything, and a model with a `title` or an `open` column would otherwise overwrite the page title and the "Open" link its own locale file already declares.

Forms are field components from `@jalapenolabs/anubis`, one per model attribute, bound to react-hook-form through `control` and `name`. The page passes translated strings down (`label={t('tangibleThings.name')}`, `help={t('tangibleThings.nameHelp')}`), which is why the locale keys the scaffolder emits are named after the props.

### Breadcrumbs

Every page hands `AppShell` a `breadcrumbs` array and the frame renders it, so the trail is decided in one place and every scaffolded page inherits it. Crumbs are page-provided rather than derived from the route: a show page's last crumb is the record's own name, and only the page has it. A crumb with a `to` renders as a router link, the last one as plain text. `Breadcrumbs` is a starter component like `AppShell`, so an application owns and restyles it.

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

`anubis new`, `anubis routes`, `anubis doctor`, `anubis scaffold model`, and `anubis scaffold field` are implemented; the rest of the `scaffold` family and `eject` are the remainder of M4 and M5.

Field types map to the [field component library](#the-field-component-library): `text_field`, `text_area`, `number_field`, `email_field`, `phone_field`, `password_field`, `boolean`, `buttons`, `options`, `super_select`, `date_field`, `date_and_time_field`, `color_picker`, `emoji_field`, `rich_text`, `code_editor`, `file_field`, `image`, `address_field`. Modifiers follow Bullet Train: `{readonly}`, `{multiple}`, `{class_name=...}`, `{source=...}`.

The generator accepts the types the living templates prove. Each row knows its column, its Diesel schema type, its Rust type, its wire type, and its React control; a later issue extends the table rather than the code around it, and an unsupported type is refused by name with the supported list.

| Field type | Column | Schema type | Rust type | Wire type | Component |
|---|---|---|---|---|---|
| `text_field` | `TEXT` | `Text` | `Option<String>` | `string \| null` | `TextField` |
| `text_area` | `TEXT` | `Text` | `Option<String>` | `string \| null` | `TextAreaField` |
| `number_field` | `INTEGER` | `Int4` | `Option<i32>` | `number \| null` | `NumberField` |
| `boolean` | `BOOLEAN NOT NULL DEFAULT false` | `Bool` | `bool` | `boolean` | `BooleanField` |
| `date_field` | `DATE` | `Date` | `Option<chrono::NaiveDate>` | `string \| null` | `DateField` |

#### Nullable, or defaulted

Every column a scaffolder adds is safe to add to a table that already holds rows: it is nullable, or it is `NOT NULL` with a database default. `boolean` is the only defaulted type today, which is why it is the only one that is not an `Option`. The rule lives in the field-type table rather than in the generator, so `scaffold field` on a live table and `scaffold model` on an empty one produce the same column, the same Rust type, and the same form control.

The living template's own `name` column is required, and `description` optional, because that is the shape the template proves. Naming either in a field list is the identity case; naming one with the other's type is refused.

Text columns are trimmed on write, and a blank value clears them, which is what a form submits when a user empties a field. Numbers and dates are stored as submitted. A boolean absent from a create request stores `false`.

## `anubis scaffold model`: one command, both ends

```
anubis scaffold model Project Team name:text_field
anubis scaffold model Goal Project,Team name:text_field description:text_area
```

The command runs inside an application, which is a directory holding `backend/`, `frontend/`, and `config/roles.yml`; the search walks up from the working directory, so it works from anywhere inside one, and inside this repository it finds `starter/`. Anywhere else it stops and says so.

The ownership chain ends in `Team`, because every application record reaches a team. `Team` selects the team-owned template and `<Parent>,Team` the nested one. Deeper chains are refused with a pointer at the roadmap rather than generated half-right.

One run produces, on the backend:

- a timestamped migration (`up.sql` and `down.sql`) with the table, its ownership index, and the shared `set_updated_at()` trigger
- a `diesel::table!` block in `backend/src/schema.rs`, plus the `joinable!` and `allow_tables_to_appear_in_same_query!` declarations for a nested model
- the model's module (`mod.rs`, `model.rs`, `routes.rs`) under `backend/src/<models>/`, carrying the ownership chain, the list conventions, the `valid_*` scoping methods, and account CRUD handlers
- the module declaration and router mount in `backend/src/lib.rs`
- `read` and `manage` grants in `config/roles.yml`, and a regenerated `frontend/src/roles.generated.ts`
- the model's own integration test in `backend/tests/<models>_flow.rs`

and on the frontend:

- the ky route module (`frontend/src/api/routes/<model>Routes.ts`) with the wire type, the permission model key, and one function per endpoint
- the form component (`frontend/src/components/<Model>Form.tsx`), one field component per attribute, creating or editing
- the model's locale file (`frontend/src/locales/models/<models>.en-US.json`), and its import and spread in `i18n.ts`
- for a team-owned model: the list page and the show page under `frontend/src/pages/`, the `UrlTree` entries and link factory in `urls.ts`, the page imports and `<Route>` elements in `App.tsx`, and the navigation entry in `AppShell.tsx`
- for a nested model: the section component (`frontend/src/components/<Models>Section.tsx`) holding its table and form, plus its import and element inside the parent's show page

Every artifact is a transformation of the application's own files: the migration comes from the migration that created the template's table, the schema block from the template's `table!` block, the module from the template module, the test from the template's narrative, the pages from the template model's pages. Improving a template improves every later scaffold.

The run is planned before anything is written, so a missing template, a missing anchor, or an existing module stops the command with the application untouched. Anchor insertions are idempotent, and a model whose module already exists is refused rather than overwritten. Generated Rust is formatted with `rustfmt` when it is on `PATH`: transformation cannot preserve line widths, since a shorter model name lets a wrapped statement fit again, and the formatter settles it.

### Fields

Every scaffolded model carries the template's own columns, `name` and `description`, wired end to end. Every other field in the command is planned exactly as `anubis scaffold field` would plan it and inserted into the artifacts the run has just stamped, so this:

```
anubis scaffold model Ticket Team urgency:number_field
```

and this:

```
anubis scaffold model Ticket Team
anubis scaffold field Ticket urgency:number_field
```

leave the application in the same state, apart from a second migration. One set of insertions serves both commands, which is why a field declared on day one and a field added in month six read identically.

Cosmetic limitation: prose in doc comments is transformed word for word, not rewrapped, so a much shorter or much longer model name leaves a ragged comment line. Comments never affect `cargo fmt --check`. On the frontend the generator pre-wraps the one construct a long model name can push past the 120-column lint limit, the link factory.

## `anubis scaffold field`: one column, everywhere

```
anubis scaffold field Project priority:text_field
anubis scaffold field Ticket due_date:date_field
```

One field per run, on a model an earlier scaffold generated. The command finds the model by its own names, from the application root, and refuses by name when it is not there:

```
error: no model named `Ghost` in this application: backend/src/ghosts/model.rs
does not exist. Generate the model first with `anubis scaffold model Ghost Team`.
```

One run produces:

- a timestamped migration adding the column, with the `ALTER TABLE ... DROP COLUMN` that takes it back
- the column in the model's `diesel::table!` block in `backend/src/schema.rs`, above the timestamps
- the column in the record, insertable, and changeset structs, and in the changeset's emptiness test
- the column in both request bodies, both handlers' normalizations, and both struct literals in `routes.rs`
- the column in the model's integration test, asserted through the create and the update
- the wire type and both request types in the model's ky route module
- the zod schema, the form values, the payload, the field component and its import in the model's form
- a column and a cell in the model's table, on its list page or in its section component
- an attribute row on the model's show page, if it has one
- the label and the help text in the model's locale file

Artifacts a model does not have are named in the report rather than skipped quietly: a nested model has a section component and no pages, and a developer may have deleted a file the scaffold wrote. The whole run is planned before it writes, so a missing anchor, a column that already exists, or a model that does not, stops the command with the application untouched.

### The generated test grows with the model

A generated model arrives with a narrative test, and a field added later joins it: the create request sends a value, the assertions check it, the update request sends a different value, and the assertions check that too. Four anchors in the test template carry it, and the samples come from the field type, so a number is `3` then `5` and a date is `2026-01-31` then `2026-02-28`. A column that reaches the database but not the test would be a column nothing proves.

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

The package ships TypeScript source, so a consuming application's Tailwind build must scan it. The starter stylesheet carries the glob twice, `../node_modules/@jalapenolabs/anubis/src/**/*.{ts,tsx}` and `../../../node_modules/...`, exactly as it does for HeroUI: yarn workspaces hoist the package to the repo root while a standalone install keeps it local, and Tailwind skips whichever glob matches nothing. Without it a field's own utilities never reach the stylesheet. The fields also use the vertical rhythm helpers (`compact`, `relaxed`) and the HeroUI theme scale, both of which the starter stylesheet defines.

## The stamping engine

All scaffolders share one pure engine, `anubis::scaffold`:

- **Names**: one model name in, every casing and plural variant out (`TangibleThing`, `tangibleThings`, `tangible_things`, `TANGIBLE_THING`, `tangible-thing`, `Tangible Things`, `tangible thing`, ...). Pluralization covers standard English rules plus a table of common irregulars.
- **Replacements**: ordered find-and-replace over paths and file bodies, longest pattern first so `tangible_things` wins over `tangible_thing`. `Replacements::between(template, target)` maps every variant pair at once, and sets compose, which is how a nested model rewrites its own name and its parent's in one pass.
- **Anchor insertion**: `insert_above_anchor` adds generated lines above a magic anchor comment, matching its indentation, and is idempotent within the list that anchor closes, so re-running a scaffold never duplicates lines. The `anubis::scaffold::anchor` module names every anchor the framework recognizes.
- **Structural insertion**: `insert_json_entries` merges strings into a locale file's own object, because JSON cannot hold an anchor comment.
- **Extraction**: `table_block` and `line_containing` read declarations back out of an application's own files, so a generated table inherits the template's shape instead of a shape hard-coded in the framework.
- **Field types**: `FieldType` and `Field` map a `name:type` argument to a column, a schema type, a Rust type, a wire type, and a React control.
- **Field planning**: `FieldScaffold` turns one field plus a model's names into every line it contributes, keyed by the `Artifact` that receives it. Both scaffolders read the same table, which is what keeps their output identical.
- **Model planning**: `ModelScaffold` turns one command's arguments into every decision the generator makes: which template, which replacements, which module, table, migration, and every line the shared backend and frontend files receive above their anchors.

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

Frontend, implemented today:

- ky route module per model, with the wire type and the permission model key
- List page (table, search, pagination), show page, and the form component built from field components
- Navigation entry, `UrlTree` entries and link factory, routes, and breadcrumbs
- Per-model i18next locale file (labels, headings, help text), imported and merged in `i18n.ts`
- A nested model's section component, attached to its parent's show page

Still to come:

- `/api/v1` handlers (separate, like Bullet Train's account vs api controllers), once [the ownership question](#deferred-apiv1-for-application-models) is settled
- Serializer registered with utoipa (OpenAPI 3.1), shared by the API and outgoing webhooks
- A generated-client refresh per scaffold. The generated client is rendered from the `/api/v1` OpenAPI document, and an application model has no `/api/v1` handlers yet, so there is nothing for a scaffold to refresh. Generated pages call the hand-written route module instead, which is the same contract typed by hand.
- Per-model frontend tests. The starter runs Vitest and the scaffolder's own frontend output is covered by `tsc`, ESLint, the production build, and the integration test that scaffolds two models and reads the result. Playwright end-to-end tests wait on Playwright itself, which the starter does not have.

`scaffold field` propagates a new attribute through every one of those artifacts, which is the feature that makes the framework compound over time.

Still deferred for `scaffold field`: one field per run (run it twice for two), and no `/api/v1` handlers to update, for the same reason `scaffold model` writes none.

## Locked conventions the generator stamps

- **List endpoints** follow the page/limit, sort, and filter conventions in [api.md](api.md); the scaffolder maintains each model's sortable and filterable whitelists. `anubis::http::ListParams` and `anubis::http::Pagination` implement the convention once, so every generated endpoint pages and sorts identically.
- **Scoping methods** (`valid_*`): for every association, the scaffolder generates an inherent method on the model, `valid_<associations>(connection, team_id) -> QueryResult<Vec<_>>`, returning the team's own records ordered by name. The same method populates the select options endpoint and validates submitted ids on create and update, so a form can never smuggle in another tenant's record. One definition, both duties.
- **Timestamps**: `created_at`/`updated_at` come from the database. `updated_at` is maintained by the shared `set_updated_at()` trigger, attached to every generated table; application code never sets either.

## Workflow

Domain modeling comes first and scaffolding makes it cheap, so follow Bullet Train's method: write the scaffold commands in a scratch file, review them with people before running them, run them, commit the generated code in its own commit, then polish the UX. Tearing down and re-scaffolding is cheap; live with a wrong domain model is not.

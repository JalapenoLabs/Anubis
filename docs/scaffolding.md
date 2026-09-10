# Scaffolding

Scaffolding is the crown jewel of Anubis, a 1:1 match of Bullet Train's Super Scaffolding philosophy: one command produces a production-ready, permission-scoped, API-backed, fully tested CRUD feature across the entire stack.

This page is the full statement of the design. [getting-started.md](getting-started.md) is the newcomer's path through the same commands, and [demo.md](demo.md) is the nine-minute script.

## Philosophy: living templates

Templates are real, functional, compiling code, not a DSL. The generator transforms template files into your model's names and namespaces, and the output is standard Rust and standard React that you own and edit freely.

Generated files contain magic anchor comments (`// 🐺 anubis:record-fields`, `{/* 🐺 anubis:nav */}`) that later scaffold commands use as insertion targets. Do not delete them. This is exactly Bullet Train's magic-comment mechanism, and it is what makes `scaffold field` able to keep editing files you have customized.

The template models mirror Bullet Train's naming for the same reason Bullet Train chose it: `scaffolding::absolutely_abstract::CreativeConcept` (root), `scaffolding::completely_concrete::TangibleThing` (child), and `scaffolding::exceedingly_granular::GranularDetail` (grandchild) carry enough namespacing fidelity to transform into any real-world chain of namespaces. They live in the starter host app as compiling, CI-tested code, so the templates can never rot. `scaffolding::incidentally_linked::IncidentalLinkage` is the fourth, the join model, and `scaffolding::merely_peripheral::PeripheralNotion` is the second team-owned model it links. `scaffolding::hypothetically_remote::HypotheticalSenderWebhook` is the last, the incoming webhook receiver, named for a sender the application will never meet.

## The template host app

The templates are ordinary application code in `starter/`. `CreativeConcept` belongs to a Team; `TangibleThing` belongs to a `CreativeConcept`; `GranularDetail` belongs to a `TangibleThing`. All three carry a required `name` and a nullable `description`, so the three ownership depths differ only in ownership. Between them they cover every artifact one `scaffold model` run produces, which is what makes them a specification rather than a demo.

The frontend halves mirror the same split. Every model owns a show page, a form component, a route module, and a locale file. A team-owned model owns a list page on top of that; a nested model owns one section component (`TangibleThingsSection`) holding its table and its form, which its parent's show page renders. Reducing a child's whole slice to one element is what lets a later scaffold attach a child to a page an earlier scaffold wrote, by inserting a single line, and a nested model owning a show page is what gives the depth below it something to attach to.

Each depth has its own narrative test, `starter/backend/tests/creative_concepts_flow.rs`, `tangible_things_flow.rs`, and `granular_details_flow.rs`, running against a real Postgres. They are templates too: one scaffold stamps the matching narrative for the generated model, so a new model arrives with the same proof its template carries. The plumbing they share (booting the router, registering an account, inviting a teammate) lives in `starter/backend/tests/support/mod.rs`, which is application code the scaffolder never rewrites.

### The join template

`anubis scaffold join` transforms a third template, and a join links two team-owned models, so the host app carries two more of them. `IncidentalLinkage` is the join itself: the table, the model that owns every rule the association needs, and the endpoints that attach, detach, list through, and offer options. `PeripheralNotion` is the far side, an ordinary team-owned model reduced to the two endpoints the join's narrative needs, because `scaffold join` generates no model of its own: a real application's far side always comes from its own `scaffold model` run. `starter/backend/tests/incidental_linkages_flow.rs` is the narrative a generated join inherits, and `frontend/src/api/routes/incidentalLinkageRoutes.ts` is the whole frontend surface a join owns.

The template's own record shape is deliberately plain. A join carries no name and no description because a link is not a thing a user names; what it carries is its two foreign keys, the pair's uniqueness, and the timestamps every table gets.

### The webhook template

`anubis scaffold webhook` transforms a fourth template, and it is the odd one out of the family: `HypotheticalSenderWebhook` is owned by nobody, reached without a session, and stored before it is understood, so it shares none of the other three's shape. Two names are rewritten rather than one, the model's and the provider's, because a receiver's identifiers are named after the model (`stripe_webhooks`, `StripeWebhook`) and its URL is named after the provider (`/webhooks/stripe`). Its module is `hypothetically_remote`, its narrative is `starter/backend/tests/hypothetical_sender_webhooks_flow.rs`, and it owns no frontend file at all.

It is also the only template that ships two deliberate blanks: `verify_signature`, because every provider signs differently, and `act_on`, because only the application knows what an event means. Both are marked in the generated code and named in the run's output. [Incoming webhooks](webhooks.md#incoming-receiving-a-third-partys-events) is the full statement of the model.

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
| `// 🐺 anubis:api-routes` | `backend/src/lib.rs` | router mounts in `api_v1_router` |
| `// 🐺 anubis:api-docs` | `backend/src/lib.rs` | per-model merges in `openapi` |
| `// 🐺 anubis:webhook-routes` | `backend/src/lib.rs` | router mounts in `webhooks_router` |
| `// 🐺 anubis:jobs` | `backend/src/lib.rs` | job registrations in `register_jobs` |
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
| `// 🐺 anubis:model-methods` | `backend/src/<models>/model.rs` | the inherent methods an association adds |
| `// 🐺 anubis:record-fields` | `backend/src/<models>/model.rs` | the record struct's columns |
| `// 🐺 anubis:insert-fields` | `backend/src/<models>/model.rs` | the insertable struct's columns |
| `// 🐺 anubis:changeset-fields` | `backend/src/<models>/model.rs` | the changeset struct's columns |
| `// 🐺 anubis:changeset-empty` | `backend/src/<models>/model.rs` | `<Model>Changes::is_empty` |
| `// 🐺 anubis:account-routes` | `backend/src/<models>/routes.rs` | the account routes an association mounts |
| `// 🐺 anubis:handlers` | `backend/src/<models>/routes.rs` | the handlers those routes dispatch to |
| `// 🐺 anubis:create-body` | `backend/src/<models>/routes.rs` | the create request body |
| `// 🐺 anubis:update-body` | `backend/src/<models>/routes.rs` | the update request body |
| `// 🐺 anubis:create-normalize` | `backend/src/<models>/routes.rs` | the create handler's bindings |
| `// 🐺 anubis:insert-values` | `backend/src/<models>/routes.rs` | the insertable struct literal |
| `// 🐺 anubis:update-normalize` | `backend/src/<models>/routes.rs` | the update handler's bindings |
| `// 🐺 anubis:changeset-values` | `backend/src/<models>/routes.rs` | the changeset struct literal |
| `// 🐺 anubis:create-associations` | `backend/src/<models>/routes.rs` | the create handler's association reconciliations |
| `// 🐺 anubis:update-associations` | `backend/src/<models>/routes.rs` | the update handler's association reconciliations |
| `// 🐺 anubis:view-fields` | `backend/src/<models>/routes.rs` | the view struct's association members |
| `// 🐺 anubis:view-load` | `backend/src/<models>/routes.rs` | the association loads a page of records needs |
| `// 🐺 anubis:view-values` | `backend/src/<models>/routes.rs` | the view struct literal one record is built into |
| `// 🐺 anubis:test-create` | `backend/tests/<models>_flow.rs` | the create request's payload |
| `// 🐺 anubis:test-created` | `backend/tests/<models>_flow.rs` | the assertions on the created record |
| `// 🐺 anubis:test-update` | `backend/tests/<models>_flow.rs` | the update request's payload |
| `// 🐺 anubis:test-updated` | `backend/tests/<models>_flow.rs` | the assertions on the updated record |
| `// 🐺 anubis:test-associations` | `backend/tests/<models>_flow.rs` | the requests an association proves itself with |
| `// 🐺 anubis:wire-fields` | `frontend/src/api/routes/<model>Routes.ts` | the wire type |
| `// 🐺 anubis:route-functions` | `frontend/src/api/routes/<model>Routes.ts` | the request functions an association adds |
| `// 🐺 anubis:create-request` | `frontend/src/api/routes/<model>Routes.ts` | the create request type |
| `// 🐺 anubis:update-request` | `frontend/src/api/routes/<model>Routes.ts` | the update request type |
| `// 🐺 anubis:field-imports` | `frontend/src/components/<Model>Form.tsx` | the field components imported |
| `// 🐺 anubis:form-imports` | `frontend/src/components/<Model>Form.tsx` | the application modules a control reads from |
| `// 🐺 anubis:form-hooks` | `frontend/src/components/<Model>Form.tsx` | the hooks a control needs, such as its options |
| `// 🐺 anubis:form-schema` | `frontend/src/components/<Model>Form.tsx` | the zod object |
| `// 🐺 anubis:form-values` | `frontend/src/components/<Model>Form.tsx` | `toFormValues` |
| `// 🐺 anubis:form-payload` | `frontend/src/components/<Model>Form.tsx` | the submitted payload |
| `{/* 🐺 anubis:form-fields */}` | `frontend/src/components/<Model>Form.tsx` | the field components rendered |
| `{/* 🐺 anubis:list-columns */}` | a list page or a section component | the table's column headers |
| `{/* 🐺 anubis:list-cells */}` | a list page or a section component | one row's cells |
| `{/* 🐺 anubis:show-fields */}` | `frontend/src/pages/<Model>Page.tsx` | the record's attribute list |

Two lists carry no anchor, because they repeat inside one file and an anchor spelling may not. A `diesel::table!` block's columns are found structurally, by the block that names the model's table, and the column joins them above `created_at`. A locale file is JSON and cannot hold a comment at all: the strings are merged into the model's own object, described below.

Four of them exist because an association is more than a column. A `valid_*` method and a label lookup are items, not struct members, so `model-methods` gives them statement position; an options endpoint is a route and a handler, so `account-routes` and `handlers` give them theirs; and proving an assignment takes requests of its own, so `test-associations` gives the narrative a place to make them. An insertion that spans a blank line is not detected as already present, which costs nothing: both commands refuse a field the model already carries before they plan anything.

The form's four anchors follow from one rule. The template builds its values in a single `toFormValues` function and its payload in a single object, so the initial values, the reset when the edited record changes, the reset after a create, and both write calls all read one list. A field is added to the form in four places rather than seven.

Each `roles.yml` anchor names its role: grants differ per role, and one insertion point can only be found once. `default` gets `read` and `editor` gets `manage`; `billing` and `admin` inherit and need no entries. A role added by hand carries its own anchor.

`anubis:routes` appears in two files, `backend/src/lib.rs` and `frontend/src/App.tsx`, spelled in each one's comment syntax. The rule is one spelling per file, not one per repository.

The two show-page anchors are what make a scaffolded page a host for later scaffolds: `scaffold model Goal Project,Team` inserts `import { GoalsSection } ...` and `<GoalsSection projectId={projectId} teamId={teamId} />` into `ProjectPage.tsx`, which `scaffold model Project Team` wrote, and `scaffold model Task Goal,Project,Team` does the same to `GoalPage.tsx`. Every show page carries both anchors, whatever its depth, which is what makes the attachment one rule rather than one per level. A nested model whose parent has no page is refused by name rather than generated half-wired.

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

Collection routes hang off the team (`/account/teams/{team_id}/creative-concepts`) and use the `TeamMember` guard directly. Member routes are shallow (`/account/creative-concepts/{id}`, `/account/tangible-things/{id}`, `/account/granular-details/{id}`), so they resolve the chain with the model's own `load_for_member`, then authorize against the compiled `RoleSet` held in router state. Both paths answer `404` for records the caller cannot reach, so an id probe cannot tell a missing record from another tenant's.

The `/api/v1` handlers answer the same questions with a bearer token instead of a session: `ApiCaller` resolves the token to its team, `load_for_team` walks the chain in one comparison, and `ApiCaller::require` authorizes against the same `RoleSet`. [api.md](api.md#application-models) states what a token's roles are and why.

### Plan limits

A generated `create` handler does **not** check a plan limit, and that is deliberate. Whether a model is metered at all, what the limit is called, and whether it is counted per team or per organization are product decisions; a template that guessed would generate a check nobody asked for and a limit name no `billing.yml` defines.

The helper is one line when the answer is yes, at the top of `insert_record`, before the write it guards:

```rust
let held = CreativeConcept::count_for_team(connection, team.id).await?;
limits.check(connection, organization_id, "creative_concepts", held).await?;
```

`anubis::billing::Limits::check` takes the caller's own connection, so it runs inside the transaction that inserts, and its error converts into `ApiError`: a hard limit answers `409` with a message naming the plan, a soft one returns `Ok` and leaves the screen to warn. The limit name convention is the model's snake-case plural, which is what a scaffolded check will use the day the generator writes one. See [billing.md](billing.md#limits).

`seats` is the exception that is already wired: the framework enforces it itself at invitation creation, because memberships are its own table.

### Outgoing webhooks

Every generated model publishes its lifecycle. `routes.rs` declares three event types (`<model>.created`, `<model>.updated`, `<model>.destroyed`) and the three shared functions emit them: `insert_record`, `apply_changes`, and `delete_record`. Emission lives in the shared half deliberately, so a record written through the browser and one written through a bearer token produce byte-identical events, and a column `scaffold field` adds reaches subscribers with no second declaration.

Each of those functions wraps its write, its association reconciliation, and its emission in one transaction, which is what makes the event exactly as durable as the row: a rollback sends nothing, and a commit never loses its webhook. `anubis::webhooks::emit` takes the connection for that reason.

`scaffold field` needs no new anchor for any of this, because the emission sits inside functions the field anchors already live in, and the payload is the `<Model>View` a field joins anyway. The generated narrative subscribes an endpoint, writes the record, and asserts the delivery rows land with the right event types and payload; it stops there, because a real HTTP delivery would need a listener, and `anubis/tests/webhooks_flow.rs` proves that half against one.

### The audit log

The same three shared functions record what they did, one `anubis::audit::record` call beside each emission, inside the same transaction. So every generated model is in its team's audit log with no per-model code and no anchor of its own: the handler hands down an `audit::Context`, the account surface attributes it to the signed-in user and the API surface to the platform application, and the subject label is the record's `name`.

On an update the change set is `Changes::between` over the Diesel record itself, which is why a column `scaffold field` adds is audited the moment it exists, and why an update that only reconciled an association records an empty change set. [Audit log](audit.md) covers the rest.

### The two surfaces of a generated model

Each model's `routes.rs` carries both surfaces and one implementation. Everything above the query is authorization, and that is the only thing the two do differently, so the file holds three shared functions (`list_page`, `insert_record`, `apply_changes`) that both sets of handlers call. They are where the per-field anchors for normalization and struct literals live.

That sharing is the point rather than a saving: the request bodies, the `<Model>View`, and the response envelopes are one declaration each, so a column `anubis scaffold field` adds is a column both surfaces accept, serialize, and document. The API half adds only its `#[utoipa::path]` attributes, its `ApiDoc` derive, and the `api_router` and `openapi` functions the application's `lib.rs` mounts and merges.

### Frontend slice

The app's own endpoints get a ky client in `frontend/src/api/index.ts` and one route module per model in `frontend/src/api/routes/`. Wire types keep snake_case field names, because those names are the contract.

JSON carries no comments, so each scaffolded model gets its own locale file at `frontend/src/locales/models/<models>.<locale>.json`, and `i18n.ts` carries the anchors that import and merge them. The base application strings stay in `locales/en-US.json`.

A field's own strings live in a `fields` object inside the model's object, and a scaffolder merges them there structurally: it finds that object, appends the new keys after the last one, and leaves every other byte of the file alone. Keys are `camelCase` (`dueDate`, `dueDateHelp`) even though the column and the form control keep the wire's `snake_case` name, so a generated form reads `label={t('tickets.fields.dueDate')}`. The nesting is not decoration: a column may be called anything, and a model with a `title` or an `open` column would otherwise overwrite the page title and the "Open" link its own locale file already declares.

Forms are field components from `@jalapenolabs/anubis`, one per model attribute, bound to react-hook-form through `control` and `name`. The page passes translated strings down (`label={t('tangibleThings.name')}`, `help={t('tangibleThings.nameHelp')}`), which is why the locale keys the scaffolder emits are named after the props.

### Breadcrumbs

Every page hands `AppShell` a `breadcrumbs` array and the frame renders it, so the trail is decided in one place and every scaffolded page inherits it. Crumbs are page-provided rather than derived from the route: a show page's last crumb is the record's own name, and only the page has it. A crumb with a `to` renders as a router link, the last one as plain text. `Breadcrumbs` is a starter component like `AppShell`, so an application owns and restyles it.

### `/api/v1` for application models

The application owns its OpenAPI document. `lib.rs` declares the identity of its v1 with a utoipa derive and merges the framework's half and one line per model into it, so a version freezes with the application rather than with the framework release it was generated against. [api.md](api.md#application-models) is the full statement: the merge, the endpoints a model gets, and what a platform token's roles are.

What a `scaffold model` run adds beyond the account slice is two lines in `lib.rs`, above the `api-routes` and `api-docs` anchors, and nothing else: the handlers, the path attributes, and the schema registrations all ride in the model's own stamped `routes.rs`.

`scaffold field` needs no API-specific anchor at all, which is the payoff of the shared declarations. A column joins the record struct, and the record's `ToSchema` puts it in the document; it joins the two request bodies, and their `ToSchema` documents it there. An association's ids join the `<Model>View` through the `view-fields` anchor and are documented the same way. The one thing the generated narrative does not do is assert each column twice: its `/api/v1` section proves the surface, and the account section proves the columns, because both halves serialize through one struct.

## CLI surface

| Command | Purpose |
|---|---|
| `anubis new <name> [--license mit]` | Stamp a new application from the starter template |
| `anubis scaffold model <Model> <ParentChain> <field:type ...>` | Full-stack CRUD scaffold |
| `anubis scaffold field <Model> <field:type>` | Add a field to an existing model, propagated everywhere |
| `anubis scaffold join <JoinModel> <a_id{class_name=A}> <b_id{class_name=B}>` | Join model for has-many-through |
| `anubis scaffold oauth <provider>` | Print how to enable an OAuth login provider (the one-line Google Auth moment) |
| `anubis scaffold webhook <Provider>` | Receiving endpoint for a third party's webhooks |
| `anubis routes` | Print the route table |
| `anubis eject <component>` | Copy a framework field component into the app to own it |
| `anubis eject --list` | Print every component that can be ejected |
| `anubis doctor` | Verify toolchain, database, and config health |
| `anubis secret generate` | Print a fresh `ANUBIS_SECRET_KEY` |

Every command in the table is implemented.

Field types map to the [field component library](#the-field-component-library): `text_field`, `text_area`, `number_field`, `email_field`, `phone_field`, `password_field`, `boolean`, `buttons`, `options`, `super_select`, `date_field`, `date_and_time_field`, `color_picker`, `emoji_field`, `rich_text`, `code_editor`, `file_field`, `image`, `address_field`. Modifiers follow Bullet Train: `{class_name=...}` and `{source=...}` are implemented on `super_select`, and `{readonly}` and `{multiple}` are still component props rather than scaffolder modifiers. Which field types the generator accepts today is the table below; the rest exist as components first, which is the order the two halves land in.

The generator accepts the types the living templates prove. Each row knows its column, its Diesel schema type, its Rust type, its wire type, and its React control; a later issue extends the table rather than the code around it, and an unsupported type is refused by name with the supported list.

| Field type | Column | Schema type | Rust type | Wire type | Component |
|---|---|---|---|---|---|
| `text_field` | `TEXT` | `Text` | `Option<String>` | `string \| null` | `TextField` |
| `text_area` | `TEXT` | `Text` | `Option<String>` | `string \| null` | `TextAreaField` |
| `number_field` | `INTEGER` | `Int4` | `Option<i32>` | `number \| null` | `NumberField` |
| `boolean` | `BOOLEAN NOT NULL DEFAULT false` | `Bool` | `bool` | `boolean` | `BooleanField` |
| `date_field` | `DATE` | `Date` | `Option<chrono::NaiveDate>` | `string \| null` | `DateField` |
| `<other>_ids:super_select{class_name=<Other>}` | none, the join table holds it | none | `Vec<Uuid>` | `string[]` | `SuperSelectField` |
| `<name>_id:super_select{class_name=<Other>}` | `UUID REFERENCES <others> (id) ON DELETE SET NULL` | `Nullable<Uuid>` | `Option<Uuid>` | `string \| null` | `SuperSelectField` |

`super_select` is the one type that spells two different things, and the suffix decides which, exactly as it does in Bullet Train. The plural `_ids` is a has-many-through and declares no column at all, because its values are rows in a join table; the singular `_id` is a belongs_to and declares a real foreign key on this model. [Association fields](#association-fields-has-many-through) and [belongs_to](#belongs_to-one-record-one-foreign-key) describe them in full.

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

```
anubis scaffold model Task Goal,Project,Team name:text_field
```

The ownership chain ends in `Team`, because every application record reaches a team. Its length selects the living template: `Team` the team-owned one, `<Parent>,Team` the nested one, `<Parent>,<GrandParent>,Team` the deepest. A fourth level is refused by name, because [a depth is a template](#three-levels-of-ownership) rather than a flag. Every link must already exist, and no link may repeat.

One run produces, on the backend:

- a timestamped migration (`up.sql` and `down.sql`) with the table, its ownership index, and the shared `set_updated_at()` trigger
- a `diesel::table!` block in `backend/src/schema.rs`, plus, for a nested model, the `joinable!` declaration and one `allow_tables_to_appear_in_same_query!` pair per link in its chain
- the model's module (`mod.rs`, `model.rs`, `routes.rs`) under `backend/src/<models>/`, carrying the ownership chain, the list conventions, the `valid_*` scoping methods, account CRUD handlers, and the `/api/v1` handlers with their OpenAPI registrations
- the module declaration, both router mounts, and the document merge in `backend/src/lib.rs`
- `read` and `manage` grants in `config/roles.yml`, and a regenerated `frontend/src/roles.generated.ts`
- the model's own integration test in `backend/tests/<models>_flow.rs`

and on the frontend:

- the ky route module (`frontend/src/api/routes/<model>Routes.ts`) with the wire type, the permission model key, and one function per endpoint
- the form component (`frontend/src/components/<Model>Form.tsx`), one field component per attribute, creating or editing
- the model's locale file (`frontend/src/locales/models/<models>.en-US.json`), and its import and spread in `i18n.ts`
- the show page (`frontend/src/pages/<Model>Page.tsx`), its `UrlTree` entry and link factory in `urls.ts`, and its import and `<Route>` element in `App.tsx`
- for a team-owned model: the list page (`frontend/src/pages/<Models>Page.tsx`) beside it, a second `UrlTree` entry and route, and the navigation entry in `AppShell.tsx`
- for a nested model, at either depth: the section component (`frontend/src/components/<Models>Section.tsx`) holding its table and form, plus its import and element inside the parent's show page

Every artifact is a transformation of the application's own files: the migration comes from the migration that created the template's table, the schema block from the template's `table!` block, the module from the template module, the test from the template's narrative, the pages from the template model's pages. Improving a template improves every later scaffold.

The run is planned before anything is written, so a missing template, a missing anchor, or an existing module stops the command with the application untouched. Anchor insertions are idempotent, and a model whose module already exists is refused rather than overwritten. Generated Rust is formatted with `rustfmt` when it is on `PATH`: transformation preserves neither import order nor line width, since a shorter model name lets a wrapped statement fit again and a name that sorts elsewhere moves within its `use` block, and the formatter settles both. `anubis new` runs the same pass over the tree it stamps.

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

### Three levels of ownership

`anubis scaffold model Task Goal,Project,Team` generates a model owned through a model that is itself owned through a model. Four things decide what that costs, and the answers are what the depth is made of:

- **Routes are the same shape at any depth.** A collection hangs off its immediate parent (`/account/goals/{goal_id}/tasks`), a member route off the record (`/account/tasks/{task_id}`). Nothing about a path says how deep it sits.
- **Chain resolution grows a hop.** `load_for_member` at depth two joins the model to its parent and asks `TeamMembership::for_user` about the parent's `team_id`. At depth three it joins twice, `tasks -> goals -> projects`, and asks about the root's. Diesel writes that happily. What it cannot do is arrive by renaming: a name-for-name transform of a two-table query never produces a three-table one, so **a depth is a living template**, not a flag. There are three, and a fourth chain link is refused by name because there is no fourth template to prove it.
- **The team is read off the chain's root, never copied onto the row.** Every generated handler needs a `team_id`: to authorize, to scope a `valid_*` lookup, and to address a webhook. A grandchild's immediate parent has no such column, so the root is selected alongside the parent in the same query (`load_for_member` returns the record, its parent, its grandparent, and the membership). The alternative, a `team_id` on every generated table kept by a trigger, would make all depths identical and is exactly the bug worth avoiding: a denormalized tenant column that drifts is the worst thing this framework could ship. Cross-tenant refusal therefore happens at every hop, because every hop is a join rather than a trusted id, and a crafted path answers `404` at the first link it cannot reach.
- **A nested model owns a show page, which is what a grandchild attaches to.** A team-owned model owns a list page and a show page; a nested model owns a section component its parent's show page renders, and a show page of its own. It gets no list page (its table is that section) and no navigation entry (it is opened from the record that owns it), but it does get a `UrlTree` entry, a link factory, an `App.tsx` route, and breadcrumbs that walk its chain back to the root. That page is the attachment point the depth below it needs, which is why the two-level scaffold generates it whether or not a third level is ever used.

The scoping rules `scaffold join` and `belongs_to` enforce are unchanged: both still require **team-owned** sides, so one comparison decides whether a submitted id is this tenant's. A model owned through a parent is refused by name on either. Lifting that is a separate piece of work, because a join whose sides sit at different depths needs a scope query per side rather than a shared one.

The living templates are `CreativeConcept` (team-owned), `TangibleThing` (nested), and `GranularDetail` (nested twice), and the depth-three narrative in `backend/tests/granular_details_flow.rs` proves the whole chain against a real Postgres, refusal by refusal.

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
- the column in both request bodies, both normalizations, and both struct literals in `routes.rs`, which is one insertion each for the account and `/api/v1` surfaces together
- the column in the model's integration test, asserted through the create and the update
- the wire type and both request types in the model's ky route module
- the zod schema, the form values, the payload, the field component and its import in the model's form
- a column and a cell in the model's table, on its list page or in its section component
- an attribute row on the model's show page, if it has one
- the label and the help text in the model's locale file

Artifacts a model does not have are named in the report rather than skipped quietly: a nested model has a section component and no list page, and a developer may have deleted a file the scaffold wrote. The whole run is planned before it writes, so a missing anchor, a column that already exists, or a model that does not, stops the command with the application untouched.

### The generated test grows with the model

A generated model arrives with a narrative test, and a field added later joins it: the create request sends a value, the assertions check it, the update request sends a different value, and the assertions check that too. Four anchors in the test template carry it, and the samples come from the field type, so a number is `3` then `5` and a date is `2026-01-31` then `2026-02-28`. A column that reaches the database but not the test would be a column nothing proves.

## `anubis scaffold join`: the has-many-through half

```
anubis scaffold join AppliedTag project_id{class_name=Project} tag_id{class_name=Tag}
```

Bullet Train splits a has-many-through into two commands, and so does Anubis, for the same reason: an association reads through a join model, and a join model links two models that both already exist. The join is generated first, the association field second. A `scaffold field` run that finds no join refuses and prints the `scaffold join` command that would create one, rather than guessing a name for a model the developer has to live with.

Both sides must be **team-owned**, which is what lets one comparison decide whether a pair is tenant-safe. A side owned through a parent is refused by name, whatever its depth, and a belongs_to refuses its target for the same reason. A side that does not exist is refused with the `scaffold model` command that would create it, and a pair that some join already links is refused with that join's name: one join model per pair.

Each side is written `<model>_id{class_name=<Model>}`, exactly as Bullet Train writes it (`class` is accepted as a spelling of `class_name`). The attribute must be the class's own `<model>_id`, because a generated join reaches its sides by that name everywhere; a differently named foreign key is refused with the expected spelling.

One run produces:

- a timestamped migration: the join table with a uuid primary key, both foreign keys with `ON DELETE CASCADE`, an index on the second side, a composite `UNIQUE` on the pair, and the shared `set_updated_at()` trigger
- the `diesel::table!` block, `joinable!` in both directions, and one `allow_tables_to_appear_in_same_query!` pair per side
- the join's module (`mod.rs`, `model.rs`, `routes.rs`) under `backend/src/<join_models>/`
- its module declaration and router mount in `backend/src/lib.rs`
- its integration test in `backend/tests/<join_models>_flow.rs`
- the ky route module `frontend/src/api/routes/<joinModel>Routes.ts`, carrying the options hook a form binds to

The composite `UNIQUE` is why attaching twice is a no-op rather than a duplicate, including when two requests race; its index is also the lookup by the owning side, which is the leading column.

### The endpoints a join owns

| Method | Path | Authorizes |
|---|---|---|
| GET | `/account/teams/{team_id}/<join-models>/options` | `read` on the target |
| GET | `/account/<owners>/{owner_id}/<targets>` | `read` on the owner |
| POST | `/account/<owners>/{owner_id}/<targets>` | `update` on the owner |
| DELETE | `/account/<owners>/{owner_id}/<targets>/{target_id}` | `update` on the owner |

The options endpoint is keyed by the join rather than by the target, so two associations reaching the same model from different owners never collide on a route. It answers `{ "options": [{ "value": ..., "label": ... }] }`, which is the field library's `FieldOption` exactly, so a generated form passes the response straight to the control.

### Why a join takes no entry in `roles.yml`

A join model is infrastructure, not a resource. Attaching a tag to a project is an update *on the project*; reading which tags are attached is a read on the project; listing the tags a form may offer is a read on the tag. Granting the join its own permissions would ask a developer to keep two grants in step for one user-visible action, and the first time they drift the answer to "who may tag a project" stops being knowable from `roles.yml`. So the join rides the two models it links, and `config/roles.yml` keeps one entry per real-world model.

### The generated test

The join's narrative proves the whole surface against a real Postgres: options scoped to the caller's team, attach, list through, a repeated attach that changes nothing, a second record joining the first, another tenant's owner behind `404`, another tenant's record refused on attach, a read-only member refused on both writes, and detach leaving the records themselves alone.

## Association fields: has-many-through

```
anubis scaffold field Project tag_ids:super_select{class_name=Tag}
```

The suffix is `_ids` plural and the name is the target's own, exactly as Rails and Bullet Train spell a `has_many :through` attribute. The field declares no column: its values are rows in the join table, so the run writes no migration and touches no `diesel::table!` block. What it does is wire the association through every artifact the model owns.

Backend:

- both request bodies gain `<other>_ids: Option<Vec<Uuid>>`; absent leaves the set alone, a list replaces it whole
- the create and update handlers reconcile the set through the join model's `replace_all`, which validates every submitted id against `valid_*` and then, in one transaction, deletes the links that are gone and inserts the ones that are new
- the model's view struct gains `<other>_ids: Vec<Uuid>`, loaded for a whole page in one query, so list, show, create, and update all serialize the same shape

Frontend:

- the wire type gains `<other>_ids: string[]`, and both request types gain it as optional
- the form gains a `SuperSelectField` in multiple mode, its options from the join's own options hook, its value read straight from the record it is editing
- the model's table gains a column counting the links, and its show page an attribute doing the same
- the model's locale file gains the label and help text, named after the model the association reaches (`tagIds` reads "Tags", not "Tag ids")

The model's narrative test gains the wire shape through the create request and its assertion. Attaching and detaching are proven by the join's own generated test, which is where those endpoints live.

### The view struct

Every scaffolded model's `routes.rs` carries a `<Model>View`, a `serde(flatten)` wrapper around the record. With no associations it serializes exactly as the table does, so it costs nothing; an association adds its ids to it. That is what makes one form able to read and write the same shape, and it is the serializer all three consumers share: the account UI, `/api/v1`, and the payload of every [outgoing webhook](webhooks.md) the model emits.

## belongs_to: one record, one foreign key

```
anubis scaffold field Project lead_id:super_select{class_name=TeamMembership}
anubis scaffold field Project owner_id:super_select{class_name=Tag}
```

The suffix is `_id` singular and the name is the attribute's own, exactly as Rails and Bullet Train spell a `belongs_to`. The field is named for the role it plays rather than for the class it reaches, so one model may point at the same target twice: `lead_id` and `reviewer_id` can both name `TeamMembership` and never collide.

**The first form is Bullet Train's signature pattern**, and it is deliberate advice rather than a technicality: a record is assigned to a **team membership**, not to a user, so a teammate who has been invited but has not signed up yet can already be assigned work. [tenancy.md](tenancy.md) states the model.

### The column

A belongs_to declares a real column, and it follows the nullable-or-defaulted rule like every other one:

```sql
ALTER TABLE projects ADD COLUMN lead_id UUID REFERENCES team_memberships (id) ON DELETE SET NULL;
CREATE INDEX projects_lead_id_index ON projects (lead_id);
```

`ON DELETE SET NULL` rather than `RESTRICT`, because an assignment is a pointer and not a dependency. Under `RESTRICT`, removing somebody from a team would fail for as long as one record still named them, and the person removing them would have no way to know which record was in the way; under `SET NULL` the membership goes and the assignment empties, which is what "this record no longer has a lead" means. Postgres indexes a primary key and never the foreign keys pointing at it, so the run adds the index the label lookup and the delete both read.

The foreign key crosses into the framework's own `team_memberships` table, which is legal: SQL constraints know nothing about crates. Only Diesel's `joinable!` and `allow_tables_to_appear_in_same_query!` are barred across a crate boundary, which is why the label is fetched rather than joined.

### `{source=...}`: where `valid_*` reads from

Bullet Train's `source` modifier names the collection behind the generated `valid_leads` method, and it means the same thing here. Rails takes any expression because it interpolates it into Ruby; a statically typed stack takes the two collections it can write a query for, and refuses the rest by name rather than emitting Rust that does not compile:

| `source` | Reads | Default for |
|---|---|---|
| `team.memberships` | The team's roster, through `anubis::tenancy::TeamMembership::valid_for_team` | `class_name=TeamMembership` |
| `team.<others>` | The target model's own team-owned records, ordered by name | every other class |

Bullet Train writes the roster scope out in full as `team.memberships.current_and_invited`, and that spelling is accepted too: Anubis deletes a membership when a person leaves, so its roster is already current and invited. A `source` naming anything else is refused with both spellings shown. `source` on a has-many-through is refused as well, because there the join model owns `valid_*`, being the one artifact that knows both sides.

The two sources are also each other's boundary. `class_name=TeamMembership` cannot read `team.team_memberships`, because an application crate cannot query a framework table in its own right; and `source=team.memberships` cannot go with any other class, because the roster is not that class's records. Both refusals name the spelling that works.

### What one run produces

Backend:

- the migration above, and the column in the model's `diesel::table!` block
- `lead_id` in the record, insertable, and changeset structs, and in the changeset's emptiness test
- **two methods on the model**, above the `model-methods` anchor: `valid_leads`, which both fills the options endpoint and validates a submitted id, and `lead_labels`, which reads the labels of a whole page of records in one query
- the options endpoint and the check both writes run, above the `account-routes` and `handlers` anchors
- `lead_id` in both request bodies, validated against `valid_leads` before the insert rather than after it, so an id from another tenant answers `400` instead of tripping the foreign key into a `500`
- `lead_label` on the view, loaded for a whole page at a time

Frontend:

- the wire type gains `lead_id: string | null` and `lead_label: string | null`, and both request types gain `lead_id`
- the model's route module gains `listProjectLeadOptions`, above the `route-functions` anchor
- the form gains a `SuperSelectField` in **single** mode, its options from `useFieldOptions`, which is the field library's own SWR hook for an options endpoint
- the table and the show page render `lead_label`, so a screen shows a name and never a uuid
- the locale file gains the label and help text, named after the attribute (`leadId` reads "Lead", not "Lead id")

The narrative proves the whole thing against a real Postgres, through the `test-associations` anchor. A membership assignment needs no fixture, because the account the narrative registers is already a member of its own team: it reads the options, assigns the first one, asserts the label comes back, clears the assignment with `null`, and is refused a uuid the team does not own. A model-backed assignment cannot create its far side from this test, so what it proves is the scoping: a fresh team is offered exactly nothing, and a record it does not own is refused on write.

### Where the options endpoint is mounted

```
GET /account/teams/{team_id}/<owners>/options/<attribute>
```

`GET /account/teams/{team_id}/projects/options/lead`, and a second assignment on the same model is `.../options/reviewer`. The key is the **owner and the attribute**, for the reason the join's options endpoint is keyed by the join rather than by the target: two associations reaching the same model must never collide on a route. It answers `{ "options": [{ "value": ..., "label": ... }] }`, the field library's `FieldOption` exactly, so a generated form hands the response straight to its control. `anubis::http::FieldOption` and `FieldOptions` are that shape in Rust, and the join's own options endpoint answers with them too.

Authorization differs by source, and each answer follows an existing rule. A roster is every member's to read, which the framework's own `/tenancy/teams/{id}/members` already says, so a membership picker asks for `read` on the model that carries the assignment. An application model has a permission key of its own, so offering its records is a `read` on **that** model, exactly as a join's options endpoint is.

### How the display name reaches the screen

The wire carries `<name>_label` beside `<name>_id`, and the view fills it with one query per page.

A join would have been the obvious alternative and it is not available: the roster lives in a framework table, and no `joinable!` may cross that boundary. Fetching by id serves both sources through one shape, costs one indexed query for a whole page (never one per row), and leaves the column itself untouched, so `/api/v1` and every outgoing webhook payload carry the id **and** the name without a second serializer. The cost is honest: the label is a projection of another record, so a record whose target is renamed reads the new name on the next request, and a stale client shows the old one until it refetches.

### What a belongs_to still refuses

- **A target that is not team-owned**, for the reason both sides of a join must be: one comparison decides whether a submitted id is this tenant's. A model owned through a parent is refused by name.
- **A belongs_to at `scaffold model` time**, like a has-many-through: an association reaches a model that has to exist already, so the field is added afterwards.
- **`class_name=User`**, implicitly: it is not `TeamMembership` and not an application model, so it is refused as a model this application does not own. Assign to the membership, which is the point of the pattern.

## `anubis scaffold oauth`: one provider, one command

```
anubis scaffold oauth google
```

This is Bullet Train's one-line Google Auth moment, and it is the thinnest command in the family: **it writes no file at all**. Every part of the feature is framework behavior that arrives with the dependency: the three routes, the authorization-code flow with PKCE, the server-side state and nonce, the ID token verification, the identity linking, and the account bootstrap all live in `anubis::auth::oauth` and are described in [api.md](api.md#oauth-sign-in). The sign-in page renders one button per provider that `GET /auth/oauth/providers` reports, so even the button is not per-provider code.

What the command does is print the two steps only a person can take: registering an OAuth client with the provider, using the redirect URI it spells out from `APP_URL`, and setting `<PROVIDER>_OAUTH_CLIENT_ID` and `<PROVIDER>_OAUTH_CLIENT_SECRET`.

### Why the command generates nothing

An application declares its providers by setting their credentials, not by listing them somewhere. A generated button is exactly such a list: a second source of truth that disagrees with the environment the moment a deployment differs from development, and the failure mode of that disagreement is a button that always fails with `oauth_unavailable`. So the environment decides and the page follows: a provider with both credentials set is enabled and appears in discovery, a provider with one of the two refuses to start, and a provider with neither is simply absent from the page.

An application that wants a different arrangement owns `SignInPage.tsx` and can lay the buttons out however it likes; what it should keep is rendering the list the backend reports rather than a list of its own.

The provider registry itself is framework-owned and OpenID Connect only, so an unknown key is refused by name with the known list. `<PROVIDER>_OAUTH_ISSUER` overrides the registry's issuer, which is what a self-hosted identity server, a single-tenant directory, and the framework's own test suite use.

## `anubis scaffold webhook`: one provider, one endpoint

```
anubis scaffold webhook Stripe
```

Bullet Train's `super_scaffold:incoming_webhook`, and the one command in the family that generates no user interface: nobody browses a provider's events, the application processes them. What it produces is a table, an unauthenticated endpoint that stores a request and queues a job in one transaction, the signature check, the job, and the narrative that proves all of it.

The argument is the **provider**, not the model, because the provider is the thing a person has an account with. The model's name follows: `Stripe` gives `StripeWebhook` in `backend/src/stripe_webhooks/`, stored in `stripe_webhooks`, received at `/webhooks/stripe`, signed with `STRIPE_WEBHOOK_SECRET`. Write the provider the way it should read in the URL, exactly as `scaffold oauth` takes its provider: `github` gives `/webhooks/github`. A name that already ends in `Webhook` is refused rather than doubled up, and a provider this application already receives is refused by name: one endpoint per provider, because two would give the provider two URLs storing the same events into different tables.

One run produces:

- a timestamped migration creating `<provider>_webhooks` (payload, headers, verified, received_at, processed_at, error), with a partial index on the unprocessed backlog and the shared `set_updated_at()` trigger
- the `diesel::table!` block in `backend/src/schema.rs`, with no `joinable!` and no same-query pair, because a received webhook points at nothing until the application decides what it is about
- the module under `backend/src/<provider>_webhooks/` (`mod.rs`, `model.rs`, `routes.rs`, `job.rs`)
- the module declaration, the router mount above `webhook-routes`, and the job registration above `jobs`, all in `backend/src/lib.rs`
- the receiver's integration test in `backend/tests/<provider>_webhooks_flow.rs`

and no entry in `config/roles.yml`, for the same reason a join takes none: the caller is not a team member, and there is no team to scope a permission to.

The run then prints what it cannot do: register the endpoint's URL with the provider, set `<PROVIDER>_WEBHOOK_SECRET`, finish `verify_signature`, and finish `act_on`. The last two are the template's two deliberate blanks, and naming them in the output is the honest alternative to generating a check that only looks like it works.

[Incoming webhooks](webhooks.md#incoming-receiving-a-third-partys-events) covers the design the generated code implements: why storing precedes verifying, what each provider's signature scheme looks like, and why the endpoint carries no rate limit.

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

`SuperSelectField` also takes `onSearch` and `isLoading`, for an association too large to send in one response: the component reports the typed query, debounced at 250 ms, and the form refetches the options endpoint and passes the page back down. Leave `onSearch` out and the given options are filtered in the browser, which is the right answer for a list that fits in one response. Either way the component's contract is the same, which is what lets a five-row association and a directory-sized one render through one control.

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
| `emoji_field` | `EmojiField` | HeroUI Input holding one grapheme |
| `rich_text` | `RichTextField` | Tiptap, behind a lazy import; `RichTextView` renders it |
| `code_editor` | `CodeEditorField` | CodeMirror 6, behind a lazy import |
| `file_field` | `FileField` | A picker and a link, uploading through the application |
| `image` | `ImageField` | The same, showing a thumbnail |

A boolean is a switch, not a checkbox: a boolean column is a setting that is on or off, and a switch says so at a glance. Checkboxes stay with selection lists, where "include this one" is the meaning.

Dates carry no timezone, so `DateField` stores the calendar date exactly as typed. Timestamps do, so `DateAndTimeField` converts in both directions and the question is answered once, in the field, instead of in every generated form.

`PasswordField` carries a reveal toggle, drawn as inline SVG. A framework that pulled a whole icon set into every application's dependency tree for two glyphs would have made a bad trade.

### The heavy fields, and what they cost

Three fields need an editor an application should not pay for unless it uses one, so each is reached through a dynamic import inside `React.lazy`. The package's own build keeps them external and keeps the import dynamic, so the application's bundler gives each one a chunk of its own:

| Chunk | Loaded when |
|---|---|
| `RichTextEditor` | a form renders a `RichTextField` |
| `CodeEditor` | a form renders a `CodeEditorField` |
| `@codemirror/lang-<name>` | that editor's `language` prop names it |

Nothing above is in the entry chunk, which is the whole point: an application that scaffolds no rich text ships no ProseMirror.

**`rich_text` stores HTML.** Bullet Train's `trix_editor` does, and HTML is the representation every other consumer of a record already understands: an email body, an export, a webhook payload, and an `/api/v1` response carry markup without first agreeing on an editor's document model. An empty document is stored as `''`, so a nullable column ends up NULL when a user clears it.

**Rendering that HTML is the consumer's problem, and the framework ships the answer.** Markup one user wrote and another user reads is a cross-site scripting hole unless something sanitizes it. `RichTextView` is that something: it runs DOMPurify and refuses to render at all on a platform DOMPurify cannot secure. Render a stored `rich_text` value with it, never with a bare `dangerouslySetInnerHTML`. Sanitizing in the browser is defense in depth rather than the whole defense: a value written through `/api/v1` by a bearer token never passes through this package, so an application that accepts rich text over its API should sanitize on write too.

**`code_editor` is CodeMirror 6, not Bullet Train's Monaco.** Monaco is an IDE carrying a worker-based TypeScript service and weighs megabytes, which is a steep price for a control that edits a template or a snippet of configuration; CodeMirror 6 is tree-shakeable, ships each grammar as its own package, and works on touch devices. An application that genuinely wants IntelliSense ejects `CodeEditorField`.

**`emoji_field` is one grapheme of text, with no picker.** Emoji Mart carries a 1.4 MB dataset, publishes types that omit its own data module's export, and pins React through a range narrower than this package's. Every platform this field is reached from already ships a complete, current picker on a keyboard shortcut (`Win` `.`, `Ctrl` `Cmd` `Space`, the emoji key on every touch keyboard), and the field holds a single character. So the control is a text input that keeps the last grapheme, which is what makes the column an ordinary `TEXT` that renders on a show page as itself. An application that wants the in-page picker ejects `EmojiField` and adds the dependency to its own tree.

### `file_field` and `image`: controlled, and deliberately so

Both fields hold a `FileReference` (a `url`, and the `name`, `size`, and `contentType` that make it readable) and take an `onUpload: (file: File) => Promise<FileReference>`. The field never talks to a server: it hands the chosen `File` to the application and stores what comes back.

That is a decision, not a placeholder. The framework stores avatars as bytes in Postgres, and whether an application's attachments belong there, in S3, or behind a signed CDN URL is something it decides once and lives with. A field component that picked one would be wrong for most applications, and a framework attachments table would make that wrong answer the default. When the framework grows a generic attachments endpoint, it becomes one implementation of `onUpload` and changes nothing about these components.

### Deferred

- `rich_text`, `code_editor`, `emoji_field`, `file_field`, `image` are **components, not yet scaffolder field types**: `anubis scaffold field` still refuses them by name. `emoji_field` and `code_editor` are plain `TEXT` columns and need only a row in `FIELD_TYPES`. `rich_text` needs one more decision first, because a show page renders a column as text and rendering HTML as text shows the tags: either the generated show page gains an import anchor so it can render `RichTextView`, or the backend sanitizes on write and the page renders trusted markup. `file_field` and `image` wait on the storage decision above, since a generated column has to hold something and only an application knows whether that is a URL or a foreign key.
- `address_field`: needs the country and region dataset, and dependent-select behavior, which is the same shape `phone_field` wants for country codes.
- `phone_field` international formatting: the field ships as a telephone input today and stores the number as typed. Country selection and E.164 normalization arrive with the country dataset.
- Association list-query batching: a list endpoint runs one query per association. That is backend work and invisible from the field library.

### Styling

The package ships TypeScript source, so a consuming application's Tailwind build must scan it. The starter stylesheet carries the glob twice, `../node_modules/@jalapenolabs/anubis/src/**/*.{ts,tsx}` and `../../../node_modules/...`, exactly as it does for HeroUI: yarn workspaces hoist the package to the repo root while a standalone install keeps it local, and Tailwind skips whichever glob matches nothing. Without it a field's own utilities never reach the stylesheet. The fields also use the vertical rhythm helpers (`compact`, `relaxed`) and the HeroUI theme scale, both of which the starter stylesheet defines.

Shipping the source is also what makes `anubis eject` possible: the command copies a component out of the installed package, so the copy is the code that application is running.

## `anubis eject`: the ownership escape hatch

```
anubis eject --list
anubis eject TextField
```

Bullet Train's `bin/resolve --eject` copies a framework file into the application so the developer owns it. This is that command, for the frontend package. It runs inside an application, exactly as the scaffolders do, and one run copies a field component into `frontend/src/anubis/`, records where the file came from, and moves every import of it in the application from the package to the copy:

```
ejected TextField from @jalapenolabs/anubis v0.1.0

created:
  frontend/src/anubis/fields/TextField.tsx
  frontend/src/anubis/fields/internal/TextualField.tsx
rewired:
  frontend/src/components/CreativeConceptForm.tsx
  frontend/src/components/settings/ProfileForm.tsx
  ...
```

### What is ejectable, and what is not

The ejectable surface is the [field component library](#the-field-component-library): the eighteen field components, `RichTextView`, `FieldWrapper`, and `useFieldState`. `anubis eject --list` prints it with a line each.

Everything else the package ships stays framework-owned on purpose. The API client, the realtime client, the React hooks (`useFieldOptions` among them, because it reads an options endpoint's envelope), and the WebAuthn helpers speak a protocol the backend keeps moving, so a copy of one would fork that contract rather than restyle a control, and the fork would be silent until an upgrade broke it. Bullet Train draws the same line: partials and locales eject, framework concerns are extended rather than copied. An application that wants different behavior composes those APIs in the pages it already owns, because the starter owns every page.

An unknown name is refused with the catalog rather than guessed at.

### Where the copy lands, and how imports are rewritten

The ejected tree mirrors the package's own layout under `frontend/src/anubis/`, so `src/fields/TextField.tsx` becomes `frontend/src/anubis/fields/TextField.tsx`. The mirror is what makes provenance obvious at a glance, and it is also load-bearing: a copied file's relative imports of its siblings still resolve, unchanged.

Each relative import inside a copied file takes one of three roads, decided by the package's own `index.ts` rather than by a list in the CLI:

- a dependency the package **exports** is read from the package (`import type { AnubisFieldProps } from '@jalapenolabs/anubis'`), so an ejected `TextField` still shares one `FieldWrapper` and one `useFieldState` with every field that was not ejected
- a dependency the package **keeps to itself** rides along, reported as an extra file. `TextField`, `EmailField`, `PasswordField`, and `PhoneField` differ only in input type, and the control behind them (`internal/TextualField.tsx`) is not exported, so leaving it behind would leave an import that does not resolve
- a dependency this application **already ejected** is reached where it lies, so ejecting `FieldWrapper` first and a field afterwards gives that field your wrapper, not the package's

A **lazily** imported dependency takes the same three roads. `RichTextField` and `CodeEditorField` fetch their editors through `import(...)` inside a `lazy(...)` factory rather than through an import statement, so the walk reads both spellings; a copy that took the field and left the editor behind would work until the form was opened. A module reached only dynamically binds no names, which makes it internal by definition, so it always rides along.

In the application's own files, only the ejected name moves. A shared import line splits, and the rest stays with the package:

```tsx
import { OptionsField, SuperSelectField } from '@jalapenolabs/anubis'
import { TextField } from '../../anubis/fields/TextField'
```

A scaffolded form imports its fields through a list closed by the `🐺 anubis:field-imports` anchor, and the anchor stays in the package's line, where the next `anubis scaffold field` run inserts. Ejecting a field never costs a model its scaffoldability.

### The trade

An ejected file is yours, and that is the whole point and the whole price: upgrading `@jalapenolabs/anubis` no longer improves it, and a fix that lands upstream has to be brought over by hand. This is Bullet Train's documented caveat about ejected views, and it applies here for the same reason. Every copied file says so in a comment under its copyright header, naming the version and the date it came from, which is what makes a later upstream diff possible:

```tsx
// Copyright © 2026 Jalapeno Labs

// Ejected from @jalapenolabs/anubis v0.1.0 (src/fields/TextField.tsx) on 2026-08-15.
// This file belongs to this application now: upstream improvements to it no longer
// arrive. Delete it to go back to the package's copy.
```

Deleting the file is the way back, and re-ejecting is refused by name while the copy exists, so the command can never quietly overwrite work. The application's imports of the deleted copy are the developer's to point back at the package; nothing else needs undoing.

### How the package is found

Ejecting reads the installed package, not the framework's own tree, so the copy matches the version the application runs. The search walks up from `frontend/`, exactly as node resolution does: yarn hoists a workspace dependency to the workspace root, while a standalone install keeps it beside the package that asked for it, and both arrangements are found. An application whose dependencies are not installed is told to run `yarn install` rather than handed a copy of something else.

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
- **Join planning**: `JoinScaffold` does the same for a join, rewriting three model names and three module paths at once so generated code reaches each side through the module that side's own scaffold created.
- **Provider planning**: `OauthScaffold` turns one provider into the redirect URI its console needs. It contributes no code, because the sign-in page renders the providers the backend reports.
- **Receiver planning**: `WebhookScaffold` turns one provider into the model name it implies, the module and table that hold its events, the path it is received at, and the environment variable its shared secret is read from.

The engine does no file I/O; the CLI is its thin filesystem shell. That split keeps every transform unit-testable as plain strings.

## How `anubis new` works

The starter tree is embedded into the `anubis` binary at build time, so stamping is offline and always matches the installed framework version. Stamping rewrites the app name across every path and file, then overlays the files that make the result a standalone repository: a workspace `Cargo.toml` carrying the framework's lint bar, a standalone `backend/Cargo.toml`, a root `package.json`, `.yarnrc.yml`, `.gitignore`, `README.md`, `.env.example`, `.github/workflows/ci.yml`, and the toolchain and clippy pins. Until the crate and npm package are published, stamped apps depend on the framework from its git repository (Cargo git dependency; yarn `#workspace=` git protocol). Drift-gate tests pin the overlay's dependency versions to the framework workspace, the CI template's tool versions to this repository's workflow, and the `.env.example` overlay to the one this repository runs on.

The stamped Rust is then formatted with `rustfmt`, for the same reason `scaffold model` formats its output: rewriting the crate name moves it inside every `use` block it appears in, since `axum` sorts after `acme` and before `zebra`, so no template order is right for every name a developer might type. A stamped tree therefore passes the `cargo fmt --check` its own CI runs first. A missing `rustfmt` is not fatal; the command says to run `cargo fmt --all` and carries on.

The stamped workflow holds the application to the framework's own bar, on GitHub-hosted runners: see [ci.md](ci.md#ci-for-stamped-applications).

### Development configuration

A stamped app configures development through one file. `.env.example` is committed and holds development values; `yarn dev` copies it to `.env` when there is none, then loads it, and passes it to `docker compose --env-file .env`. The database credentials therefore live in exactly one place: compose interpolates them (with the same values as defaults, so a bare `docker compose up` still works) and the backend reads `DATABASE_URL` from the same file. The database is named after the application, and each stamp mints its own development password, so no two applications ship the same default.

### License

Applications are private by default: `"license": "UNLICENSED"` in `package.json` and no LICENSE file. `anubis new <name> --license mit` writes an MIT LICENSE stamped with the current year and a placeholder for the copyright holder, and sets the manifest field to match.

### Lockfiles

Templates ship no lockfiles, and the application pins by committing `Cargo.lock` and `yarn.lock` with its first commit. Shipping a lockfile in a template pins the app to whatever the framework's own tree resolved on the day it was released, including entries for dependencies the app does not have, and it goes stale between releases in a way nothing checks. What the framework does pin exactly is the layer under the lockfiles: `rust-toolchain.toml`, `packageManager`, and the dependency versions in the stamped manifests, which the drift gate keeps equal to the versions the starter is tested against. Between a stamp and the first install, a caret range can therefore resolve a patch or minor the starter never saw; the stamped CI runs `yarn install --immutable`, so once that first commit exists the app is pinned and any later drift is a deliberate update.

## Route visibility

`anubis routes` prints the framework's mounted surface from two sources: a curated manifest in `anubis::manifest` (drift-gated by a test that composes the real routers and probes every entry) and the OpenAPI document, which contributes every versioned `/api/v1` operation automatically. Application-defined routes live in the application's router and are not visible to the CLI.

## What one `scaffold model` produces

Backend, implemented today:

- Diesel migration and `schema.rs` update
- Model struct with the ownership chain, validations, and `valid_*` scoping methods
- Entry in `roles.yml` permission grants, and the regenerated frontend permissions module
- Account CRUD handlers, routes wired into the router, and the model's integration test
- `/api/v1` CRUD handlers with utoipa registrations, mounted and merged into the application's OpenAPI document, and proven by the `/api/v1` half of that same test
- A serializer (`<Model>View`) shared by both surfaces, by the published document, and by the model's outgoing webhooks

Frontend, implemented today:

- ky route module per model, with the wire type and the permission model key
- Show page and form component at every depth, plus a list page (table, search, pagination) for a team-owned model
- `UrlTree` entries and link factory, routes, breadcrumbs that walk the chain back to its root, and, for a team-owned model, a navigation entry
- Per-model i18next locale file (labels, headings, help text), imported and merged in `i18n.ts`
- A nested model's section component, attached to its parent's show page, at either nested depth

Still to come:

- A generated-client refresh per scaffold. The `/api/v1` client (`frontend/src/api/v1.generated.ts`) is rendered from the application's exported document, so a scaffold changes what it would contain, but the run does not regenerate it: that takes compiling the application, which a text generator does not do. Run the two commands in [api.md](api.md#application-models) after a scaffold, as CI does.
- Scaffolded pages consuming the generated client. They call the hand-written ky route module today, which is the same contract typed by hand; the generated client is additive, for consumers outside the application. Which of the two a generated page should read is its own decision, and it is not settled here.
- Per-model frontend tests. The starter runs Vitest and the scaffolder's own frontend output is covered by `tsc`, ESLint, the production build, and the integration test that scaffolds two models and reads the result. The starter now has Playwright and three specs that drive the templates' own screens end to end (see [testing.md](testing.md#end-to-end-tests)); what remains is emitting one per scaffolded model, the narrative that creates a record, opens it, and edits it.

`scaffold field` propagates a new attribute through every one of those artifacts, which is the feature that makes the framework compound over time.

Still deferred for `scaffold field`:

- **One field per run** (run it twice for two).
- **Extending a model's sortable and filterable whitelists.** A scaffolded column reaches the record, both request bodies, the view, and the screens, but `SORTABLE` and the filter struct are still the model's own to edit. Which columns a list endpoint should let a caller order and search by is a product decision with a cost attached (each one is an index question), and a generator that added every column to both would answer it wrongly by default.
- **Fixed, translatable option lists** (`status:buttons`, `status:options`). `ButtonsField` and `OptionsField` ship, and Bullet Train's flow is to scaffold the field and then edit the option list in the model's locale file. What the two need here is a row in the field-type table plus an `options` array in the locale file the control reads, and a decision on whether the column is constrained by a `CHECK` or only by the form. This is a separate item from `{source=...}`, which names where a `super_select` reads its records from and is [implemented](#source-where-valid_-reads-from).

## Locked conventions the generator stamps

- **List endpoints** follow the page/limit, sort, and filter conventions in [api.md](api.md); the scaffolder maintains each model's sortable and filterable whitelists. `anubis::http::ListParams` and `anubis::http::Pagination` implement the convention once, so every generated endpoint pages and sorts identically.
- **Scoping methods** (`valid_*`): for every association, the scaffolder generates an inherent method, `valid_<associations>(connection, team_id)`, returning the team's own records ordered by name. The same method populates the select options endpoint and validates submitted ids on write, so a form can never smuggle in another tenant's record. One definition, both duties. For an ownership-chain parent it lives on the model that points at it, returning that parent's records; for a belongs_to it lives on the model carrying the foreign key, named after the attribute (`lead_id` gives `valid_leads`) and returning `anubis::http::FieldOption`, which is the one shape an application model and the framework's roster can both produce; for a has-many-through it lives on the join model, which is the only artifact that knows both sides and is generated once for every association that uses it.
- **Timestamps**: `created_at`/`updated_at` come from the database. `updated_at` is maintained by the shared `set_updated_at()` trigger, attached to every generated table; application code never sets either.

## Proving the generators

A generator is only as good as its last run, so the proof is continuous rather than ceremonial. `scripts/ci-scaffold-proof.sh` runs the whole family against the real `starter/` tree, in one sequence that covers every shape a template takes: a team-owned model with extra fields, a field added afterwards, every field type the templates prove, all three ownership depths with each nested model attaching itself to its parent's page, a field added at the deepest one, a join with the has-many-through field that reads through it, a membership assignment on two ownership depths, a model-backed assignment with `source` spelled out, a sign-in provider, and an incoming webhook receiver. It then holds the output to the bar the hand-written code is held to: `cargo fmt --check` on untouched generated Rust, clippy with warnings denied, the generated narratives against a real Postgres, and the frontend typecheck, lint, test, and production build over the generated TypeScript.

CI runs it on every push and pull request, and the job is blocking. Run it yourself before touching a living template, with `yarn dev` stopped so nothing else writes to the database it uses:

```sh
docker compose --env-file .env.example -f starter/compose.yaml up -d --wait
DATABASE_URL=postgres://anubis_starter:anubis-starter-dev-password@127.0.0.1:54321/anubis_starter_development \
  bash scripts/ci-scaffold-proof.sh
```

It leaves the generated models in `starter/` so the output is there to read, and [ci.md](ci.md#the-scaffold-proof) carries the three-path checkout and clean that takes the tree back. [ci.md](ci.md#the-scaffold-proof) states what each gate catches.

## Workflow

Domain modeling comes first and scaffolding makes it cheap, so follow Bullet Train's method: write the scaffold commands in a scratch file, review them with people before running them, run them, commit the generated code in its own commit, then polish the UX. Tearing down and re-scaffolding is cheap; live with a wrong domain model is not.

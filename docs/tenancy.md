# Tenancy, Teams, and Organizations

Anubis adopts Bullet Train's multi-tenancy model ("teams should be an MVP feature") and extends it two levels: Organizations sit above Teams, and SubTenants sit between them. Above all three sits the Platform, which is the deployment itself and is not a tenant at all.

## Entity model

```
User ─< OrganizationMembership >─ Organization
                                       │
                                       ├─< SubTenant
                                       │      │
User ─< SubTenantMembership >──────────┘      │
                                              │
User ─< TeamMembership >─ Team ───────────────┘  (nullable sub_tenant_id)
```

- **User**: a person who can log in. Owns credentials, profile, and preferences. Owns nothing domain-related directly.
- **Organization**: the top-level tenant. The billing and policy umbrella. It owns people and policy, not work.
- **SubTenant**: the tier that owns the work. GCP calls it a project, Jira calls it a project, GitHub calls it a repository, Linear calls it a workspace; the shape is the same every time. It belongs to exactly one Organization and holds its own membership and its own Teams.
- **Team**: the working tenant. All domain resources chain their ownership back to a Team.
- **OrganizationMembership**: joins a User to an Organization, carrying an `access` level and a `suspended_at` marker. Organization-wide role grants hang off it.
- **SubTenantMembership**: joins a User to a SubTenant, carrying its own `suspended_at`. This is the explicit grant a guest reaches a sub-tenant through, and the sub-tenant's role grants hang off it.
- **TeamMembership**: joins a User to a Team. Domain resources are assigned to TeamMemberships, never directly to Users. This allows assigning work to invited people who have not signed up yet, and keeps assignments intact when a user leaves. The team's role grants, and grants on single records of the team, hang off it.
- **Role**: one of an organization's own roles, a named, coloured, ordered bundle of permission keys. Seeded from `roles.yml` and edited from there; see [runtime roles](#runtime-roles).
- **RoleGrant**: one role held through one membership, which is the grant's scope.
- **Invitation**: created when someone is added to a Team or Organization by email. The emailed 256-bit token (hashed at rest, 14-day expiry) is the credential; whichever signed-in account holds it may claim, and claiming consumes the invitation. It offers roles by id. Team invitations pre-create the unclaimed TeamMembership and grant the offered roles on it, so the membership (id, grants, and any resource assignments) survives the claim intact; organization invitations create the OrganizationMembership at claim time and grant the roles their offer still names. Re-inviting an email replaces the pending invitation. Inviting takes `Tenancy::Membership.create` on the target, and the organization's managers may invite into any team in their organization. A manager can revoke a pending invitation, which discards the unclaimed membership with it; a claimed invitation no longer exists, so a claim cannot be taken back.

At signup, every user gets a personal Organization containing a default SubTenant ("Main") and a default Team ("General"), so solo use requires zero tenancy ceremony. The UI reveals organization complexity only when the user opts into it.

## The sub-tenant tier

The tier is optional in every sense that matters: an application that never surfaces it sees a complete ownership chain regardless, because every organization has a sub-tenant from the moment it exists, and an ownership chain that ends in `Team` keeps working exactly as it did. What the tier adds is a place for applications whose domain is organized around projects, repositories, or workspaces, where mapping that concept onto a Team collapses the tier the product is built around.

A Team's `sub_tenant_id` is nullable, and the two states are GitHub's split between an organization team and repository access:

- **Null** is an organization-level team, inherited by every sub-tenant in the organization. This is the state a team is created in, and the state every team was in before the tier existed.
- **Set** scopes the team to one sub-tenant, and it never leaks to another. A composite foreign key over `(sub_tenant_id, organization_id)` makes a team scoped to another organization's sub-tenant impossible in the schema rather than only in the queries that read it.

### Access resolution

`anubis::tenancy::resolve_sub_tenant_access` is the one function that answers who reaches a sub-tenant and with which roles, beside its two siblings for an organization and a team (see [who decides](#who-decides)). Nothing re-derives it: two call sites working the rules out for themselves would eventually disagree about who may read something, and a disagreement between two authorization paths is a data leak.

**A suspension is a deny.** It is checked before any grant and outranks every one of them, the organization administrator's bypass included, because a deny an admin bit silently ignored would not be a deny. A suspended organization membership cuts the member out of every tenant in the organization; a suspended sub-tenant membership cuts them out of that sub-tenant and the teams scoped to it. All three guards honor it.

Otherwise a path admits the caller when any of these hold, and the caller's roles there are the **union** of the default role and the grants of every path that applies. Permission is monotone in the roles held, so the union is exactly the strongest standing the caller holds across the tiers:

| Path | Rule |
|---|---|
| Organization administrator | The administrator role, granted organization-wide, bypasses the tier and reaches every sub-tenant in the organization |
| Full organization member | `access = full` cascades into every sub-tenant, with the organization-wide grants |
| Guest | `access = guest` cascades into nothing, and reaches only what was granted by name |
| Sub-tenant membership | An explicit grant, applying to full members and guests alike, since it can only add |
| Team | An organization-level team is inherited by every sub-tenant, with its team-wide grants; a scoped team reaches only its own |

Two consequences are worth stating rather than leaving to be discovered. An organization membership marked `guest` that also holds the administrator role is a contradiction an administrator can write, and the administrator role wins. And a team membership grants reach whatever the member's organization access says, because putting somebody on a team is itself the explicit act: an administrator confining a guest to one sub-tenant scopes the team to it rather than leaving the team organization-level.

The `SubTenantMember` guard is the first caller. It resolves the route's `{sub_tenant_id}` and answers `404` for a sub-tenant that does not exist and for one the caller cannot reach, byte-identical, so probing ids reveals nothing.

### Not built yet

The tier's foundation is the schema, the model, the resolution function, the guard, and the auto-created default. Still to come, tracked on [Anubis #93](https://github.com/JalapenoLabs/Anubis/issues/93):

- The scaffolder's template family for an ownership chain ending in `SubTenant`, and the equivalents of the three existing depth templates.
- Management endpoints and screens for creating, renaming, and deleting a sub-tenant, managing its roster, scoping a team to it, and setting `access` and `suspended_at`. Until they exist, an application writes those columns itself.
- The membership overview (`GET /tenancy/memberships`) does not yet group teams by sub-tenant.
- The last-admin invariant does not yet cover suspension, because nothing in the framework creates one. The endpoint that does must take the same organization lock the other membership changes take.

## The platform tier

The platform is the deployment. It is what the process is serving, there is exactly one of it, and it is nobody's tenant: an operator is not a member of every organization, they are above the question. So the tier has no entity, no roster, and no id in a route, and a role granted there lives on the user (`users.platform_roles`) rather than on a membership row.

A role reaches the tier only by naming it:

```yaml
operator:
  scopes: [platform]
  models:
    Sample: [manage]
```

That is the one scope which is not in the default set, and the reason is blast radius. Omitting `scopes` means every *tenancy* tier, so every role written before this tier existed keeps meaning exactly what it meant. A `default` role that silently became grantable over the whole deployment would be the worst possible default.

`anubis::guard::PlatformMember` is the guard. It admits an account holding any platform-scoped role and answers every other signed-in account `404`, byte-identical to a route that does not exist, for the same reason the tenant guards do it: an operator console that returns `403` has told a stranger it is there. Holding a platform role is admission; what an operator may do still comes from the compiled role set, so a handler calls `operator.require(Action::Update, "Sample")` exactly as a team handler does.

### Appointing the first operator

Every tenancy tier appoints administrators through somebody who is already one, and the platform has nobody. Two environment variables close the loop:

| Variable | Meaning |
|---|---|
| `ANUBIS_INITIAL_ADMIN_EMAIL` | The account that operates this deployment |
| `ANUBIS_INITIAL_ADMIN_PASSWORD` | The password, used only if the boot has to create the account |

`anubis::platform::ensure_initial_admin` applies them, and an application calls it once, right after its migrations. Both are validated when configuration loads, so a typo stops the boot beside every other configuration mistake rather than halfway through.

**The seed is a grant, never a reset**, which is what makes it safe to leave in a production environment:

- No account at that address: one is created with its email already verified (the deployment vouched for it, and there is no inbox to click a link in), its personal organization bootstrapped exactly as registration does it, and `operator` granted.
- An account without the role: the role is granted and **the password is not touched**, so the variable cannot become a standing override of somebody's chosen password, and a leaked deployment config is not a way into an existing account.
- An account that already holds it: nothing happens and nothing is written, including in the audit log.

Revoking is deliberately absent. Removing the variables leaves the operator in place, because a boot that silently demoted the only operator would lock the deployment out of itself. Every grant is recorded in the audit log as `platform.roles_changed`, attributed to the system, on neither a team nor an organization, exactly as a password change is.

### Bringing people in

After the first operator, an operator invites people, and that is the framework's half of an application's user manager. `anubis::platform::Accounts` is built once from the configuration and the mailer the auth router was given, and the application's operator routes call it; every method takes the `PlatformMember` the guard produced, which is both the proof the caller operates the deployment and who the audit log names. Which action each asks of `roles.yml` is the application's `operator.require(...)` before the call.

| Call | Effect |
|---|---|
| `accounts.invite(connection, &operator, &context, InviteRequest { email, platform_role })` | Writes an invitation, optionally carrying a platform role, and mails the link |
| `accounts.resend_invitation(connection, &operator, &context, invitation_id)` | Mails a fresh link with a fresh 24 hours; the previous link stops working |
| `accounts.revoke_invitation(connection, &operator, &context, invitation_id)` | Withdraws a pending invitation, so its link stops working |
| `platform::pending_invitations(connection, &list_params)` | The invitations neither accepted nor revoked, most recently sent first, paged |
| `accounts.set_temporary_password(connection, &operator, &context, user_id)` | Gives an account a generated password it must replace; see below |

**An invitation is a row, not an account.** `platform_invitations` holds the address, the role it will grant, who sent it (by id, and by a copied name that outlives their account), the SHA-256 of the emailed token, and when it was sent, expires, was accepted, or was revoked. The account is created when the invitee accepts, never before, so an address nobody answers for leaves no account, no personal organization, and no password anybody would have to invent. Until it is accepted, registering, an emailed code, and an OpenID Connect provider treat the address exactly as one nobody invited. Creating the account up front was the alternative, and it would make "pending" a property of an account, which a list of pending invitations then filters millions of accounts to find; here the list reads a partial index holding only the live rows.

**The rules**, each a sentence the operator reads rather than a constraint error:

- The address is normalized as registration normalizes it, and an address that already has an account is refused (`409`).
- One live invitation per address, held by a partial unique index that also decides two invites that race. A second invite is refused with a sentence that says to resend.
- A role must be one `roles.yml` lets the platform tier grant (`400` otherwise), checked against the same compiled file the guard admitted the operator with. It is granted when the account is created and recorded as `platform.roles_changed`, attributed to the new account.
- A link works for `INVITATION_TTL_HOURS`, 24. An expired invitation is still pending, says so through `PlatformInvitation::is_expired`, and a resend revives it.
- Resending or revoking an accepted or revoked invitation is refused (`409`). Resending to an address somebody registered meanwhile is refused too, and revoking is what clears it.
- Every decision holds the invitation row, so a resend racing an acceptance or a revocation reads the outcome rather than reviving it.
- Disposable-domain and corporate-only rules are not the framework's. An application that keeps one applies it before calling `invite`.

**The invitee's half is framework-owned**, mounted with the auth routes: `POST /auth/invitations/lookup` answers the address a live link was sent to, and `POST /auth/invitations/accept` takes the password (held to the registration policy), the optional time zone and locale a sign-up takes, creates the account with its **email verified** (opening the link proved the address) and its personal organization bootstrapped, marks the invitation accepted, and signs the account in exactly as login does, recording `session.created` with the method `invitation`. All of it commits in one transaction. The token travels in the body, never the path, because a path is what request tracing writes down. Unknown, used, revoked, and expired tokens answer the same `400` with the same sentence, both routes spend the credential-guessing budget, and an account created at the address after the invitation was sent is answered `409` rather than taken over. `useInvitation(token)` in `@jalapenolabs/anubis` drives the application's accept page.

The audit verbs are the tenancy invitation verbs on the subject type `PlatformInvitation`: `invitation.created`, `invitation.resent`, `invitation.revoked`, and `invitation.claimed`, on neither a team nor an organization.

### Temporary passwords

For the person who cannot sign in and cannot reach their inbox either. `accounts.set_temporary_password` generates a password (four groups of five characters from an alphabet without the ones people misread, about 99 bits), sets it, marks the account `password_change_required`, deletes every session the account had, revokes every OAuth grant it gave a connected client (each recorded as `oauth.revoked` by the operator), and records `password.temporary_set` with an empty change set, all in one transaction holding the account. It returns the password once as a `TemporaryPassword`, whose `Debug` prints a placeholder and whose memory is zeroed on drop; the operator hands it over by a channel they trust. It is never stored, mailed, logged, or audited.

The account then signs in normally, by password, and with its second factor if it has one, and the sign-in and `GET /auth/me` carry `password_change_required: true`. **Every other authenticated route answers `403` with the code `password_change_required`** until the account chooses a password of its own. The rule lives in the `CurrentUser` extractor, so it holds for every framework route, every guard built on it, and every application route that takes it, written yet or not. The exceptions take `SignedIn` instead, and there are two: `GET /auth/me`, so the SPA can see the flag, and `POST /auth/change-password`, which clears it. Signing out reads the cookie directly and was never gated. The realtime socket takes `CurrentUser`, so it stays closed until the password changes. A connected client is held to the same rule: setting the password already revoked every grant, `oauth_server::Bearer` answers any live token for a flagged account with the same `403` and the same code, and the consent screen cannot approve a new one because its routes take `CurrentUser`. See [oauth-server.md](oauth-server.md#the-resource-side).

Changing the password refuses a new password equal to the current one, which is what stops a temporary password from being "changed" into itself, and a password reset through the inbox clears the flag too. An account accepting an invitation sets its own password and never carries the flag.

Hashing it passes a gate of its own, one permit wide, because the auth router's gate is not reachable from an application. One computation at a time adds at most 19 MiB to what the deployment can be made to hold, and setting a temporary password is a rare act a handful of people perform.

### Not built yet

- Endpoints and a screen for listing operators and granting or revoking the role, and a screen for the invitations above. The primitives exist; the routes and the user manager are the application's, and the starter does not stamp them yet. Until it does, an application writes them, or writes `users.platform_roles` itself.
- Platform-scoped resources in the scaffolder. `anubis scaffold model` chains every model to a Team; a deployment-wide catalog is hand-written today.

## Ownership chain

Every scaffolded model declares its parent chain back to a Team, exactly like Bullet Train:

```
anubis scaffold model Goal Project,Team description:text_field
```

The chain drives everything: authorization scoping, nested routes, breadcrumbs, and the parent's show-view table. Tenant isolation is enforced by walking the chain, never by trusting a client-supplied id.

Enforcement is extractor-based. A handler that takes `anubis::guard::TeamMember` (or `OrganizationMember`) gets, before its body runs: authentication (401), the route's `{team_id}` resolved against the caller's membership (404 for non-members, byte-identical to a nonexistent id, so probing reveals nothing), and permission checks via `member.require(Action::Update, "Project")` against the compiled role set (403). Routers provide the needed request extensions with `anubis::guard::layer(pool, roles)`. Scaffolded models resolve their parent chain to the owning team and ride the same primitives.

Selectable associations are scoped through generated `valid_*` methods on the model. `anubis scaffold field Project lead_id:super_select{class_name=TeamMembership}` writes `Project::valid_leads`, and these methods populate select fields and enforce the tenancy boundary on write, so a form can never smuggle in another tenant's record. See [scaffolding.md](scaffolding.md#belongs_to-one-record-one-foreign-key).

**Assign to a membership, never to a user.** That is Bullet Train's advice and it is the framework's: a membership exists from the moment somebody is invited, so a record can be assigned to a teammate who has not signed up yet, and it disappears with them when they leave. The framework owns the roster half of that, because `team_memberships` is a framework table an application crate cannot reach through its own Diesel query graph: `TeamMembership::valid_for_team` returns the team's memberships as `anubis::http::FieldOption` values, ordered by the label a person reads, and `TeamMembership::labels_for` reads the labels of a page of assignments in one query. A label is the member's name, falling back to their account email, then to the address their invitation was sent to.

## Roles and permissions

Two things, deliberately kept apart: the **vocabulary** of permissions is compiled, and an organization's **roles** are data.

### The vocabulary

Permission keys are declared once, in `config/roles.yml`, and compiled into both ends of the stack. Every model the file names contributes its four actions (`read`, `create`, `update`, `destroy`, or `manage` as shorthand for all four), and the framework adds its own three subjects. A key is spelled `<Subject>.<action>`, so `CreativeConcept.update` and `Tenancy::Membership.create`:

| Subject | Actions | Guards |
|---|---|---|
| `Tenancy::Membership` | `create`, `update`, `destroy` | Inviting; granting and revoking roles; removing members and revoking invitations |
| `Tenancy::Role` | `create`, `update`, `destroy` | Making, editing, reordering, and deleting roles |
| `Billing::Subscription` | `update` | Spending the organization's money: checkout, the portal, reconciling |

The framework's subjects are namespaced, which no scaffolded model can be, so an application's own `Subscription` never collides with the framework's. A framework subject takes only the actions it guards, and `roles.yml` naming another is refused at boot, because a switch that guards nothing would mislead whoever reads the role editor.

What the framework keeps with the administrator role instead is deliberate: renaming or deleting a tenant, creating teams, the audit log, webhook endpoints, and platform applications. A platform application's token acts as the team's administrator and a webhook endpoint receives every model's payload, so a role able to mint either could exercise, through it, permissions it does not hold.

1. The backend embeds the file at compile time (`include_str!`) and resolves it at boot through `anubis::roles::RoleSet`, which rejects unknown includes, inheritance cycles, unknown actions, an action a framework subject does not take, malformed names and colours, and a structural role narrowed with `scopes`, before the server takes traffic. `anubis::roles::Permission` is a key, typed.
2. `anubis roles generate-ts` emits the TypeScript module (`roles.generated.ts`): the vocabulary as `PermissionKeys` and the `PermissionKey` type, what each `roles.yml` role grants, and `can(permissions, action, model)`. Output is deterministic, and CI regenerates it and fails on drift.

`anubis roles check` validates the file standalone.

### Runtime roles

Each organization holds its own roles: rows in `roles`, each a name, a colour (`#rrggbb`), a position, and a set of permission keys drawn from the vocabulary. **Position is rank**: higher outranks lower, positions stay dense, and every organization has two structural roles:

| Role | Seeded from | Position | What makes it structural |
|---|---|---|---|
| Default | `default` | always `0` | Everybody who reaches a tenant holds it there, without a grant; it is never granted and cannot be deleted. Its permissions are editable |
| Administrator | `admin` | always the highest | Holds every framework permission on top of what `roles.yml` grants `admin`; its permissions are not edited, it cannot be deleted, and its holders are exempt from the hierarchy |

**The recommended roles.** The roles `roles.yml` declares are what each organization is offered, in declaration order, least senior first, so the file reads as the ladder it seeds. An organization is created with its two structural roles (all registration and a platform invitation can build without the application's `roles.yml` in hand), and the rest are offered the first time the tenancy surface reads the organization's roles. `organizations.offered_role_keys` remembers what was offered, so a recommended role an organization deleted never comes back, and a role an application adds to `roles.yml` later arrives once, directly above the default role, where it outranks nobody an administrator already arranged.

**A seeded role follows `roles.yml` until it is edited.** Its key is kept, and each of its name, colour, and permissions follows that key's definition while the column is null. The first edit writes the value down, and from then on it is the organization's; the role editor offers to return permissions to the recommended set. Following is what lets a model scaffolded next month reach every organization's unedited roles without a data migration per model, and what makes a key removed from `roles.yml` grant nothing the moment the file says so. A stored permission key the vocabulary no longer holds grants nothing either: both fail closed.

`roles.yml` may give a role a display `name` and a `color`; without them a role reads as its key made readable, in a neutral grey. The well-known `billing` key keeps the meaning it always had: the framework grants it `Billing::Subscription.update` itself, so a file declaring `billing` with no grants of its own still pays for the organization.

### Grants and their scope

A grant is one role held through one membership (`role_grants`), and **the membership is the scope**:

| Held through | Scope | Applies |
|---|---|---|
| An organization membership | `organization` | Organization-wide: the organization's own surface, and every sub-tenant the member's access cascades into |
| A sub-tenant membership | `sub_tenant` | In that sub-tenant |
| A team membership | `team` | In that team |
| A team membership, with a record | `resource` | On that one record of the team and everything nested under it |

`scope_type` and `scope_id` are generated from the holder rather than written, so they can never disagree with it. Composite foreign keys tie the role, the membership, and the team or sub-tenant to the grant's organization, which makes a grant across organizations impossible in the schema, and removing a membership removes its grants in the same statement: somebody removed and invited back starts from what the new invitation offers, never from a grant the old membership held. A grant on a record names an application row the framework cannot see, so it is never a foreign key; a grant naming a record of another team is inert, because a grant is only read through the membership of the team that owns the record in hand.

**An organization-wide grant does not reach into a team.** A team's roles are the team's to grant, which is what a team administrator has always meant, and a team route admits and authorizes a member through their team membership alone. **A narrow grant never reaches wider**: a record grant is read only for that record, a team grant only in that team.

### Who decides

Three functions decide reach and permissions, each answering `None` for a tenant that does not exist and for one the caller cannot reach alike:

| Function | Answers for |
|---|---|
| `resolve_organization_access` | The organization itself: its roster, its roles, its money |
| `resolve_sub_tenant_access` | One sub-tenant and the work it owns, under the rules above |
| `resolve_team_access` | One team, narrowed to the records in hand |

Each returns an `Access`: the roles that apply there, highest first, the permissions they carry, `rank()` (the position of the highest), and `can`, `require`, and `require_administrator`. The guards carry it (`OrganizationMember::access`, `TeamMember::access`, `SubTenantMember::access`), and a scaffolded model's `load_for_member` resolves the team with the record and its parents as the records in hand, so a role granted on a record reaches it and everything nested under it. `GET /tenancy/memberships` answers each organization and team with the same resolved access, which is what the browser draws its affordances from.

The platform tier is untouched: a platform role stays a key held on the account and is asked about through `RoleSet::can`, and a platform application's token acts with the compiled `admin` grants, as it always has.

### The hierarchy

Holding a role-management permission lets somebody change what others may do, so every change is held to four rules (`anubis::tenancy` keeps them in one module, and every endpoint asks the same question the same way):

1. **Nobody acts on a role at or above their own rank**: editing, granting, revoking, reordering, or deleting it. **Nobody changes the roles of, or removes, somebody who ranks at or above them.** Changing your own roles is held to the role rules alone, because a role below you whose permissions you hold gives you nothing you lack. Rank is measured where the change happens: organization-wide for a role's definition and an organization grant, in the team for a team grant, on the record for a record grant.
2. **Only what you hold can be given.** Every permission a role edit switches on, and every permission a grant or an invitation hands out through a role, must be one the actor holds there. Switching a permission off is held to the same rule, so an edit made from a stale screen cannot quietly undo somebody else's.
3. **The administrator role is the top of the ladder, and exempt from it.** Its holders may act on any role, the administrator role included, which is how an administrator appoints the next one; nobody else grants, revokes, or edits it. Every organization keeps at least one organization-wide holder and every team at least one claimed team-wide holder (`409` otherwise), so role management can never be locked out of an organization.
4. **Every reorder states the whole ladder.** A reorder names every role but the two structural ones, each once, highest first; anything else is refused (`400`), so two people reordering at once can never interleave into an order neither asked for, and no list can tie two positions. A list that would move a role at or above the reorderer is refused whole (`403`).

**How these answer the ways a role editor gets abused:**

| Attempt | Answer |
|---|---|
| Editing a role below your own to add a permission you lack | Refused by rule 2 |
| Granting yourself, or inviting somebody into, a role carrying what you lack | Refused by rule 2, and by rule 1 for a role at or above you |
| Reordering a role above your own, or a partial list that ties positions | Refused by rule 4 |
| Editing the default role, which everybody holds, to raise everybody | Rule 2 limits the edit to what the editor holds; and a member whose highest role is the default role ranks `0`, so even a management permission on it acts on nothing |
| Freezing the top: nobody may touch a role at or above their own | The administrator role is exempt, and appoints the next administrator |
| Removing the last holder of role management | The administrator role holds every framework permission, cannot be deleted, and always keeps a holder |
| Raising organization-wide rank through a team or record grant | Rank is measured where the change happens; a narrow grant never reaches wider |
| Naming another organization's role, team, or membership | `404`, byte-identical to an id that does not exist |
| Two managers editing one role, or reordering, at once | Every role and grant change locks the organization row first and reads the actor's standing again under it, so the second is judged against what the first left behind |

**Deleting a role** revokes it from everybody holding it, at once, through the cascading foreign key: holders keep their other roles and the default role, and each is told with the "roles changed" notification. The roles above close the gap.

**Every change is audited** in the transaction that makes it: `role.created`, `role.updated`, `role.destroyed`, and `role.reordered` on the organization, and `member.role_changed` on the tenant the grant is in, each naming roles as they read at the time.

### The migration from role keys

Before roles were data, a membership carried a `roles TEXT[]` of `roles.yml` keys. Migration `000029` converts every one: each organization gets a role for every key its people held, plus the two structural roles, all following `roles.yml`; each key a membership held becomes a grant of that role at the membership's scope; an organization invitation's keys become its offer; and the columns are dropped. Nothing is lost and nothing is gained but the default role every member now holds. Between the two structural roles, the converted roles are ordered by key, because the migration cannot read `roles.yml`; an administrator reorders them once. `anubis/tests/roles_migration_flow.rs` pins every legacy shape (administrators of both tiers, team-only members, guests, suspended members, sub-tenant grants, invitations in flight).

Going back narrows and never widens: a role maps back to its key only while it still means exactly that key, so one the organization made or edited is dropped rather than widened, and a record grant is dropped rather than widened to its team.

## Managing tenants

Tenancy is manageable from the API, not only at signup. The routes mount under `/tenancy` alongside the membership overview and the invitation endpoints:

| Route | Who | Effect |
|---|---|---|
| `POST /tenancy/organizations` | any signed-in user | Create an organization with its default team; the creator administers both |
| `PATCH /tenancy/organizations/{organization_id}` | org administrator | Rename the organization |
| `DELETE /tenancy/organizations/{organization_id}` | org administrator | Delete the organization, its teams, and their records |
| `POST /tenancy/organizations/{organization_id}/teams` | org administrator | Create a team, with the creator as its administrator |
| `DELETE /tenancy/organizations/{organization_id}/teams/{team_id}` | org administrator | Delete a team and its records |
| `GET /tenancy/organizations/{organization_id}/members` | org member | The organization roster, outstanding invitations included |
| `PATCH /tenancy/organizations/{organization_id}/members/{membership_id}` | `Tenancy::Membership.update` | Replace a member's organization-wide roles |
| `DELETE /tenancy/organizations/{organization_id}/members/{membership_id}` | `Tenancy::Membership.destroy` | Remove another organization member |
| `POST /tenancy/organizations/{organization_id}/leave` | org member | Leave the organization |
| `DELETE /tenancy/organizations/{organization_id}/invitations/{invitation_id}` | `Tenancy::Membership.destroy` | Revoke any pending invitation in the organization |
| `GET /tenancy/organizations/{organization_id}/roles` | org member | The roles, the vocabulary, and the caller's standing |
| `POST /tenancy/organizations/{organization_id}/roles` | `Tenancy::Role.create` | Make a role, directly above the default role |
| `PATCH /tenancy/organizations/{organization_id}/roles/{role_id}` | `Tenancy::Role.update` | Rename, recolour, or change a role's permissions |
| `DELETE /tenancy/organizations/{organization_id}/roles/{role_id}` | `Tenancy::Role.destroy` | Delete a role, revoking it from everybody |
| `PUT /tenancy/organizations/{organization_id}/role-order` | `Tenancy::Role.update` | Put the roles in a new order |
| `PATCH /tenancy/teams/{team_id}` | team administrator | Rename the team |
| `GET /tenancy/teams/{team_id}/roles` | team member | The organization's roles, with the caller's standing in the team |
| `PATCH /tenancy/teams/{team_id}/members/{membership_id}` | `Tenancy::Membership.update` | Replace a member's roles in the team, or on one record of it |
| `DELETE /tenancy/teams/{team_id}/members/{membership_id}` | `Tenancy::Membership.destroy` | Remove another member |
| `POST /tenancy/teams/{team_id}/leave` | team member | Leave the team |
| `DELETE /tenancy/teams/{team_id}/invitations/{invitation_id}` | `Tenancy::Membership.destroy` | Revoke a pending team invitation |

Team-scoped routes take the `TeamMember` guard, organization-scoped routes take `OrganizationMember`, and sub-tenant-scoped routes take `SubTenantMember`, so the discipline is the one every ownership chain follows: `401` when signed out, `404` when the caller is not a member (byte-identical to a nonexistent id), and `403` when a member lacks the permission or the hierarchy refuses. Dissolving a team is an organization act rather than a team act, which is why deletion is nested under the organization: a team's own administrators run the team, and the organization decides whether the team exists.

Roles are replaced wholesale rather than patched, so a request states the end state (`role_ids`) and two managers editing the same member cannot interleave into a set neither asked for. Every id must be one of the organization's roles (`404` otherwise, whoever's it is), every role added is held to the hierarchy, and an empty list leaves the member with the default role alone.

A tenant's name is one to a hundred characters and carries no control characters. The length is a display bound; the control characters are refused because the name is rendered into an invitation's subject line, where a line break is at best a display bug and at worst an attempt at a header of the caller's own. An invited email address goes through the same validation registration uses, for the same reason. The mail layer encodes whatever it is given, so both are the second lock rather than the only one.

Inviting sends mail to an address the request names, and re-inviting is allowed, so the endpoint charges the same per-recipient inbox budget a password reset charges: five an hour per address, across every surface, since the application builds one limiter and passes it to both routers. The charge lands after the inviter is shown to administer the target, so no signed-in account can spend a stranger's budget. See [rate limiting](api.md#rate-limiting).

The two rosters answer the two levels. A team roster row is a TeamMembership, so an invited person already holds one and carries the `invitation_id` an admin revokes. An organization roster row is either an OrganizationMembership or an invitation that has not created one yet, which is why its `membership_id` is null exactly when `pending` is true. Team invitations belong to their team's roster rather than to the organization's, so each place is listed once.

Both levels are left the same way: `leave` releases the caller's own membership, and removing somebody else is an admin act with its own route, so a request can never mean both. Trying to remove yourself answers `400` and names the leave route. Leaving an organization releases the organization membership and nothing else; the teams inside it are separate memberships, left team by team, because a person working in one team without standing in its organization is a state the model already has (a team-only invitation produces exactly that).

### Invariants

**A team always keeps at least one claimed administrator**: a claimed member holding the administrator role team-wide. Taking the role from the last one, or the last one leaving, answers `409 Conflict`: the request is well formed and only the current state refuses it, which is exactly what `409` says. Unclaimed memberships never count, because an invitation is not a person.

The rule is enforced after the change rather than before it, inside a transaction that first locks the organization's row, then the team's. Both halves matter. Counting afterwards means one rule covers demotion, removal, and leaving alike, and the transaction rolls the change back when it fires. Locking means two administrators acting at the same instant are ordered rather than each reading the other as the one who remains and both committing, which is the one way a team could have been left with nobody able to administer it, and the caller's standing is read again under the lock, so a manager demoted a moment ago is judged by the standing they have now. Every role and membership change in an organization takes the organization's row first, in one order everywhere, because two transactions each locking the other's next row deadlock, and a deadlock is a `500` where a `409` belongs.

So removing another member normally cannot break the rule, since the caller outranks the member being removed, and the one case that can reach the `409` is a caller whose own administrator role was taken by a request that committed while theirs was in flight.

**An organization keeps an administrator under the same rule**, enforced the same way: the holders of the administrator role organization-wide. The last one cannot step down or leave, and gets the same `409`. The way out of a personal organization is therefore to appoint someone or to delete it, not to walk out of it and leave it unreachable. Account deletion is the one path that cannot refuse, and it appoints instead (below).

**Nothing marks the organization created at signup as special.** Its admin may rename it or delete it like any other. Introducing a "personal" flag purely to forbid one deletion would add a permanent concept to the model in order to protect a state that a user rebuilds with a single `POST /tenancy/organizations`. A user with no organization is a valid, recoverable state; a schema concept nobody else needs is not.

### Cascade semantics

Deletion cascades, deliberately. The framework's migrations declare `ON DELETE CASCADE` from `organizations` to sub-tenants, teams, memberships, and invitations; from `sub_tenants` to their memberships and to the teams scoped to them; and from `teams` to memberships, invitations, and platform applications. The scaffolder's living templates declare the same on the ownership chain: a team-owned table's `team_id` references `teams(id) ON DELETE CASCADE`, and a nested model cascades from its parent. So deleting a team deletes the records that chain to it, and deleting an organization deletes its teams first.

Cascade is the right default here because the ownership chain already means "this record exists inside that team". Restrict would force a caller to empty a team by hand through endpoints that may not exist yet, and orphaning is not an option since a record with no team has no tenant and therefore no reachable authorization. An application that deliberately declares a restricting reference of its own is respected rather than overridden: the delete answers `409 Conflict` telling the caller to remove those records first, instead of failing as a `500`.

### Account deletion

Deleting an account is terminal and must always succeed, so it settles what the user leaves behind instead of refusing. In the same transaction that removes the user, the framework:

1. Deletes the user's organization, sub-tenant, and team memberships.
2. Deletes every organization nobody can reach any more, meaning no organization memberships, no claimed team memberships, and no sub-tenant memberships remain anywhere in it. This is what keeps a personal organization from outliving its only member, and what stops a guest's project being deleted out from under them.
3. Keeps every surviving organization and team administrable. One that still has members but lost its last administrator grants the administrator role to its longest-standing remaining member. If a surviving organization has no organization memberships left at all, and lives on only through its teams or its sub-tenants, the longest-standing member of either gains the organization membership and the administrator role, teams first.

A surviving team with no members left is kept rather than deleted: it still owns application records, and an organization admin can delete it or invite people back into it. The invariant the interactive endpoints defend by refusing, this path defends by succeeding.

## Billing

Subscriptions attach to the Organization, not the User and not the Team. Plans live in `config/billing.yml`, an organization with no subscription is on the free plan, and Stripe is the system of record for money. See [billing.md](billing.md).

Tenancy meets billing in one place: **the `seats` limit is enforced when an invitation is created.** An invitation that would put the organization over its plan's seats answers `409` with a message naming the plan. A seat is a person counted once, by email address, across organization memberships, claimed team memberships, and invitations still claimable, so somebody in three teams pays for one seat and re-inviting an existing member costs nothing. The check runs inside the transaction that writes the invitation, under the same organization lock the last-admin rule takes, so two admins inviting at the same instant are ordered rather than each seeing room for one more.

Every membership change (invite, claim, revoke, remove, leave, delete a team) also queues the job that tells Stripe the new seat count, but only when a plan sells a per-seat price. An application with no `config/billing.yml` passes `None` to `anubis::tenancy::router` and has neither behavior.

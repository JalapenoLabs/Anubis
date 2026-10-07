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
- **OrganizationMembership**: joins a User to an Organization, carrying org-level roles (org admin, billing), an `access` level, and a `suspended_at` marker.
- **SubTenantMembership**: joins a User to a SubTenant, carrying sub-tenant-level roles and its own `suspended_at`. This is the explicit grant a guest reaches a sub-tenant through.
- **TeamMembership**: joins a User to a Team, carrying team-level roles. Domain resources are assigned to TeamMemberships, never directly to Users. This allows assigning work to invited people who have not signed up yet, and keeps assignments intact when a user leaves.
- **Invitation**: created when someone is added to a Team or Organization by email. The emailed 256-bit token (hashed at rest, 14-day expiry) is the credential; whichever signed-in account holds it may claim, and claiming consumes the invitation. Team invitations pre-create the unclaimed TeamMembership, so the membership (id, roles, and any resource assignments) survives the claim intact; organization invitations create the OrganizationMembership at claim time. Re-inviting an email replaces the pending invitation. Inviting requires the admin role on the target, and organization admins may invite to any team in their organization. An admin can revoke a pending invitation, which discards the unclaimed membership with it; a claimed invitation no longer exists, so a claim cannot be taken back.
- **Role**: declared in `roles.yml`, granted through memberships at any of the three levels, or held on the user at the platform tier.

At signup, every user gets a personal Organization containing a default SubTenant ("Main") and a default Team ("General"), so solo use requires zero tenancy ceremony. The UI reveals organization complexity only when the user opts into it.

## The sub-tenant tier

The tier is optional in every sense that matters: an application that never surfaces it sees a complete ownership chain regardless, because every organization has a sub-tenant from the moment it exists, and an ownership chain that ends in `Team` keeps working exactly as it did. What the tier adds is a place for applications whose domain is organized around projects, repositories, or workspaces, where mapping that concept onto a Team collapses the tier the product is built around.

A Team's `sub_tenant_id` is nullable, and the two states are GitHub's split between an organization team and repository access:

- **Null** is an organization-level team, inherited by every sub-tenant in the organization. This is the state a team is created in, and the state every team was in before the tier existed.
- **Set** scopes the team to one sub-tenant, and it never leaks to another. A composite foreign key over `(sub_tenant_id, organization_id)` makes a team scoped to another organization's sub-tenant impossible in the schema rather than only in the queries that read it.

### Access resolution

`anubis::tenancy::resolve_sub_tenant_access` is the one function that answers who reaches a sub-tenant and with which roles. Nothing re-derives it: two call sites working the rules out for themselves would eventually disagree about who may read something, and a disagreement between two authorization paths is a data leak.

**A suspension is a deny.** It is checked before any grant and outranks every one of them, the organization administrator's bypass included, because a deny an admin bit silently ignored would not be a deny. A suspended organization membership cuts the member out of every tenant in the organization; a suspended sub-tenant membership cuts them out of that sub-tenant and the teams scoped to it. All three guards honor it.

Otherwise a grant applies when any of these hold, and the caller's resolved roles are the **union** of every grant that applies. Permission is monotone in the role set (`RoleSet::can` asks whether *any* held role grants the action), so the union is exactly the strongest standing the caller holds across the tiers:

| Path | Rule |
|---|---|
| Organization admin | The `admin` role bypasses the tier and reaches every sub-tenant in the organization |
| Full organization member | `access = full` cascades into every sub-tenant |
| Guest | `access = guest` cascades into nothing, and reaches only what was granted by name |
| Sub-tenant membership | An explicit grant, applying to full members and guests alike, since it can only add |
| Team | An organization-level team is inherited by every sub-tenant; a scoped team reaches only its own |

Two consequences are worth stating rather than leaving to be discovered. An organization membership marked `guest` that also holds `admin` is a contradiction an administrator can write, and admin wins. And a team membership grants reach whatever the member's organization access says, because putting somebody on a team is itself the explicit act: an administrator confining a guest to one sub-tenant scopes the team to it rather than leaving the team organization-level.

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

For the person who cannot sign in and cannot reach their inbox either. `accounts.set_temporary_password` generates a password (four groups of five characters from an alphabet without the ones people misread, about 99 bits), sets it, marks the account `password_change_required`, deletes every session the account had, and records `password.temporary_set` with an empty change set, all in one transaction holding the account. It returns the password once as a `TemporaryPassword`, whose `Debug` prints a placeholder and whose memory is zeroed on drop; the operator hands it over by a channel they trust. It is never stored, mailed, logged, or audited.

The account then signs in normally, by password, and with its second factor if it has one, and the sign-in and `GET /auth/me` carry `password_change_required: true`. **Every other authenticated route answers `403` with the code `password_change_required`** until the account chooses a password of its own. The rule lives in the `CurrentUser` extractor, so it holds for every framework route, every guard built on it, and every application route that takes it, written yet or not. The exceptions take `SignedIn` instead, and there are two: `GET /auth/me`, so the SPA can see the flag, and `POST /auth/change-password`, which clears it. Signing out reads the cookie directly and was never gated. The realtime socket takes `CurrentUser`, so it stays closed until the password changes. A connected client is held to the same rule: `oauth_server::Bearer` answers a live token for a flagged account with the same `403` and the same code, so a program the account connected before the rescue cannot act for it on the operator's password, and the consent screen cannot approve a new one because its routes take `CurrentUser`. See [oauth-server.md](oauth-server.md#the-resource-side).

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

Roles are declared once, in `config/roles.yml`, with role inheritance (`admin` includes `editor` and `billing`) and per-model action grants (`read`, `create`, `update`, `destroy`, or `manage` as shorthand for all four), modeled on `bullet_train-roles`. The starter ships the baseline vocabulary: `default`, `editor`, `billing`, and `admin`.

A role may also name `scopes`, the tiers it can be granted at, out of `platform`, `organization`, `sub_tenant`, and `team`. Omitting it means every tenancy tier, which is what a role written before either newer tier existed keeps meaning; the starter scopes `billing` to `organization`, because subscriptions attach to the organization and the role means nothing anywhere else, and `operator` to `platform`. Scopes say where a role key attaches, never what it grants, so they do not travel through `includes`. `RoleSet::is_grantable_at` is the backend's question and `isGrantableAt` is the SPA's.

One definition drives both sides of the stack:

1. The backend embeds the file at compile time (`include_str!`) and resolves it at boot through `anubis::roles::RoleSet`, which rejects unknown includes, inheritance cycles, and unknown actions before the server takes traffic. Authorization asks `RoleSet::can(held_roles, action, model)`.
2. `anubis roles generate-ts` emits the TypeScript permissions module (`roles.generated.ts`) the SPA uses to hide or disable controls the current member cannot use. Output is deterministic, and CI regenerates it and fails on drift.

`anubis roles check` validates the file standalone.

## Managing tenants

Tenancy is manageable from the API, not only at signup. The routes mount under `/tenancy` alongside the membership overview and the invitation endpoints:

| Route | Who | Effect |
|---|---|---|
| `POST /tenancy/organizations` | any signed-in user | Create an organization with its default team; the creator administers both |
| `PATCH /tenancy/organizations/{organization_id}` | org admin | Rename the organization |
| `DELETE /tenancy/organizations/{organization_id}` | org admin | Delete the organization, its teams, and their records |
| `POST /tenancy/organizations/{organization_id}/teams` | org admin | Create a team, with the creator as its admin member |
| `DELETE /tenancy/organizations/{organization_id}/teams/{team_id}` | org admin | Delete a team and its records |
| `GET /tenancy/organizations/{organization_id}/members` | org member | The organization roster, outstanding invitations included |
| `DELETE /tenancy/organizations/{organization_id}/members/{membership_id}` | org admin | Remove another organization member |
| `POST /tenancy/organizations/{organization_id}/leave` | org member | Leave the organization |
| `DELETE /tenancy/organizations/{organization_id}/invitations/{invitation_id}` | org admin | Revoke any pending invitation in the organization |
| `PATCH /tenancy/teams/{team_id}` | team admin | Rename the team |
| `PATCH /tenancy/teams/{team_id}/members/{membership_id}` | team admin | Replace a member's roles |
| `DELETE /tenancy/teams/{team_id}/members/{membership_id}` | team admin | Remove another member |
| `POST /tenancy/teams/{team_id}/leave` | team member | Leave the team |
| `DELETE /tenancy/teams/{team_id}/invitations/{invitation_id}` | team admin | Revoke a pending team invitation |

Team-scoped routes take the `TeamMember` guard, organization-scoped routes take `OrganizationMember`, and sub-tenant-scoped routes take `SubTenantMember`, so the discipline is the one every ownership chain follows: `401` when signed out, `404` when the caller is not a member (byte-identical to a nonexistent id), and `403` when a member lacks the role. Dissolving a team is an organization act rather than a team act, which is why deletion is nested under the organization: a team's own admins run the team, and the organization decides whether the team exists.

Roles are replaced wholesale rather than patched, so a request states the end state and two admins editing the same member cannot interleave into a set neither asked for. Every requested key is checked against the compiled `roles.yml`; an unknown key is a `400`, and an empty list means the baseline `default` role.

A tenant's name is one to a hundred characters and carries no control characters. The length is a display bound; the control characters are refused because the name is rendered into an invitation's subject line, where a line break is at best a display bug and at worst an attempt at a header of the caller's own. An invited email address goes through the same validation registration uses, for the same reason. The mail layer encodes whatever it is given, so both are the second lock rather than the only one.

Inviting sends mail to an address the request names, and re-inviting is allowed, so the endpoint charges the same per-recipient inbox budget a password reset charges: five an hour per address, across every surface, since the application builds one limiter and passes it to both routers. The charge lands after the inviter is shown to administer the target, so no signed-in account can spend a stranger's budget. See [rate limiting](api.md#rate-limiting).

The two rosters answer the two levels. A team roster row is a TeamMembership, so an invited person already holds one and carries the `invitation_id` an admin revokes. An organization roster row is either an OrganizationMembership or an invitation that has not created one yet, which is why its `membership_id` is null exactly when `pending` is true. Team invitations belong to their team's roster rather than to the organization's, so each place is listed once.

Both levels are left the same way: `leave` releases the caller's own membership, and removing somebody else is an admin act with its own route, so a request can never mean both. Trying to remove yourself answers `400` and names the leave route. Leaving an organization releases the organization membership and nothing else; the teams inside it are separate memberships, left team by team, because a person working in one team without standing in its organization is a state the model already has (a team-only invitation produces exactly that).

### Invariants

**A team always keeps at least one claimed admin.** Demoting the last admin, or the last admin leaving, answers `409 Conflict`: the request is well formed and only the current state refuses it, which is exactly what `409` says. Unclaimed memberships never count as admins, because an invitation is not a person.

The rule is enforced after the change rather than before it, inside a transaction that first locks the tenant's own row. Both halves matter. Counting afterwards means one rule covers demotion, removal, and leaving alike, and the transaction rolls the change back when it fires. Locking the tenant means two admins acting at the same instant are ordered rather than each reading the other as the admin who remains and both committing, which is the one way a team could have been left with nobody able to administer it. The lock is on the team or organization row, not on the memberships, because two transactions each locking the other's target deadlock, and a deadlock is a `500` where a `409` belongs.

So removing another member normally cannot break the rule, since the caller is a claimed admin and is not the member being removed, and the one case that can reach the `409` is a caller whose own admin role was taken by a request that committed while theirs was in flight.

**An organization keeps an admin under the same rule**, enforced the same way. The last organization admin cannot leave, and gets the same `409`; every organization membership is claimed, so all of them count. The way out of a personal organization is therefore to promote someone or to delete it, not to walk out of it and leave it unreachable. Account deletion is the one path that cannot refuse, and it promotes instead (below).

**Nothing marks the organization created at signup as special.** Its admin may rename it or delete it like any other. Introducing a "personal" flag purely to forbid one deletion would add a permanent concept to the model in order to protect a state that a user rebuilds with a single `POST /tenancy/organizations`. A user with no organization is a valid, recoverable state; a schema concept nobody else needs is not.

### Cascade semantics

Deletion cascades, deliberately. The framework's migrations declare `ON DELETE CASCADE` from `organizations` to sub-tenants, teams, memberships, and invitations; from `sub_tenants` to their memberships and to the teams scoped to them; and from `teams` to memberships, invitations, and platform applications. The scaffolder's living templates declare the same on the ownership chain: a team-owned table's `team_id` references `teams(id) ON DELETE CASCADE`, and a nested model cascades from its parent. So deleting a team deletes the records that chain to it, and deleting an organization deletes its teams first.

Cascade is the right default here because the ownership chain already means "this record exists inside that team". Restrict would force a caller to empty a team by hand through endpoints that may not exist yet, and orphaning is not an option since a record with no team has no tenant and therefore no reachable authorization. An application that deliberately declares a restricting reference of its own is respected rather than overridden: the delete answers `409 Conflict` telling the caller to remove those records first, instead of failing as a `500`.

### Account deletion

Deleting an account is terminal and must always succeed, so it settles what the user leaves behind instead of refusing. In the same transaction that removes the user, the framework:

1. Deletes the user's organization, sub-tenant, and team memberships.
2. Deletes every organization nobody can reach any more, meaning no organization memberships, no claimed team memberships, and no sub-tenant memberships remain anywhere in it. This is what keeps a personal organization from outliving its only member, and what stops a guest's project being deleted out from under them.
3. Keeps every surviving organization and team administrable. One that still has members but lost its last admin promotes its longest-standing remaining member. If a surviving organization has no organization memberships left at all, and lives on only through its teams or its sub-tenants, the longest-standing member of either gains the organization membership as its admin, teams first.

A surviving team with no members left is kept rather than deleted: it still owns application records, and an organization admin can delete it or invite people back into it. The invariant the interactive endpoints defend by refusing, this path defends by succeeding.

## Billing

Subscriptions attach to the Organization, not the User and not the Team. Plans live in `config/billing.yml`, an organization with no subscription is on the free plan, and Stripe is the system of record for money. See [billing.md](billing.md).

Tenancy meets billing in one place: **the `seats` limit is enforced when an invitation is created.** An invitation that would put the organization over its plan's seats answers `409` with a message naming the plan. A seat is a person counted once, by email address, across organization memberships, claimed team memberships, and invitations still claimable, so somebody in three teams pays for one seat and re-inviting an existing member costs nothing. The check runs inside the transaction that writes the invitation, under the same organization lock the last-admin rule takes, so two admins inviting at the same instant are ordered rather than each seeing room for one more.

Every membership change (invite, claim, revoke, remove, leave, delete a team) also queues the job that tells Stripe the new seat count, but only when a plan sells a per-seat price. An application with no `config/billing.yml` passes `None` to `anubis::tenancy::router` and has neither behavior.

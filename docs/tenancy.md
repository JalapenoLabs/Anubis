# Tenancy, Teams, and Organizations

Anubis adopts Bullet Train's multi-tenancy model ("teams should be an MVP feature") and extends it one level: Organizations sit above Teams.

## Entity model

```
User ─< OrganizationMembership >─ Organization
User ─< TeamMembership >─ Team ─> Organization
```

- **User**: a person who can log in. Owns credentials, profile, and preferences. Owns nothing domain-related directly.
- **Organization**: the top-level tenant. Owns Teams, billing, and org-wide settings. Every Team belongs to exactly one Organization.
- **Team**: the working tenant. All domain resources chain their ownership back to a Team.
- **OrganizationMembership**: joins a User to an Organization, carrying org-level roles (org admin, billing).
- **TeamMembership**: joins a User to a Team, carrying team-level roles. Domain resources are assigned to TeamMemberships, never directly to Users. This allows assigning work to invited people who have not signed up yet, and keeps assignments intact when a user leaves.
- **Invitation**: created when someone is added to a Team or Organization by email. The emailed 256-bit token (hashed at rest, 14-day expiry) is the credential; whichever signed-in account holds it may claim, and claiming consumes the invitation. Team invitations pre-create the unclaimed TeamMembership, so the membership (id, roles, and any resource assignments) survives the claim intact; organization invitations create the OrganizationMembership at claim time. Re-inviting an email replaces the pending invitation. Inviting requires the admin role on the target, and organization admins may invite to any team in their organization. An admin can revoke a pending invitation, which discards the unclaimed membership with it; a claimed invitation no longer exists, so a claim cannot be taken back.
- **Role**: declared in `roles.yml`, granted through memberships at either level.

At signup, every user gets a personal Organization containing a default Team, so solo use requires zero tenancy ceremony. The UI reveals organization complexity only when the user opts into it.

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

Team-scoped routes take the `TeamMember` guard and organization-scoped routes take `OrganizationMember`, so the discipline is the one every ownership chain follows: `401` when signed out, `404` when the caller is not a member (byte-identical to a nonexistent id), and `403` when a member lacks the role. Dissolving a team is an organization act rather than a team act, which is why deletion is nested under the organization: a team's own admins run the team, and the organization decides whether the team exists.

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

Deletion cascades, deliberately. The framework's migrations declare `ON DELETE CASCADE` from `organizations` to teams, memberships, and invitations, and from `teams` to memberships, invitations, and platform applications. The scaffolder's living templates declare the same on the ownership chain: a team-owned table's `team_id` references `teams(id) ON DELETE CASCADE`, and a nested model cascades from its parent. So deleting a team deletes the records that chain to it, and deleting an organization deletes its teams first.

Cascade is the right default here because the ownership chain already means "this record exists inside that team". Restrict would force a caller to empty a team by hand through endpoints that may not exist yet, and orphaning is not an option since a record with no team has no tenant and therefore no reachable authorization. An application that deliberately declares a restricting reference of its own is respected rather than overridden: the delete answers `409 Conflict` telling the caller to remove those records first, instead of failing as a `500`.

### Account deletion

Deleting an account is terminal and must always succeed, so it settles what the user leaves behind instead of refusing. In the same transaction that removes the user, the framework:

1. Deletes the user's organization and team memberships.
2. Deletes every organization nobody can reach any more, meaning no organization memberships and no claimed team memberships remain anywhere in it. This is what keeps a personal organization from outliving its only member.
3. Keeps every surviving organization and team administrable. One that still has members but lost its last admin promotes its longest-standing remaining member. If a surviving organization has no organization memberships left at all, and lives on only through its teams, the longest-standing team member gains the organization membership as its admin.

A surviving team with no members left is kept rather than deleted: it still owns application records, and an organization admin can delete it or invite people back into it. The invariant the interactive endpoints defend by refusing, this path defends by succeeding.

## Billing

Subscriptions attach to the Organization, not the User and not the Team. Plans live in `config/billing.yml`, an organization with no subscription is on the free plan, and Stripe is the system of record for money. See [billing.md](billing.md).

Tenancy meets billing in one place: **the `seats` limit is enforced when an invitation is created.** An invitation that would put the organization over its plan's seats answers `409` with a message naming the plan. A seat is a person counted once, by email address, across organization memberships, claimed team memberships, and invitations still claimable, so somebody in three teams pays for one seat and re-inviting an existing member costs nothing. The check runs inside the transaction that writes the invitation, under the same organization lock the last-admin rule takes, so two admins inviting at the same instant are ordered rather than each seeing room for one more.

Every membership change (invite, claim, revoke, remove, leave, delete a team) also queues the job that tells Stripe the new seat count, but only when a plan sells a per-seat price. An application with no `config/billing.yml` passes `None` to `anubis::tenancy::router` and has neither behavior.

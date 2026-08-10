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
- **Invitation**: created when someone is added to a Team or Organization by email. The emailed 256-bit token (hashed at rest, 14-day expiry) is the credential; whichever signed-in account holds it may claim, and claiming consumes the invitation. Team invitations pre-create the unclaimed TeamMembership, so the membership (id, roles, and any resource assignments) survives the claim intact; organization invitations create the OrganizationMembership at claim time. Re-inviting an email replaces the pending invitation. Inviting requires the admin role on the target, and organization admins may invite to any team in their organization.
- **Role**: declared in `roles.yml`, granted through memberships at either level.

At signup, every user gets a personal Organization containing a default Team, so solo use requires zero tenancy ceremony. The UI reveals organization complexity only when the user opts into it.

## Ownership chain

Every scaffolded model declares its parent chain back to a Team, exactly like Bullet Train:

```
anubis scaffold model Goal Project,Team description:text_field
```

The chain drives everything: authorization scoping, nested routes, breadcrumbs, and the parent's show-view table. Tenant isolation is enforced by walking the chain, never by trusting a client-supplied id.

Enforcement is extractor-based. A handler that takes `anubis::guard::TeamMember` (or `OrganizationMember`) gets, before its body runs: authentication (401), the route's `{team_id}` resolved against the caller's membership (404 for non-members, byte-identical to a nonexistent id, so probing reveals nothing), and permission checks via `member.require(Action::Update, "Project")` against the compiled role set (403). Routers provide the needed request extensions with `anubis::guard::layer(pool, roles)`. Scaffolded models resolve their parent chain to the owning team and ride the same primitives.

Selectable associations are scoped through generated `valid_*` methods on the model (for example `valid_leads` returning `team.memberships().current_and_invited()`). These methods populate select fields and enforce the tenancy boundary on write, so a form can never smuggle in another tenant's record.

## Roles and permissions

Roles are declared once, in `config/roles.yml`, with role inheritance (`admin` includes `editor` and `billing`) and per-model action grants (`read`, `create`, `update`, `destroy`, or `manage` as shorthand for all four), modeled on `bullet_train-roles`. The starter ships the baseline vocabulary: `default`, `editor`, `billing`, and `admin`.

One definition drives both sides of the stack:

1. The backend embeds the file at compile time (`include_str!`) and resolves it at boot through `anubis::roles::RoleSet`, which rejects unknown includes, inheritance cycles, and unknown actions before the server takes traffic. Authorization asks `RoleSet::can(held_roles, action, model)`.
2. `anubis roles generate-ts` emits the TypeScript permissions module (`roles.generated.ts`) the SPA uses to hide or disable controls the current member cannot use. Output is deterministic, and CI regenerates it and fails on drift.

`anubis roles check` validates the file standalone.

## Billing

Subscriptions attach to the Organization, not the User and not the Team. Plan limits can meter per-organization, per-team, or per-seat.

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
- **Invitation**: created when someone is added to a Team or Organization by email. Claimable by new or existing users; discarded once claimed.
- **Role**: declared in `roles.yml`, granted through memberships at either level.

At signup, every user gets a personal Organization containing a default Team, so solo use requires zero tenancy ceremony. The UI reveals organization complexity only when the user opts into it.

## Ownership chain

Every scaffolded model declares its parent chain back to a Team, exactly like Bullet Train:

```
anubis scaffold model Goal Project,Team description:text_field
```

The chain drives everything: authorization scoping, nested routes, breadcrumbs, and the parent's show-view table. Tenant isolation is enforced by walking the chain, never by trusting a client-supplied id.

Selectable associations are scoped through generated `valid_*` methods on the model (for example `valid_leads` returning `team.memberships().current_and_invited()`). These methods populate select fields and enforce the tenancy boundary on write, so a form can never smuggle in another tenant's record.

## Roles and permissions

Roles are declared once, in `config/roles.yml`, with role inheritance (`admin` includes `editor` and `billing`) and per-resource grants, modeled on `bullet_train-roles`.

The `anubis` CLI compiles that single file into two artifacts:

1. A Rust permissions module the backend uses to authorize every web and API request.
2. A generated TypeScript permissions module the SPA uses to hide or disable controls the current member cannot use.

One definition, enforced on the backend, reflected in the UI. Both artifacts are regenerated whenever `roles.yml` changes, and drift is a compile error.

## Billing

Subscriptions attach to the Organization, not the User and not the Team. Plan limits can meter per-organization, per-team, or per-seat.

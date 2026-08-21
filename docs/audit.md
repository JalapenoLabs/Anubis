# Audit log

The audit log answers one question: who did what, and when. Bullet Train shipped it as a Pro module; here it is framework plumbing, on the same terms outgoing webhooks are, and it is on from the first request a stamped application serves.

Two properties are the whole design. **A recorded event commits with the write that caused it**, because `anubis::audit::record` writes through the caller's own connection. And **the table is append-only**: nothing in the framework updates or deletes a row, and no endpoint offers either. A log anybody can rewrite proves nothing, so there is no update surface to gate, no delete surface to audit, and no `updated_at` column to wonder about.

## What a row says

| Column | What it holds |
|---|---|
| `team_id`, `organization_id` | Where the act happened. At most one is set |
| `user_id`, `actor_name` | Who acted, and how they read at the time |
| `action` | The verb: a lifecycle action, or a framework verb |
| `subject_type`, `subject_id`, `subject_label` | What it happened to, and how that read at the time |
| `changes` | The fields that moved, as `{"field": {"old": ..., "new": ...}}` |
| `request_id` | The `x-request-id` of the request that did it |
| `created_at` | When it was recorded |

**Names are copied, not joined.** A log that renders "(deleted user)" where a name belongs has lost the answer it exists to give, so `actor_name` and `subject_label` hold how both read at the moment of the act. A deleted account nulls `user_id` and leaves the name standing; a destroyed record leaves its label behind.

**At most one tenant column is set, and both may be empty.** A team-level act names its team. An organization-level act (renaming the organization, removing an organization member, dissolving a team) names its organization. An account-level act, such as a password change, names neither, because it happens outside every tenant the account belongs to. Dissolving an organization also names neither: the audit rows cascade from the organization they name, so naming it would delete the record of its own deletion, and the act lands in the actor's own log instead, which is the one place that outlives the organization.

## Recording

`anubis::audit::record` is the whole producer surface. A handler picks up the request's ambient facts with the `Context` extractor, attributes them to whoever acted, and describes the act with an `Event`:

```rust
async fn rename_team(
    member: TeamMember,
    context: audit::Context,
    Json(body): Json<NameBody>,
) -> Result<impl IntoResponse, ApiError> {
    // ... perform the rename inside a transaction ...
    audit::record(
        transaction,
        &context.by(&member.user),
        &audit::Event::new(audit::TEAM_RENAMED, "Team")
            .team(member.team.id)
            .subject(member.team.id)
            .label(&team.name)
            .changes(Changes::new().field("name", old_name, team.name.clone())),
    )
    .await?;
}
```

`Context` never fails to extract. Outside a served request there is no request id, and an event without one is still the truth about what happened; `Context::system()` is the same thing for a job or a sweep, and records a null actor. `Context::by_application` attributes an act to a platform application's token, which belongs to its team rather than to any member, so the actor is the application's name and no user id: naming a person who was not there would be worse than naming nobody.

`record` returns a query error rather than an error type of its own, for the same reason `webhooks::emit` does. The caller is already inside a Diesel transaction, and the failure should join the rollback of the write it belongs to.

## Actions

A scaffolded model records the bare lifecycle action and names itself in `subject_type`:

```
created      CreativeConcept
updated      CreativeConcept
destroyed    CreativeConcept
```

The framework's own surfaces record a dotted verb, and every one is a constant in `anubis::audit`:

| Constant | Action | Recorded when |
|---|---|---|
| `ORGANIZATION_CREATED` | `organization.created` | An organization is created, at signup or later |
| `ORGANIZATION_RENAMED` | `organization.renamed` | An organization is renamed |
| `ORGANIZATION_DESTROYED` | `organization.destroyed` | An organization is deleted |
| `TEAM_CREATED` | `team.created` | A team is created inside its organization |
| `TEAM_RENAMED` | `team.renamed` | A team is renamed |
| `TEAM_DESTROYED` | `team.destroyed` | A team is dissolved |
| `INVITATION_CREATED` | `invitation.created` | An invitation is sent |
| `INVITATION_CLAIMED` | `invitation.claimed` | An invitation is accepted |
| `INVITATION_REVOKED` | `invitation.revoked` | A pending invitation is taken back |
| `MEMBER_ADDED` | `member.added` | Somebody joins by claiming an invitation |
| `MEMBER_ROLE_CHANGED` | `member.role_changed` | A member's roles are replaced |
| `MEMBER_REMOVED` | `member.removed` | An administrator removes somebody else |
| `MEMBER_LEFT` | `member.left` | Somebody leaves of their own accord |
| `PASSWORD_CHANGED` | `password.changed` | An account rotates its password |
| `MFA_ENROLLED` | `mfa.enrolled` | An account confirms a TOTP enrollment |
| `MFA_DISABLED` | `mfa.disabled` | An account turns its second factor off |
| `PASSKEY_ADDED` | `passkey.added` | An account registers a passkey |
| `PASSKEY_REMOVED` | `passkey.removed` | An account removes a passkey |
| `SESSION_REVOKED` | `session.revoked` | An account signs one of its sessions out |
| `ACCOUNT_DELETED` | `account.deleted` | An account is deleted |

The dot is what tells the two vocabularies apart at a glance, and it is why a scaffolded model's `created` can never collide with a framework verb.

## Secrets

`Changes` never carries one, and not because anything strips it afterwards.

A scaffolded model's change set is computed by `Changes::between` from the serialized record, which is the same shape the REST API answers with and so holds nothing secret to begin with. This is also why no generated model needs per-field audit code: the diff is taken from the record itself, so a column `anubis scaffold field` adds is audited the moment it exists. The timestamps the database maintains are left out, since they move on every update and answer a question `created_at` already answers.

The framework's own credential events record an empty change set. That a password changed is the whole of what an auditor needs; the password is not, and neither is the TOTP seed, the recovery codes, or the passkey. **The rule is that a secret is never serialized into a change set, not that it is removed from one.**

## Scaffolded models

Recording rides the shared create, update, and destroy functions the account handlers and the `/api/v1` handlers both call, which are the same seams webhooks emit from. So a record created through the browser and one created through a bearer token produce the same audit row, with a different actor, and a generated model carries one call per write instead of a scheme of its own:

```rust
let creative_concept = CreativeConceptView::one(connection, record).await?;
anubis::webhooks::emit(connection, team_id, CREATED_EVENT, &creative_concept).await?;
anubis::audit::record(
    connection,
    context,
    &anubis::audit::Event::created(MODEL, record_id)
        .team(team_id)
        .label(&label),
)
.await?;
```

The label is the record's `name`, which every scaffolded model has. On an update, the change set is `Changes::between(&before, &after)` over the Diesel record, so it names exactly the columns that moved. An update that only reconciled an association records an `updated` event with an empty change set, which is the same thing the webhook comment beside it says: an association reconciled above is a change the columns cannot see.

## Reading

Two routes, both read-only, mounted under `/account`:

| Route | Guard | What it lists |
|---|---|---|
| `GET /account/teams/{team_id}/audit-events` | team admin | Everything recorded in that team |
| `GET /account/audit-events` | signed in | The caller's own account events |

The team listing takes the admin role for the same reason the Developers section does: it shows every member's activity, which is an administrative view of the team rather than an editorial one. A member without it gets `403`, a non-member gets `404` (the guard refuses before the role is read, so the team's existence is not revealed), and nobody at all gets `401`.

It pages on the standard `page`, `limit`, and `sort` parameters, newest first, and narrows on two filters:

```
GET /account/teams/{team_id}/audit-events?subject_type=CreativeConcept
GET /account/teams/{team_id}/audit-events?actor_id={user_id}&page=2
```

Both are exact matches rather than searches, because an audit log is read by following a thread ("what did this person do", "what happened to invoices") and a substring match would answer a different question less precisely.

The account listing is what makes the credential events readable. They belong to a person rather than a tenant, so the team listing cannot show them and this one can.

Neither route is on `/api/v1`. The log is an operator's view of one team, not part of the versioned public contract, so it stays where the account screens are.

## The screen

`/teams/:teamId/audit-log`, reachable from the team switcher's Manage section. One table: who, what, to what, when, and the change set. Time reads as "3 hours ago" with the exact moment in its tooltip, because a log is read by scanning; the change set is a count that unfolds into the fields that moved, rendered as JSON, because the honest rendering of a value the screen knows nothing about is the value.

The screen is gated on the compiled affordances the same way the endpoint is gated on the role, and it mirrors rather than replaces the server's answer: a member without the admin role sees a sentence instead of a table that would have come back `403`.

## Roadmap

Deliberately out of scope for now, and each is its own decision:

- **Retention.** Rows accumulate forever today. Pruning belongs with the recurring-schedule work in [jobs.md](jobs.md#roadmap), and the policy (how long, per team or per plan, and whether an export precedes it) is a product decision rather than a framework default.
- **Export.** No CSV, no JSON download, no streaming endpoint. The listing is paginated JSON, which is enough to write one against.
- **An organization roll-up screen.** Organization-level acts are recorded from day zero and are readable through the database, but no endpoint or screen serves them yet. The rows exist so that the screen, when it lands, has a history to show rather than starting from that day.
- **Diffing associations.** A `super_select` change reads as an `updated` event with an empty change set, because the diff is taken from the record's own columns. Naming the association that moved needs the reconciliation step to report what it did.
- **Join models and the far side of a has-many-through.** Neither records, for the same reason neither emits a webhook: a join row is infrastructure, and the act worth auditing is the update to the model that owns the association.

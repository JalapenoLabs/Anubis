# Notifications

A notification is one row addressed to one person, and a bell in the navbar is what they see of it. The framework owns the table, the emission call, the endpoints, and the React component; an application writes its own notices with one call and renders nothing of its own.

Notifications and [webhooks](webhooks.md) are the same idea pointed at two audiences. A webhook tells another system that something happened; a notification tells a person. They share a transactional contract, because both are worthless if they can disagree with the database: `anubis::notifications::notify` writes through the caller's own connection, exactly as `anubis::webhooks::emit` does, so a notice commits with the write that caused it.

## The row

| Column | What it holds |
|---|---|
| `user_id` | The recipient. Deleting the account takes the inbox with it |
| `team_id` | The team the notice is about, when it is about one; null otherwise |
| `kind` | The machine-readable type, `<subject>.<event>` |
| `title` | The one line the bell shows |
| `body` | The detail under it, or null |
| `href` | Where the entry navigates, as an application path, or null |
| `read_at` | Null until the recipient reads it; this is what the badge counts |
| `created_at` | When the thing it names happened |

**The text is stored, not recomputed.** A notification says what was true when it was written, and the record it talks about may since have been renamed, reassigned, or deleted. Rendering it from the current row would make yesterday's news read as today's, and would break outright when the record is gone.

**A notification is addressed, not owned.** It chains to a person rather than to a team, which is why it takes no `roles.yml` entry and no ownership guard: the recipient is the authorization rule. That is also why the API surface is session-only. A platform application's bearer token acts as its team, and a team has no inbox.

Two indexes carry the two reads: `(user_id, created_at DESC)` for the page, and a partial index on unread rows for the badge, which is asked for on every page load.

## Emitting

```rust
use anubis::notifications::{self, NewNotification};

notifications::notify(
    connection,
    NewNotification {
        user_id: lead_id,
        team_id: Some(team.id),
        kind: "project.assigned",
        title: &format!("{} assigned you {}", actor.email, project.name),
        body: None,
        href: Some(&format!("/projects/{}", project.id)),
    },
)
.await?;
```

The insert rides `connection`, so calling this inside the transaction that wrote a record ties the two together: a rolled-back write notifies nobody, and a committed write never loses its notice. It returns a query error rather than an error type of its own, for the same reason `emit` does: the caller is already inside a Diesel transaction, and the failure belongs in that rollback.

Notifying several people is a loop, and two helpers name the usual audiences: `notifications::team_admins(connection, team_id)` and `notifications::organization_admins(connection, organization_id)`, both returning the users behind claimed memberships that hold the `admin` role.

**The scaffolder emits nothing.** `anubis scaffold model` writes the `webhooks::emit` calls, because an event type is mechanical: every model has `created`, `updated`, and `destroyed`. Which of a model's changes is worth interrupting a person about, and which person, is a product decision. This is the same line the framework draws for [billing limits](billing.md#limits), and for the same reason: a generated guess would be wrong more often than it was right, and a wrong notification is spam.

## Kinds

`kind` is a machine-readable `<subject>.<event>` type in snake case. The framework's own are constants:

| Constant | Kind | Sent to |
|---|---|---|
| `INVITATION_RECEIVED` | `invitation.received` | The invitee, when the invited address already has an account |
| `INVITATION_CLAIMED` | `invitation.claimed` | The admins of the team or organization that was joined |
| `MEMBERSHIP_ROLES_CHANGED` | `membership.roles_changed` | The member whose roles changed |
| `WEBHOOK_DELIVERY_FAILED` | `webhook.delivery_failed` | The team's admins, when a delivery runs out of attempts |

An invitation to an address with no account writes nothing: there is no inbox yet, and the email carries the invitation either way. The claim token stays in that email and never reaches a notification, because a token is a credential and the row would be holding it in the clear, so the notice says where to accept rather than linking to it.

The webhook notice is sent **once per endpoint per day**, not once per lost delivery: a receiver that is down fails everything sent to it, and an inbox holding one notice per lost event says less than a single one does. The endpoint is left active. Pausing a team's subscription on the framework's initiative would lose events nobody asked it to lose, so the team is told and the decision stays theirs.

### Translating an inbox

The stored `title` and `body` are English, written by whoever wrote the notice. `kind` is what a translated application keys off: look the copy up by kind, render that, and ignore the stored text. This is the rule that keeps enum values out of locale files generally, applied here. The framework ships no per-kind locale keys, because the interesting half of a notice is the interpolated name it carries.

## Realtime

After the transaction commits, the recipient's browsers are told to refetch on the channel `user:{user_id}:notifications`.

Publishing is not transactional and a notification is, which is the whole design constraint. A publish inside the transaction would tell a browser to refetch a notice that may yet roll back; a publish just before the commit races the refetch it asks for. So `notify` enqueues a framework job, `anubis::notifications::PingRecipient`, on the caller's connection beside the row, and a `Pinger` registered on a worker publishes it once the commit has happened. [Realtime channels](realtime.md#publishing) states the rule this follows: publish after the commit, or from a background job the commit enqueued.

```rust
let pinger = anubis::notifications::Pinger::new(channels.clone());
let worker = anubis::jobs::Worker::builder(pool.clone())
    .register(move |job: anubis::notifications::PingRecipient| {
        let pinger = pinger.clone();
        async move { pinger.ping(job).await }
    })
    .build();
```

The ping runs on its own `notifications` queue and carries an empty payload. **Nothing durable rides the socket**: the frame says "look again", and the client reads `/account/notifications`. `anubis new` stamps the registration above into the application's `main.rs`. An application that removes it still delivers every notification, and its bells update on their next fetch instead of at once, but the ping rows then sit in the queue unclaimed, because a worker draws only from the queues it has handlers for.

## The API

Three session-authenticated endpoints, mounted under `/account` beside the application's own account routes:

| Route | Answers |
|---|---|
| `GET /account/notifications?page=&limit=` | One page, unread first and newest first, with `pagination` and the whole inbox's `unread` count |
| `POST /account/notifications/{notification_id}/read` | The notification, now read |
| `POST /account/notifications/read-all` | `{ "marked": n }` |

```rust
.nest("/account", anubis::notifications::router(pool.clone()))
```

Ordering is two keys, not one: an inbox is read for what still needs attention and only then for what happened, so marking an entry read moves it down the list rather than out of it. The `unread` count describes the inbox rather than the page, because it is what the badge shows.

A notification id that belongs to somebody else answers `404`, exactly as one that does not exist. Marking an already-read notification read again is harmless and keeps the first reading's timestamp.

## The bell

`NotificationBell` ships in `@jalapenolabs/anubis`, and the stamped `AppShell` mounts it on the right of the navbar:

```tsx
<NotificationBell
  onNavigate={(href) => navigate(href)}
  labels={{
    bell: t('notifications.bell'),
    heading: t('notifications.heading'),
    empty: t('notifications.empty'),
    markAllRead: t('notifications.markAllRead'),
  }}
/>
```

It is one component and one hook. `useNotifications` fetches the first page with SWR, subscribes to the recipient's channel under a `RealtimeProvider`, refetches on every ping, and exposes `markRead` and `markAllRead`. Pressing an entry marks it read and follows its `href`, which is what makes the badge mean "waiting for you" rather than "happened recently".

The package imports no i18next and no router, so the bell takes its four strings as `labels` (English defaults) and its navigation as `onNavigate`, the same shape the field components use. Without a `RealtimeProvider` above it the bell still works; it just waits for its next fetch rather than being pushed.

Because the framework stores rendered notices, it also has to name the screens its own notices point at: `/teams/{id}/settings` and `/teams/{id}/developers`. Those paths are a contract with the stamped application's `UrlTree`, the same one `anubis::billing` relies on when it hands Stripe a return URL. An application that renames the route updates both.

## What this deliberately leaves out

- **Email digests.** A daily summary is a scheduled job, a preference, and a template, and none of the three exists yet.
- **Per-type preferences.** Every recipient gets every notice addressed to them. Muting a kind is a settings screen and a table, and the framework has neither opinion nor evidence about which kinds people mute.
- **Reply-by-email.** Inbound mail parsing is a separate subsystem from [incoming webhooks](webhooks.md#incoming-receiving-a-third-partys-events), and nothing in the framework receives mail today.
- **Grouping and digests inside the bell.** Ten notices about ten things are ten rows.

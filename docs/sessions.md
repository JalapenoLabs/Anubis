# Sessions

A browser stays signed in with a session: an opaque token in a cookie, checked on every request. Sessions live in Postgres by default, beside everything else, and an application that already runs Redis can keep them there instead by setting `SESSION_STORE=redis`. Nothing else changes between the two: the cookie, the expiry, every way to sign out, the session list, and the audit log behave identically.

## What a session is

- **The token** is 256 random bits, base64url, in the `anubis_session` cookie: `HttpOnly`, `SameSite=Lax`, `Path=/`, and `Secure` in production.
- **The store keeps only the token's SHA-256**, in either store, so a leaked database or a stolen Redis snapshot holds nothing a browser could present.
- **A session lives 30 days** (`anubis::auth::SESSION_TTL_DAYS`), fixed from when it was created. Sliding renewal is not built.
- **Every request reads the account from Postgres**, whichever store holds the session, together with its session generation (below). A temporary password, a changed role, a sign-out-everywhere, or a deleted account takes effect on the very next request, because no account is ever cached beside the session.

`CurrentUser` is the extractor that turns the cookie into the account; see [api.md](api.md#authentication).

## Signing out everywhere is decided in Postgres

Every account has a session generation (`users.session_generation`), and every session is stamped with the generation it was created under (`sessions.generation`, or a field of the session in Redis). A session signs nobody in unless its stamp equals the account's current generation, which every lookup reads beside the account.

Signing out everywhere advances the generation, as the last statement of the transaction that changes the credentials: a password reset, an operator's temporary password, and a password change, which re-stamps the browser making the change so it stays signed in. The sessions therefore end exactly when that transaction commits, in either store, and deleting them afterwards is only housekeeping. A store that loses those deletes cannot bring an ended session back, whether it evicted the keys that would have found them, came back from a snapshot taken before the commit, failed over to a replica that never saw the deletes, or was the other store before a change of `SESSION_STORE`.

The stamp must equal the generation, not merely reach it. In Redis the password change's re-stamp cannot roll back, so if the commit then fails the kept session carries a stamp one ahead of the account, and equality signs that browser out rather than letting it outlive the next sign-out-everywhere. The price is a few milliseconds, between the re-stamp and the commit, in which another request from that same browser is answered `401`.

Signing one session out, from the list or with `POST /auth/logout`, deletes that session from its store and moves no generation. It is as durable as the store's delete: a Redis that loses the write, to a restart from an older snapshot or a failover, brings that one session back until it ends or the account signs out everywhere, which ends it for good.

A sign-in racing a sign-out-everywhere loses: a session stamped with the generation it read before the other transaction committed is ended by it.

## Choosing the store

| `SESSION_STORE` | Sessions live in | Needs |
|---|---|---|
| unset, or `postgres` | the `sessions` table | nothing |
| `redis` | the Redis `REDIS_URL` names | `REDIS_URL`, and a Redis of its own at version 7.0 or later, that never evicts keys, and persists with an append-only file (below) |

Postgres is the right default: one datastore fewer to run, and sessions are backed up with everything else. Redis suits an application that already runs one for other work and wants its sessions there too, keeping the session table's writes, a row per sign-in and a delete per sign-out, out of Postgres and out of its backups. It does not take the per-request check off Postgres: every authenticated request still reads the account, and its generation, by primary key, now after a Redis read rather than inside one query.

The store is its own setting rather than a consequence of `REDIS_URL`, because the two are separate decisions: an application fanning [realtime](realtime.md) channels out across instances need not want its sessions in Redis. The reverse does hold: both read the one `REDIS_URL`, so choosing Redis for sessions puts realtime fanout there too.

Configuration refuses what cannot work, at load, beside every other configuration mistake: `SESSION_STORE=redis` without `REDIS_URL`, and any value other than `postgres` or `redis`, because a typo that quietly fell back to Postgres would be a deployment nobody could explain. `REDIS_URL` itself is parsed the way a Redis client parses it, and only `redis://` and `rediss://` are accepted.

### Switching stores

Switching signs everyone out of the new store once, since sessions are not moved between stores. The old store's sessions are not deleted either: they wait, and switching back within their 30 days finds the ones nobody ended. A sign-out-everywhere made under either store ends them too, because it is decided in Postgres, so a password reset is never undone by a switch. To retire the old store's sessions outright, clear it after the switch: `DELETE FROM sessions` when leaving Postgres, or deleting the `anubis:sessions:*` keys when leaving Redis.

## The same behaviour in both

| Act | What happens |
|---|---|
| Signing in, by any path | A session is created and `session.created` is recorded with the method; see [audit.md](audit.md#sign-ins) |
| A session reaching its end | It signs nobody in and leaves the list. Postgres sweeps a user's ended rows at their next sign-in; Redis drops the key at its end |
| Signing out (`POST /auth/logout`) | That session ends. Signing out twice is fine |
| Signing one session out from the list (`DELETE /auth/sessions/{id}`) | That session ends and `session.revoked` is recorded against it. Another account's session id, and a session that already ended, answer `404`, exactly as an id that never existed does |
| Changing the password | Every other session ends; the browser that changed it stays signed in |
| Resetting the password, or an operator setting a temporary one | Every session ends, the current one included |
| Deleting the account | Every session ends with it |
| Listing sessions (`GET /auth/sessions`) | Live sessions only, oldest first, the requesting one marked `current` |

The security page in the starter is that list, so it reads the same against either store.

## How the store reaches every route

`anubis::server::harden`, which every served application passes through, puts the configured store on every request. That is the whole of opting in: no router is built differently, and a route an application writes reads the same store the auth routes write to.

A Redis store connects on its first command, so `anubis::server::serve` asks it whether it can hold sessions before binding. It refuses to start, naming `SESSION_STORE` and `REDIS_URL`, when Redis does not answer, reports a version older than 7.0, or has a memory limit with a policy that evicts keys; it logs a warning when Redis persists less than an append-only file, or will not say. A deployment that cannot check a session cannot serve anyone who is signed in, so finding out at boot beats every sign-in failing. Once it passes, the boot logs the store it uses as `session.store`.

An application that drives `harden` and `serve_with_shutdown` itself, to stop a job worker on the same signal, gets the store on every request but not the boot check: its Redis store connects on the first request that needs it, and an unreachable one fails that request rather than the boot.

A request that never went through `harden`, such as one a test sends straight to a bare router, reads sessions from Postgres, and logs that at `debug`. That is what keeps an application or a suite that never names a store exactly as it was. The authentication routes, which are the ones that write sessions, hold that fallback to the configuration: there, a request carrying no store while `SESSION_STORE` is `redis`, or carrying a store the configuration did not choose, is refused with a `500` and an error log naming both, rather than answered by the wrong store.

## Writes that span two stores

Postgres sessions commit or roll back with the transaction around them. Redis cannot join a Postgres transaction, so every write is ordered to keep failures safe:

- **Creating a session** is the last step of the transaction that records the sign-in, and whatever the sign-in spends, a recovery code, an emailed code, an operator's invitation, is spent in that same transaction. A Redis failure rolls all of it back, so a code is never spent on a sign-in that did not happen, and the cookie is only issued after the commit, so no browser ever holds a session the audit log does not know about. A commit that fails after the write leaves a session no browser holds, which ends with its expiry.
- **Signing out everywhere** advances the generation as the last statement of its transaction and sweeps the store after the commit, so nothing waits on Redis while holding the account's row, and a Redis that is down does not hold up a password reset. A password reset's link is spent inside that same transaction, so a reset that fails leaves the link usable.

## Redis

Two kinds of key, under the framework's own `anubis:sessions:` prefix:

| Key | Holds | Expires |
|---|---|---|
| `anubis:sessions:token:<sha256>` | the session as JSON: its id, the user's id, its generation stamp, when it was created, when it ends | when the session ends |
| `anubis:sessions:user:<user id>` | a sorted set of that user's token hashes, scored by when each ends | when the user's last session ends |

The token key answers the per-request question in one read; the user's index answers the account's own questions, listing its sessions and finding them to sweep. Creating a session writes both keys in one `MULTI` block. Signing one out deletes its key and then its index entry, in two steps; an entry left behind between them names a key that is gone, which every read skips. Ended entries are pruned from the index at the user's next sign-in and skipped by every read.

Every read checks a session's end on the application's clock as well as relying on Redis to drop the key, the same check Postgres makes, and checks its stamp against the generation it reads from Postgres.

The connection is one self-reconnecting, multiplexed connection per process, with a one-second connect timeout, a one-second response timeout, and two reconnection attempts. A Redis that stops answering fails a request promptly rather than holding it until the request timeout.

### What the Redis must be

- **One deployment's own.** The keys carry no deployment name, so two deployments sharing a Redis, staging and production say, share sessions by user id once their databases share ids, as a staging database restored from production does. Give each deployment its own Redis; a separate database number keeps the session keys apart but not realtime fanout, whose channels Redis does not divide by database.
- **Version 7.0 or later**, because the index's expiry is set with `EXPIREAT ... NX` and `GT`.
- **Never evicting.** With a memory limit (`maxmemory`), any policy but `noeviction` lets Redis drop session keys, and the index goes first, since only signing in and the account's own pages touch it. A session whose index entry was dropped still signs in but is missing from the session list, where nobody can see it or sign it out alone; signing out everywhere still ends it. The boot refuses such a Redis, and a full Redis under `noeviction` refuses new sign-ins instead.
- **Persisting with an append-only file**, below.
- **One server, or a primary behind one URL.** Redis Cluster is not supported: a session's two keys live in different hash slots, and `MULTI` needs one.

### Persistence

A Redis holding sessions has to keep them across a restart.

| Redis persists with | Effect of a restart | `anubis doctor` |
|---|---|---|
| An append-only file (`appendonly yes`) | At most about a second of writes is lost, with the default `appendfsync everysec` | ok |
| RDB snapshots only (the stock `save` schedule) | The sign-ins since the last snapshot are lost, and sessions signed out one at a time since then come back | warns |
| Neither | Everyone is signed out | warns |

Whatever a restart loses, a sign-out-everywhere stays done, because it lives in Postgres. Run Redis with `appendonly yes`.

`anubis doctor` reads `SESSION_STORE` and `REDIS_URL` with the configuration's own parser, so it fails whatever the boot would refuse, and for Redis asks exactly what the boot asks: its version, `maxmemory` and `maxmemory-policy`, and `appendonly` and `save`. A version or eviction policy the boot refuses fails the check; persistence short of an append-only file warns. Managed Redis often refuses `CONFIG`; the eviction policy is then reported as unknown, and persistence falls back to `INFO persistence`, which can confirm an append-only file but not the snapshot schedule, so anything short of the file warns that it could not tell.

### When Redis goes away

Losing the session Redis is losing sign-in. Until it returns, every authenticated request answers `500`, never `401`, so no browser is told it was signed out; sign-in fails the same way, without spending the code or link it was made with. A password reset, which needs no session, still completes, since it is decided in Postgres; only its sweep is missed, and the ended sessions expire on their own. Nothing is lost as long as Redis persists.

The rest of the application keeps its Postgres connections while it waits. The per-request session check, logging out, the session list, and signing one session out all read Redis before checking a connection out, so a stalled Redis holds none, and a cookie naming no session costs none at all. Signing in holds one across its Redis write, and a password change across its re-stamp, because each sits inside its transaction; each gives up within the response timeout. Routes that need no session keep working, and so do the probes.

### Running it locally

The starter's compose file keeps Redis behind a profile, with an append-only file on a volume:

```sh
docker compose -f starter/compose.yaml --profile redis up -d --wait
SESSION_STORE=redis REDIS_URL=redis://localhost:63790 cargo run -p anubis-starter
```

`.env.example` carries both variables, commented out.

## Testing

`anubis/tests/session_store_flow.rs` is one narrative run twice, once per store, through `anubis::server::harden` and the real setting. It walks every act in the table above and reads the store directly at each step, asserting the session is a row in Postgres, or a key in Redis with no row behind it. Postgres is the default, so a Redis store that was never wired in would otherwise pass as Postgres.

The narrative also defeats the sweep on purpose: before a password reset it evicts the user's Redis index, so the sweep finds nothing, and after the reset it puts the session back as a restored snapshot would, row or key and index entry alike. The session must still sign nobody in, stay off the list, and answer `404` to signing it out, which only the generation can guarantee. A second test serves one database through both stores and resets the password under each, proving a sign-out-everywhere ends the other store's sessions, the ones a switch back would find.

`anubis/tests/session_store_outage_flow.rs` points a Redis store at a port nothing listens on, beside a healthy deployment over the same database, and proves that a failed sign-in spends neither a recovery code nor an emailed code, and that a password reset completes, spends its link once, and ends the sessions anyway.

The Postgres run and the outage suite need `DATABASE_URL`; the Redis runs also need `REDIS_URL`. Each logs a skip without what it needs, and CI provides both; see [ci.md](ci.md). The narratives isolate themselves in a shared Redis by creating new accounts, and never flush or reconfigure it.

## Roadmap

- Readiness covering the session store, so a load balancer drains an instance whose session Redis is unreachable.
- Sliding renewal, extending an active session instead of ending it 30 days after sign-in.

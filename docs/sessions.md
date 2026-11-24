# Sessions

A browser stays signed in with a session: an opaque token in a cookie, checked on every request. Sessions live in Postgres by default, beside everything else, and an application that already runs Redis can keep them there instead by setting `SESSION_STORE=redis`. Nothing else changes between the two: the cookie, the expiry, every way to sign out, the session list, and the audit log behave identically.

## What a session is

- **The token** is 256 random bits, base64url, in the `anubis_session` cookie: `HttpOnly`, `SameSite=Lax`, `Path=/`, and `Secure` in production.
- **The store keeps only the token's SHA-256**, in either store, so a leaked database or a stolen Redis snapshot holds nothing a browser could present.
- **A session lives 30 days** (`anubis::auth::SESSION_TTL_DAYS`), fixed from when it was created. Sliding renewal is not built.
- **Every request reads the account from Postgres**, whichever store holds the session. A temporary password, a changed role, or a deleted account takes effect on the very next request, because no account is ever cached beside the session.

`CurrentUser` is the extractor that turns the cookie into the account; see [api.md](api.md#authentication).

## Choosing the store

| `SESSION_STORE` | Sessions live in | Needs |
|---|---|---|
| unset, or `postgres` | the `sessions` table | nothing |
| `redis` | the Redis `REDIS_URL` names | `REDIS_URL`, Redis 7.0 or later, and persistence (below) |

Postgres is the right default: one datastore fewer to run, and sessions are backed up with everything else. Redis suits an application that already runs one and wants the session check, which every authenticated request makes, off Postgres.

The store is its own setting rather than a consequence of `REDIS_URL`, because the two are separate decisions: an application fanning [realtime](realtime.md) channels out across instances need not want its sessions in Redis. The reverse does hold: both read the one `REDIS_URL`, so choosing Redis for sessions puts realtime fanout there too.

Configuration refuses what cannot work, at load, beside every other configuration mistake: `SESSION_STORE=redis` without `REDIS_URL`, and any value other than `postgres` or `redis`, because a typo that quietly fell back to Postgres would be a deployment nobody could explain. `REDIS_URL` itself is parsed the way a Redis client parses it, not only checked for its scheme.

Switching stores signs everyone out once, since the sessions in the old store are not moved. Plan it as a deploy people notice.

## The same behaviour in both

| Act | What happens |
|---|---|
| Signing in, by any path | A session is created and `session.created` is recorded with the method; see [audit.md](audit.md#sign-ins) |
| A session reaching its end | It signs nobody in and leaves the list. Postgres sweeps a user's ended rows at their next sign-in; Redis drops the key at its end |
| Signing out (`POST /auth/logout`) | That session ends. Signing out twice is fine |
| Signing one session out from the list (`DELETE /auth/sessions/{id}`) | That session ends and `session.revoked` is recorded against it. Another account's session id answers `404`, exactly as an id that never existed does |
| Changing the password | Every other session ends; the browser that changed it stays signed in |
| Resetting the password, or an operator setting a temporary one | Every session ends, the current one included |
| Deleting the account | Every session ends with it |
| Listing sessions (`GET /auth/sessions`) | Live sessions only, oldest first, the requesting one marked `current` |

The security page in the starter is that list, so it reads the same against either store.

## How the store reaches every route

`anubis::server::harden`, which every served application passes through, puts the configured store on every request. That is the whole of opting in: no router is built differently, and a route an application writes reads the same store the auth routes write to.

A Redis store connects on its first command, so `anubis::server::serve` asks it for its version before binding and refuses to start, naming `SESSION_STORE` and `REDIS_URL`, when Redis does not answer or reports a version older than 7.0. A deployment that cannot check a session cannot serve anyone who is signed in, so finding out at boot beats every sign-in failing. Once it passes, the boot logs the store it uses as `session.store`.

An application that drives `harden` and `serve_with_shutdown` itself, to stop a job worker on the same signal, gets the store on every request but not the boot check: its Redis store connects on the first request that needs it, and an unreachable one fails that request rather than the boot.

A request that never went through `harden`, such as one a test sends straight to a bare router, uses Postgres. That is what keeps an application or a suite that never names a store exactly as it was.

## Writes that span two stores

Postgres sessions commit or roll back with the transaction around them. Redis cannot join a Postgres transaction, so every write is ordered to keep failures safe:

- **Creating a session** is the last step of the transaction that records the sign-in, and the cookie is issued only after the commit. A Redis failure rolls the sign-in back, so no browser ever holds a session the audit log does not know about. A commit that fails after the write leaves a session no browser holds, which ends at its expiry.
- **Every sign-out-everywhere** (password change, password reset, temporary password, account deletion) revokes inside its transaction, before the commit. A store that cannot revoke aborts the change, so a stale session never outlives the credentials it was made with; a commit that fails after the revocation leaves the person signed out with nothing else changed.

## Redis

Two kinds of key, under the framework's own `anubis:sessions:` prefix:

| Key | Holds | Expires |
|---|---|---|
| `anubis:sessions:token:<sha256>` | the session as JSON: its id, the user's id, when it was created, when it ends | when the session ends |
| `anubis:sessions:user:<user id>` | a sorted set of that user's token hashes, scored by when each ends | when the user's last session ends |

The token key answers the per-request question in one read; the user's index answers the account's own questions, listing and signing out. Writes touching both keys run as one `MULTI` block. Signing everyone out removes exactly the sessions it read from the index, so a sign-in racing it survives and stays listed and revocable, which is what the Postgres store's `DELETE` does too. Ended entries are pruned from the index at the user's next sign-in and skipped by every read.

Every read checks a session's end on the application's clock as well as relying on Redis to drop the key, the same check Postgres makes.

The connection is one self-reconnecting, multiplexed connection per process, with a one-second connect timeout, a one-second response timeout, and two reconnection attempts. A Redis that stops answering fails a request promptly rather than holding it until the request timeout.

One Redis, or a primary behind one URL, at version 7.0 or later, because the index's expiry is set with `EXPIREAT ... NX` and `GT`. The boot checks the version and `anubis doctor` reports it. Redis Cluster is not supported: a session's two keys live in different hash slots, and `MULTI` needs one.

### Persistence

A Redis holding sessions has to keep them across a restart, because one that comes back empty signs everyone out.

| Redis persists with | Effect of a restart | `anubis doctor` |
|---|---|---|
| An append-only file (`appendonly yes`) | Nothing is lost | ok |
| RDB snapshots only (the stock `save` schedule) | Sign-ins since the last snapshot are lost | ok, with a note |
| Neither | Everyone is signed out | warns |

Run Redis with `appendonly yes`. `anubis doctor` names the store in use, and for Redis reads `CONFIG GET appendonly` and `CONFIG GET save`. Managed Redis often refuses `CONFIG`; doctor then reads `INFO persistence`, which confirms an append-only file but cannot see snapshots, so anything short of the file warns that it could not tell. A Redis store that does not answer, one older than 7.0, or a `SESSION_STORE` the server would refuse, fails the check, because each one stops the boot.

### When Redis goes away

Losing the session Redis is losing sign-in. Until it returns, every authenticated request answers `500`, never `401`, so no browser is told it was signed out; sign-in fails the same way. Nothing is lost as long as Redis persists.

The rest of the application keeps its Postgres connections while it waits. The per-request session check reads Redis before it checks a connection out, so a stalled Redis holds none, and a cookie naming no session costs none at all. Only the routes that manage sessions, signing in and out and the session list, hold a connection across a Redis call, and each gives up within the response timeout. Routes that need no session keep working, and so do the probes.

### Running it locally

The starter's compose file keeps Redis behind a profile, with an append-only file on a volume:

```sh
docker compose -f starter/compose.yaml --profile redis up -d --wait
SESSION_STORE=redis REDIS_URL=redis://localhost:63790 cargo run -p anubis-starter
```

`.env.example` carries both variables, commented out.

## Testing

`anubis/tests/session_store_flow.rs` is one narrative run twice, once per store, through `anubis::server::harden` and the real setting. It walks every act in the table above and reads the store directly at each step, asserting the session is a row in Postgres, or a key in Redis with no row behind it. Postgres is the default, so a Redis store that was never wired in would otherwise pass as Postgres.

The Postgres run needs `DATABASE_URL`; the Redis run also needs `REDIS_URL`. Each logs a skip without what it needs, and CI provides both; see [ci.md](ci.md). The narrative isolates itself in a shared Redis by creating new accounts, and never flushes or reconfigures it.

## Roadmap

- Readiness covering the session store, so a load balancer drains an instance whose session Redis is unreachable.
- Sliding renewal, extending an active session instead of ending it 30 days after sign-in.

# REST API

Every Anubis application ships a versioned, documented, public REST API from day one, because scaffolding generates it alongside the web UI. This mirrors Bullet Train's "zero-effort API" goal.

## Structure

- Routes live under `/api/v1/...`, one handler file per route, directory structure matching the URL structure.
- Web (account) handlers and API handlers are separate but share the same permission checks and the same serializers.
- Request validation is the API-first source of truth: the accepted-fields definition lives in the current API version and the account handlers reuse it. Bumping the API version freezes the old definition automatically, which is what makes versioning safe.

## List endpoint conventions

Every scaffolded list endpoint (web and API) follows one shape:

- **Pagination is page/limit**, not cursor: `?page=2&limit=25`. `page` is 1-based and defaults to 1; `limit` defaults to 25 and caps at 100. Out-of-range values clamp rather than error.
- **Sorting**: `?sort=name` ascending, `?sort=-created_at` descending. Sortable fields are a per-model whitelist maintained by the scaffolder; unknown fields fall back to the default sort (`created_at` descending).
- **Filtering**: explicit per-field query params (`?status=active`), whitelisted per model by the scaffolder. No generic filter DSL.
- **Response envelope**: the plural resource key plus a `pagination` object:

```json
{
  "projects": [ ... ],
  "pagination": { "page": 2, "limit": 25, "total_items": 61, "total_pages": 3 }
}
```

The framework implements the convention once: handlers take `anubis::http::ListParams` as a query extractor beside their own filter struct, and answer with `anubis::http::Pagination`. Values that are out of range, or that do not parse at all, fall back to the convention, so a paging bug in a client degrades into a valid page instead of a 400.

## Transport

Every response, on every route, carries an `x-request-id` and the framework's security headers, and every request runs under a timeout. Cross-origin access is off until `CORS_ALLOWED_ORIGINS` names exact origins; the `/api/v1` bearer-token surface is what that exists for, since session cookies stay same-origin. [The server](server.md) covers the whole serve path, the headers, and the liveness and readiness probes.

## Versioning

`/api/v1` is stable once users build against it. A breaking change means minting `/api/v2` handlers and serializers while `/api/v1` continues to serve frozen behavior. The scaffolder always targets the newest version.

## Documentation and client generation

Handlers and serializers register with utoipa, producing an OpenAPI 3.1 document served at `/api/v1/openapi.json`, with human-readable Scalar docs at `/api/v1/docs`. `anubis openapi` exports the document for tooling.

`anubis client generate-ts` renders the document as the generated TypeScript client (`createAnubisV1`): exported wire types plus one ky function per operation, authenticated with a platform bearer token. Output is deterministic and house-style, and CI regenerates it and fails on drift, so the client can never lag the API. Types keep wire field names (snake_case) because they are the contract itself.

## Authentication

- **Browser**: Postgres-backed cookie sessions. The cookie carries an opaque 256-bit token (`HttpOnly`, `SameSite=Lax`, `Secure` in production); the database stores only the token's SHA-256, so a leaked database yields no usable sessions. Passwords hash with argon2id. Handlers require sign-in via the `CurrentUser` extractor.
- **Sign-in methods**: password; passwordless emailed 6-digit codes (10-minute life, attempt-limited, enumeration-safe); and passkeys (WebAuthn discoverable credentials, password-manager-first, cross-platform authenticators welcome).
- **Second factor**: TOTP (authenticator apps) with QR enrollment and single-use recovery codes. When confirmed, password and email-code login answer a 5-minute challenge instead of a session; a passkey is multi-factor by construction and bypasses the challenge.
- **OAuth**: OpenID Connect providers, one line of setup each. See [OAuth sign-in](#oauth-sign-in) below.
- **Secrets at rest**: recovery codes hash like every other token. A TOTP seed must be read back to compute the expected code, so it is encrypted instead, with AES-256-GCM under the application key in `ANUBIS_SECRET_KEY` (base64 for exactly 32 bytes, required in production; development and test fall back to a public built-in key and warn at startup). Stored values are versioned and self-describing, `v1:<nonce>:<ciphertext>`, so a future scheme can be added without a migration. A seed that no longer decrypts, because the key rotated, is discarded: the account drops back to single-factor login and the user enrolls again. The same `anubis::auth::secret_box` module covers any later secret that needs recoverable storage.
- **Account management routes**: profile (names, time zone, locale), avatar upload/serve/delete, signed-in password and email change, session listing and revocation, and password-confirmed account deletion.
- **API**: per-team Platform Applications, each issuing bearer access tokens (Doorkeeper's role in Bullet Train). Tokens follow the framework discipline (256-bit, SHA-256 at rest, shown exactly once at creation or rotation) and do not expire; rotation and application deletion are the revocation paths. Management endpoints live under `/developers/teams/{team_id}/platform-applications` (create, list, delete, rotate-token), team-scoped through the `TeamMember` guard and restricted to the admin role. The `ApiCaller` extractor resolves `Authorization: Bearer` to the owning application and team, which scopes everything a v1 handler may touch. The framework ships `GET /api/v1/team` as the pattern's reference endpoint; API serializers live in the version module (`TeamV1`) and freeze with it.

Authorization is identical in both paths: the compiled `roles.yml` permissions module authorizes every request against the membership's roles and the resource's ownership chain.

## Rate limiting

Tokens are attempt-limited individually, but the endpoints that accept them would otherwise take attempts at network speed. The framework gives each client a balance on the endpoints an attacker can drive without credentials, and answers `429 Too Many Requests` with a `Retry-After` header once it is spent. The body is the standard error shape, and its message names only the wait: which budget tripped, and whether the account or address involved exists, stay invisible.

| Endpoint | Budget | Keyed by |
|---|---|---|
| `POST /auth/login`, `POST /auth/mfa/verify`, `POST /auth/email-code/verify` | 10 per minute | Client address |
| `POST /auth/register` | 10 per hour | Client address |
| `POST /auth/password-reset/request`, `POST /auth/email-code/request`, `POST /auth/verify-email/request` | 20 per hour | Client address |
| `POST /auth/password-reset/request`, `POST /auth/email-code/request` | 5 per hour | Target email address |

Each budget is a burst followed by a steady refill: ten credential attempts are available at once, then one more every six seconds. The confirm endpoints are absent on purpose, because guessing a 256-bit token is not an attack a budget improves on.

The mail endpoints carry two budgets because the two abuses differ. A client hammering the endpoint is caught per address; a campaign rotating addresses to bomb one inbox is caught per recipient, which is why that budget is the tighter of the two. The recipient is charged before the account lookup and only its SHA-256 becomes a key, so no address sits in memory in the clear and the answer never depends on whether the address is registered.

Rate limiting is not an enumeration oracle. Budgets count requests, never outcomes, so the same volume from the same client produces the same `429` whether the accounts involved exist or not. That is a property of counting volume rather than a check anyone has to remember to add.

**Addressing.** By default the client is the socket peer address, which is correct whenever the application terminates connections itself. Behind a proxy, set `TRUSTED_PROXY_HEADER=x-forwarded-for` (or whichever header that proxy appends to). The limiter then reads the **last** entry of that header, the hop the trusted proxy wrote. Everything before it was written by an upstream the application does not control, or by the client, so trusting the first entry would let any caller choose its own key and opt out of the limits. A configured header that is missing or unparsable falls back to the peer address, so a misconfigured proxy makes limits stricter, never looser. Serving the router with `into_make_service_with_connect_info::<SocketAddr>()` is what supplies the peer address; without it the framework logs a warning at the first request and admits everything, since only a composition bug can produce a request with no address.

**Turning it off.** `RATE_LIMIT_DISABLED=true` switches every budget off. It exists for development and for test suites that drive these endpoints hard from one address; deployments leave it unset. The budgets are otherwise identical in every environment, because a limit that differs between development and production is a limit nobody has tested.

**Scope and cost.** State is in-process: two instances behind a load balancer enforce two budgets, so the effective limit multiplies by the instance count. That still bounds an attack at `instances * quota` per period, which is the difference between a bounded attack and an unbounded one. A shared store is the eventual answer, not a prerequisite. Memory is bounded too: the limiter tracks at most 32,768 keys, roughly a hundred bytes each, and prunes when it reaches that, first the keys whose balance is already full and then the least-loaded ones. A client rotating addresses to flood the map evicts its own fresh keys before the keys the limiter is holding back, because those are the heaviest in the map.

## The identity screens

Every route above has a screen. The starter owns the pages; `@jalapenolabs/anubis` owns the typed client (`createAnubisApi`) and the WebAuthn browser helpers, because both are the same in every application.

| Screen | Route | Endpoints |
|---|---|---|
| Profile settings | `/settings/profile` | `PATCH /auth/profile`, `POST` and `DELETE /auth/profile/avatar` |
| Security settings | `/settings/security` | change password, change email, MFA, passkeys, sessions, account deletion |
| New address confirmation | `/change-email?token=` | `POST /auth/change-email/confirm` |
| Sign in | `/sign-in` | password, `email-code/request` and `verify`, `mfa/verify`, the passkey login ceremony |

Sign-in is one page with three methods and a shared second-factor step, rather than a route per method, because the preserved `?next=` destination has to survive every branch and a step of the same page keeps it in the URL for free.

Avatar upload has no cropper. The server center-crops to a square, caps the longest edge at 512 px, flattens transparency, and re-encodes as JPEG, so a browser cropper would only be a second opinion the stored image ignores; the picker previews the file and posts the bytes.

Recovery codes appear exactly once, on the step after a confirmed enrollment, with a copy affordance. The server stores only their hashes, so there is no second chance to show them and the screen says so.

WebAuthn needs binary where JSON has none, so the package exports the conversion both ceremonies need: `toCredentialCreationOptions` and `toCredentialRequestOptions` decode a challenge into what `navigator.credentials` accepts, `serializeRegistrationCredential` and `serializeAuthenticationCredential` encode the authenticator's answer back, and `base64UrlToArrayBuffer` and `arrayBufferToBase64Url` are the pair underneath. The serializers take `unknown` and validate, because an authenticator's answer is as much a runtime boundary as an HTTP response.

## OAuth sign-in

Two framework routes carry a whole provider, and `anubis scaffold oauth <provider>` adds the button that calls them:

| Route | Effect |
|---|---|
| `GET /auth/oauth/{provider}/start` | Redirect the browser to the provider's consent screen |
| `GET /auth/oauth/{provider}/callback` | Verify the response, issue the session, land on the destination |

Both are browser navigations, so both answer with a redirect rather than JSON. A `?next=` on `start` is preserved through the round trip and is where a successful callback lands; it is validated as a root-relative path server-side, so it cannot become an open redirect.

The flow is authorization code with PKCE. `start` discovers the provider's endpoints from its issuer (cached per process), generates the PKCE verifier and the nonce, and stores them server-side under a fresh opaque token. That token is the OAuth `state` parameter, so the provider's echo is what finds the row; only its SHA-256 is stored, and consuming it deletes it, which makes a replayed callback fail exactly like an expired one. Flows expire after 15 minutes.

`callback` exchanges the code, verifies the ID token's signature, issuer, audience, and nonce, and then resolves the account in one transaction:

1. A `(provider, subject)` pair that is already linked signs that user in, whatever their address is today.
2. A **verified** email that an account already uses links the identity to that account, so a password user starts using the button without a second account appearing.
3. Anything else creates a user with the same bootstrap registration performs (personal organization, default team, admin memberships), verified because the provider vouched for the address.

An unverified email is never matched or created against, because the provider's assertion is the only proof of ownership in the flow. An OAuth-created account has no usable password until its owner sets one through the reset flow.

The session cookie is the one password login issues. Failures redirect to `/sign-in?error=<code>`, where the code is stable and the detail stays in the logs:

| Code | Meaning |
|---|---|
| `oauth_unavailable` | Unknown provider, unconfigured provider, or the issuer could not be reached |
| `oauth_denied` | The user or the provider refused the request |
| `oauth_expired` | The state token was unknown, already used, or too old |
| `oauth_email_unavailable` | The provider returned no email address |
| `oauth_email_unverified` | The provider would not vouch for the address it returned |
| `oauth_failed` | The code exchange, the ID token, or this application failed |

Providers are OpenID Connect only, because a discovery document and a signed ID token are what make an identity verifiable. Google ships in the registry; each provider reads `<PROVIDER>_OAUTH_CLIENT_ID`, `<PROVIDER>_OAUTH_CLIENT_SECRET`, and an optional `<PROVIDER>_OAUTH_ISSUER` override, and is enabled by the presence of the first two. Register `<APP_URL>/auth/oauth/<provider>/callback` as the redirect URI. GitHub publishes no discovery document and issues no ID token, so it needs a plain OAuth 2 path with a provider-specific profile fetch, which the framework does not have.

## Tenancy endpoints

The framework mounts the tenancy surface under `/tenancy`. These are account (browser session) routes, not `/api/v1` routes: they administer the tenant the API itself is scoped to. [Tenancy, teams, and organizations](tenancy.md) covers the model, the invariants each route enforces, and the cascade semantics of the deletions.

| Route | Guard | Effect |
|---|---|---|
| `GET /tenancy/memberships` | signed in | Everything the caller belongs to, grouped by organization |
| `GET /tenancy/teams/{team_id}/members` | team member | The team roster, pending invitations included |
| `POST /tenancy/invitations` | admin on the target | Invite an email to a team or an organization |
| `POST /tenancy/invitations/claim` | signed in | Claim an invitation token |
| `POST /tenancy/organizations` | signed in | Create an organization with its default team |
| `PATCH /tenancy/organizations/{organization_id}` | org admin | Rename the organization |
| `DELETE /tenancy/organizations/{organization_id}` | org admin | Delete the organization and everything under it |
| `POST /tenancy/organizations/{organization_id}/teams` | org admin | Create a team |
| `DELETE /tenancy/organizations/{organization_id}/teams/{team_id}` | org admin | Delete a team and its records |
| `DELETE /tenancy/organizations/{organization_id}/invitations/{invitation_id}` | org admin | Revoke a pending invitation in the organization |
| `PATCH /tenancy/teams/{team_id}` | team admin | Rename the team |
| `PATCH /tenancy/teams/{team_id}/members/{membership_id}` | team admin | Replace a member's roles |
| `DELETE /tenancy/teams/{team_id}/members/{membership_id}` | team admin | Remove a member |
| `POST /tenancy/teams/{team_id}/leave` | team member | Leave the team |
| `DELETE /tenancy/teams/{team_id}/invitations/{invitation_id}` | team admin | Revoke a pending team invitation |

Guarded routes answer `401` when signed out, `404` when the caller is not a member of the named tenant, and `403` when a member lacks the role. A request that only the current state refuses, such as demoting a team's last admin, answers `409 Conflict`.

## Webhooks

- **Outgoing**: applications emit webhooks using the same serializers as the API, with a user-facing subscription and debugging UI. Delivery runs through the background job queue with retries.
- **Incoming**: `anubis scaffold webhook <name>` generates a receiving endpoint, signature verification stub, and tests.

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

## Versioning

`/api/v1` is stable once users build against it. A breaking change means minting `/api/v2` handlers and serializers while `/api/v1` continues to serve frozen behavior. The scaffolder always targets the newest version.

## Documentation and client generation

Handlers and serializers register with utoipa, producing an OpenAPI 3.1 document served at `/api/v1/openapi.json`, with human-readable Scalar docs at `/api/v1/docs`. `anubis openapi` exports the document for tooling.

`anubis client generate-ts` renders the document as the generated TypeScript client (`createAnubisV1`): exported wire types plus one ky function per operation, authenticated with a platform bearer token. Output is deterministic and house-style, and CI regenerates it and fails on drift, so the client can never lag the API. Types keep wire field names (snake_case) because they are the contract itself.

## Authentication

- **Browser**: Postgres-backed cookie sessions. The cookie carries an opaque 256-bit token (`HttpOnly`, `SameSite=Lax`, `Secure` in production); the database stores only the token's SHA-256, so a leaked database yields no usable sessions. Passwords hash with argon2id. Handlers require sign-in via the `CurrentUser` extractor.
- **Sign-in methods**: password; passwordless emailed 6-digit codes (10-minute life, attempt-limited, enumeration-safe); and passkeys (WebAuthn discoverable credentials, password-manager-first, cross-platform authenticators welcome).
- **Second factor**: TOTP (authenticator apps) with QR enrollment and single-use recovery codes. When confirmed, password and email-code login answer a 5-minute challenge instead of a session; a passkey is multi-factor by construction and bypasses the challenge. OAuth providers via OpenID Connect remain on the roadmap (M4).
- **Account management routes**: profile (names, time zone, locale), avatar upload/serve/delete, signed-in password and email change, session listing and revocation, and password-confirmed account deletion.
- **API**: per-team Platform Applications, each issuing bearer access tokens (Doorkeeper's role in Bullet Train). Tokens follow the framework discipline (256-bit, SHA-256 at rest, shown exactly once at creation or rotation) and do not expire; rotation and application deletion are the revocation paths. Management endpoints live under `/developers/teams/{team_id}/platform-applications` (create, list, delete, rotate-token), team-scoped through the `TeamMember` guard and restricted to the admin role. The `ApiCaller` extractor resolves `Authorization: Bearer` to the owning application and team, which scopes everything a v1 handler may touch. The framework ships `GET /api/v1/team` as the pattern's reference endpoint; API serializers live in the version module (`TeamV1`) and freeze with it.

Authorization is identical in both paths: the compiled `roles.yml` permissions module authorizes every request against the membership's roles and the resource's ownership chain.

## Webhooks

- **Outgoing**: applications emit webhooks using the same serializers as the API, with a user-facing subscription and debugging UI. Delivery runs through the background job queue with retries.
- **Incoming**: `anubis scaffold webhook <name>` generates a receiving endpoint, signature verification stub, and tests.

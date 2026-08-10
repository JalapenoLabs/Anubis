# REST API

Every Anubis application ships a versioned, documented, public REST API from day one, because scaffolding generates it alongside the web UI. This mirrors Bullet Train's "zero-effort API" goal.

## Structure

- Routes live under `/api/v1/...`, one handler file per route, directory structure matching the URL structure.
- Web (account) handlers and API handlers are separate but share the same permission checks and the same serializers.
- Request validation is the API-first source of truth: the accepted-fields definition lives in the current API version and the account handlers reuse it. Bumping the API version freezes the old definition automatically, which is what makes versioning safe.

## Versioning

`/api/v1` is stable once users build against it. A breaking change means minting `/api/v2` handlers and serializers while `/api/v1` continues to serve frozen behavior. The scaffolder always targets the newest version.

## Documentation and client generation

Handlers and serializers register with utoipa, producing an OpenAPI 3.1 document served at a stable path. That document drives:

1. Human-readable API docs for the application's developers menu.
2. The generated TypeScript client: types plus ky route functions per resource, consumed through SWR hooks in the SPA.

The SPA consumes the same public API it documents, so the API can never lag the UI.

## Authentication

- **Browser**: Postgres-backed cookie sessions. The cookie carries an opaque 256-bit token (`HttpOnly`, `SameSite=Lax`, `Secure` in production); the database stores only the token's SHA-256, so a leaked database yields no usable sessions. Passwords hash with argon2id. Handlers require sign-in via the `CurrentUser` extractor. Optional TOTP 2FA and OAuth providers via OpenID Connect are on the roadmap.
- **API**: per-team Platform Applications, each issuing bearer access tokens (Doorkeeper's role in Bullet Train). Tokens are provisioned in a "Developers" section of the app UI.

Authorization is identical in both paths: the compiled `roles.yml` permissions module authorizes every request against the membership's roles and the resource's ownership chain.

## Webhooks

- **Outgoing**: applications emit webhooks using the same serializers as the API, with a user-facing subscription and debugging UI. Delivery runs through the background job queue with retries.
- **Incoming**: `anubis scaffold webhook <name>` generates a receiving endpoint, signature verification stub, and tests.

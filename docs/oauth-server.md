# The OAuth authorization server

`anubis::oauth_server` makes a deployment an OAuth 2.1 authorization server, so a person can let a program act for them: add the deployment's `/mcp` to Claude Code, Claude Desktop, or Codex, sign in once in the browser, approve the program, and from then on it calls the application's [MCP tools](mcp.md) as that person. A program on a machine with no browser, such as an agent on a headless server, uses the [device flow](#the-device-flow) instead: it shows a short code, and the person enters it and approves from any browser.

This is the opposite direction from [OAuth sign-in](api.md#oauth-sign-in), where the application signs a person in with Google. Here the application is the one vouching for the person, and the program is the client.

## Wiring an application

```rust
let scopes = anubis::oauth_server::Scopes::new()
    .scope("projects:read", "See your projects and their tasks")
    .scope("projects:write", "Create and change projects in your name");
let authorization = anubis::oauth_server::Server::new(pool.clone(), &config, scopes);

let app = Router::new()
    .merge(anubis::oauth_server::router(&authorization, &rate_limit))
    .merge(anubis::mcp::router(&authorization, tools));
```

Both routers merge at the root, because the discovery documents live at paths RFC 8414 and RFC 9728 fix. The starter does this in `connected_clients` in `main.rs`, and declares its scopes in `oauth_scopes` and its tools in `mcp_tools` in `lib.rs`. Nothing is configured through the environment: the issuer is `APP_URL`, so `APP_URL` must be the origin people and programs reach the application at, with no path.

## Routes

| Route | Who calls it | Effect |
|---|---|---|
| `GET /.well-known/oauth-authorization-server` | the program | Authorization Server Metadata (RFC 8414) |
| `GET /.well-known/oauth-protected-resource/mcp` | the program | Protected Resource Metadata (RFC 9728), at the path MCP clients try first |
| `GET /.well-known/oauth-protected-resource` | the program | The same document, at the root path clients fall back to |
| `POST /oauth/register` | the program | Dynamic Client Registration (RFC 7591), bounded |
| `GET /oauth/authorize` | the browser | Validate the request, store it, open the consent screen |
| `POST /oauth/device_authorization` | the program | Start the device flow: a device code and a user code (RFC 8628) |
| `POST /oauth/device_verification` | the code-entry page | The request a user code names (session) |
| `GET /oauth/requests/{id}` | the consent screen | What the person is asked to approve (session) |
| `POST /oauth/requests/{id}` | the consent screen | Approve or deny, answering where the browser goes (session) |
| `POST /oauth/token` | the program | Exchange a code or a device code, or rotate a refresh token |
| `POST /oauth/revoke` | the program | Revoke a token (RFC 7009) |
| `GET /oauth/connections` | account settings | The account's connected clients (session) |
| `DELETE /oauth/connections/{id}` | account settings | Revoke one (session) |

`/oauth`, `/mcp`, and `/.well-known` are reserved prefixes, so an unknown path under them answers a JSON `404` rather than the SPA. That matters for `/.well-known` in particular: a client probing for `openid-configuration`, which this server does not publish, must read a `404` and move on.

## The ceremony

1. The program calls `/mcp` without a token and reads the `401`'s `WWW-Authenticate: Bearer resource_metadata="<APP_URL>/.well-known/oauth-protected-resource/mcp", scope="..."`.
2. It reads the Protected Resource Metadata, then the Authorization Server Metadata it names.
3. It identifies itself: by the URL of its Client ID Metadata Document when the server supports one, which this one does, or by registering.
4. It sends the person's browser to `/oauth/authorize` with an `S256` code challenge, its `state`, the scopes, and `resource=<APP_URL>/mcp`.
5. The server validates everything, stores the request, and redirects to the SPA's consent screen at `/consent?request=<id>`. The SPA signs the person in first if it has to and comes back.
6. The person approves or denies. Approving creates a **grant** and a single-use code, and the browser goes to the program's redirect URI with `code`, `state`, and `iss`.
7. The program exchanges the code with its PKCE verifier for an access token and a refresh token.

Nothing the program sent rides through the browser twice: step 5 stores the request, and the decision acts on the stored row.

## The device flow

The device authorization grant (RFC 8628) is for a program that cannot receive a redirect: an agent on a server with no screen, a command-line tool over SSH. It ends at the same consent screen, with the same grant, tokens, scopes, and resource binding, and the connection is listed and revoked like any other.

1. The program posts its `client_id`, the `scope` it wants, and optionally `resource=<APP_URL>/mcp` to `POST /oauth/device_authorization`, a form body like the token endpoint's.
2. The server answers with a `device_code` for the program to keep, a `user_code` such as `WDJB-MJHT` for the person, `verification_uri` (`<APP_URL>/device`), `verification_uri_complete` (the same with `?user_code=WDJB-MJHT`, for a link or a QR code), `expires_in` (600), and `interval` (5).
3. The program shows the person the code and the address, and polls `POST /oauth/token` with `grant_type=urn:ietf:params:oauth:grant-type:device_code`, its `device_code`, and its `client_id`.
4. The person opens the address in any browser, signs in if they have to, and types the code. The SPA's code-entry page posts it to `POST /oauth/device_verification` and opens the consent screen for the request it names.
5. The person approves or denies. Approving creates the grant, exactly as it does for a redirecting client, and the browser returns to `/device?result=approved` (or `denied`), which says so.
6. The program's next poll is answered with an access token and a refresh token, which behave like every other pair from here on.

### Polling

Until the person decides, a poll answers `authorization_pending`. A poll that arrives sooner than `interval` seconds after the last one answers `slow_down`, and the interval grows by five seconds for that poll and every one after it, as RFC 8628 section 3.5 requires; the first poll is never too soon. A refusal answers `access_denied`, a code past its ten minutes answers `expired_token` (an approval does not extend it), and a code nobody issued, or one presented by a different client, answers `invalid_grant` and changes nothing.

Every poll writes: its time, the grown interval, or the exchange. So a poll locks its device code's row, decides from what that row says, and writes before it lets go, and polls of one code take turns. The decision is one conditional update of the same row, which only an undecided, unexpired code takes, so an approval racing a refusal, a second browser, or the clock either lands or answers `404` and rolls back the grant it was about to create.

### Exchanged once

A device code is exchanged once. Presented again, it is held twice, and the server treats that exactly as it treats a replayed authorization code: the grant is revoked and `oauth.device_code_reused` recorded. The consequence for a program is that two polls in flight at the same moment after an approval mint one token set, and the second one's arrival ends it. A program that polls one request at a time, as RFC 8628 describes, never meets that.

### The codes

The device code is the framework's usual 256 random bits. The user code is eight letters from RFC 8628's twenty consonants, `BCDFGHJKLMNPQRSTVWXZ`, shown as two groups of four and compared without regard to case, dashes, or spaces. Both are stored as SHA-256 and live ten minutes. The user code's 34 bits are what make it typeable, and they are guarded online rather than by size:

- **Entry needs a session.** The approval binds to the account that made it, and the account is a key a guesser cannot rotate.
- **Entry is charged twice**: thirty an hour per client address (`Budget::UserCodePerClient`) and thirty an hour per account (`Budget::UserCodePerAccount`), before anything is looked up, so a refusal says nothing about the code.
- **Every unusable code reads the same**: malformed, never issued, expired, and already decided all answer one `400` with one sentence.
- **At most 5,000 device codes wait at once.** `POST /oauth/device_authorization` is open to strangers, takes the same thirty an hour per address as `/oauth/authorize` (`Budget::Authorization`, one balance across both), and answers `503` at the ceiling, so a flood fills a bounded table, and the odds of a guess naming any live code stay below one in five million.

A user code is also guessable offline from a leaked database, and worth nothing there: it lives ten minutes, and finding one only lets its finder approve somebody else's device into their own account.

When the database cannot be reached, every device route answers `server_error` (the token and device authorization endpoints in OAuth's shape, the session routes as the framework's `500`) and logs the cause, never a code. The limiter lives in the process, so it cannot be unreachable; a request that arrives with no client address, which only a composition bug produces, passes the per-address budget with one warning in the log, and the per-account budget still holds code entry.

### One request, decided once

Starting a device flow stores an authorization request beside the device code, the same row a redirecting client's `/oauth/authorize` stores, recording that it is delivered to a device rather than to a redirect URI. Entering the user code answers that request's id, so two browsers entering one code see one request, and the consent screen's decision consumes it as it consumes any request: the first decision lands and the second finds it gone. A decided code no longer resolves on the code-entry page.

### Phishing

The device flow's inherent weakness is RFC 8628 section 5.4's remote phishing: anyone can start a device flow on their own machine and send the code, or the `verification_uri_complete` link, to somebody else, hoping they approve it. Approving would hand the sender's device a connection to that person's account. The flow cannot tell a code typed from one's own screen from a code somebody sent, so the person has to, and the screens make that the obvious question:

- The consent screen names the client and who vouches for it, exactly as for any client, and in place of the redirect address it warns that approving connects a device that showed a code, to be allowed only if the person started it on a device of their own and denied if someone sent the code.
- A link carrying the code only fills in the field. The code-entry page asks the person to check it against their device's screen and never submits on their behalf.

An application whose clients are all its own can tighten this further, for instance by publishing a metadata document for its device client so the consent screen shows a host that vouches for it.

## Clients

The [MCP authorization specification](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization) orders a client's choices: a Client ID Metadata Document when the server advertises support, Dynamic Client Registration otherwise. Claude (every surface) and Codex both take the document when the metadata says `client_id_metadata_document_supported: true` and lists `none` among `token_endpoint_auth_methods_supported`, which it does. Registration remains for clients that predate documents.

Every client is **public**. No secret is ever issued, and a client proves itself by holding its PKCE verifier, its device code, and its current refresh token. Both kinds of client are rows in `oauth_clients`, and the row records which kind it is rather than inferring it from the id.

A client also declares the grants it uses, in RFC 7591's `grant_types`, and the row records them. A client that declares nothing gets the authorization code flow, as RFC 7591 says. A client using the device flow names `urn:ietf:params:oauth:grant-type:device_code`, and `/oauth/device_authorization` refuses any other with `unauthorized_client`; `/oauth/authorize` likewise refuses a client that did not declare `authorization_code`. Every grant is issued refresh tokens, so `refresh_token` is always recorded. Only the authorization code flow redirects, so only its clients must register a redirect URI.

### Client ID Metadata Documents

A client whose `client_id` is an `https` URL, such as Claude Code's `https://claude.ai/oauth/claude-code-client-metadata`, is asking this server to fetch that URL and believe it. The URL arrives in an unauthenticated query string, so the fetch follows `draft-ietf-oauth-client-id-metadata-document` to the letter:

- The URL is `https`, has a path, carries no fragment, credentials, or dot segments, and is already canonical.
- The host is resolved here, **every** address must be public (no private, loopback, link-local, shared, documentation, reserved, or multicast range, and no IPv6 prefix that maps one back in), and the connection is pinned to the checked address, so a DNS answer that changes in between cannot point the fetch inside the network.
- No redirects, only a `200` is a document, at most five kilobytes are read, five seconds in all.
- `/oauth/authorize`, which is where a fetch is triggered, takes thirty requests an hour per client address (`Budget::Authorization`), so the endpoint cannot be used to make this server fetch the internet on a stranger's behalf.
- The document's `client_id` must equal the URL, it must name the client and a grant this server supports among its `grant_types`, it must name 1 to 10 redirect URIs this server would register (none are needed for the device flow alone), and it must not describe a client that authenticates with a shared secret. A grant this server does not support is passed over rather than refused, because a document describes its client to every server it meets.
- It is cached per its `Cache-Control`, between five minutes and a day, an hour when it says nothing. A failed fetch is never cached and refuses the request rather than falling back to a stale row.
- A cached document nothing holds on to, no grant and no pending request, is forgotten once it is stale, swept whenever another document is fetched. The row is only a cache of what the URL says, so forgetting it costs a fetch, and a flood of distinct document URLs leaves rows that live a day at most rather than forever.

Outside production, documents may be fetched from loopback hosts over `http`, which the draft permits for development and testing. That is how the integration tests serve one, and how a developer tries a local client.

### Dynamic registration

`POST /oauth/register` is open to anybody, so it is bounded three ways: ten registrations an hour per client address (`Budget::ClientRegistration`); a public client, the `authorization_code`, `urn:ietf:params:oauth:grant-type:device_code`, and `refresh_token` grants (a grant it names that this server does not support is refused), the `code` response type, up to 10 redirect URIs (at least one for the authorization code flow), and a name of at most 100 characters; and a registration nobody completed a grant with is swept a day later, unless a device flow it started is still waiting, while 5,000 such rows exist the endpoint answers `503`. A flood from rotating addresses therefore fills a bounded table rather than an unbounded one. The response echoes the grants recorded, and `response_types` is `["code"]` for a client of the authorization code flow and empty for a device-only one.

A registered client's name is its own word, and the consent screen says so.

### Redirect URIs

A redirect URI registers when it is `https`, or `http` on a loopback host (`localhost`, `127.0.0.1`, `[::1]`). A presented one must match a registration exactly, with the one exception RFC 8252 requires: a loopback registration matches any port, because a native program binds a random port at sign-in and registers the URI without one. Claude Code and Codex register both `http://localhost/callback` and `http://127.0.0.1/callback` for that reason, and host, path, and query still match exactly.

## Errors before and after the redirect URI is trusted

Until the client and its redirect URI check out, nothing is sent to that URI, because sending a browser to an unverified address is an open redirect. Those failures land on the consent screen as `/consent?error=<code>`: `invalid_client`, `invalid_redirect_uri`, `invalid_request`, or `server_error`. After that, failures go back to the client as `error=<code>` with its `state` and this server's `iss`: `unauthorized_client` (a client that did not declare `authorization_code`), `unsupported_response_type`, `invalid_request` (a missing or non-`S256` challenge, or an oversized `state`), `invalid_scope`, `invalid_target` (a `resource` that is not this server's), `access_denied`, or `server_error`.

The device authorization endpoint answers in the token endpoint's JSON shape instead, as RFC 8628 has it: `invalid_request`, `invalid_client` (`401`, an unknown client or a document that failed its checks), `unauthorized_client`, `invalid_scope`, `invalid_target`, `temporarily_unavailable` (`503`, at the ceiling of waiting device codes), or `server_error`. A parameter sent with no value counts as not sent.

## Scopes

The application declares them, in Rust, with `Scopes::new().scope(name, description)`. The framework hardcodes none. A scope's name is what a token carries and the description is what the consent screen shows. `offline_access` is refused as a name: the MCP specification keeps it out of what a server advertises, and a refresh token is issued on every grant anyway.

A request that names no scope gets a grant for none. Such a token still proves who the person is, which is all the framework's `whoami` tool needs.

Scopes should be small enough that reading something never implies changing it: `projects:read` and `projects:write`, not `projects`.

## Grants and tokens

A **grant** is one person's consent for one client, and every token descends from one: the code, each access token, and the refresh-token family. Revoking the grant ends the connection whole. It is what the account screen lists as a connected app.

| Credential | Lifetime | Stored as |
|---|---|---|
| Authorization request | 10 minutes | the row, consumed by the decision |
| Authorization code | 2 minutes, single use | SHA-256 |
| Device code | 10 minutes, exchanged once | SHA-256 |
| User code | 10 minutes, spent by the decision | SHA-256 |
| Access token | 1 hour | SHA-256, with its scopes |
| Refresh token | 30 days, renewed by every rotation | SHA-256 |

Tokens are opaque and looked up on every request, so revocation is immediate. Every token is bound to the resource `<APP_URL>/mcp` (RFC 8707); a token for any other resource is refused exactly like an unknown one, which is the audience check the specification requires. A client that omits `resource` gets the one resource there is; one that names another is refused with `invalid_target`. The comparison parses the URL first, so an uppercase scheme or host and a trailing slash still match.

### Spending, and reuse

A code or a refresh token is spent with one conditional update, `SET used_at = now() WHERE used_at IS NULL`, never a read followed by a write, so two exchanges racing for one credential cannot both win. A device code is spent under its row's lock, because every poll of it writes (see [polling](#polling)). A credential presented **after** it was spent means two parties hold it, so the server revokes the whole grant and records why:

- A replayed code revokes everything that code produced (`oauth.code_reused`).
- A rotated-away refresh token revokes the family (`oauth.refresh_reused`).
- A device code polled again after its exchange revokes its grant (`oauth.device_code_reused`).

The consequence for a client is that two refreshes sent at the same moment with one token end its connection. That is the trade OAuth 2.1 makes for public clients, and a client that serializes its refreshes never meets it.

A refresh may ask for fewer scopes than the grant holds and is answered with an access token carrying only those. Asking for one the grant does not hold is refused with `invalid_scope`, and checked before the token is spent, so the client can retry.

Expired rows are swept as a grant issues tokens, as requests are stored, and as device flows start. A spent refresh token or device code is kept until it expires, because presenting it again is the reuse being watched for. Revoking a grant deletes an approved device code that was not exchanged yet, so a program that polls after its person revoked, or reset their password, reads `invalid_grant`. A cached metadata document or a dynamic registration with a device flow still waiting is never swept, so a program keeps reading `access_denied` after a refusal rather than a refusal it never earned.

## The resource side

`Bearer` is to a connected client what `CurrentUser` is to a browser: an extractor yielding the same `User`, plus the token's scopes, the client's name and id, and the grant. A session cookie never satisfies it. A refusal answers `401` with the challenge above; `error="invalid_token"` is added when a token was presented and failed. `Bearer::require_scope` answers `403` with `error="insufficient_scope"` and the scope needed, the step-up challenge the specification describes, for a handler an application protects with it. The MCP endpoint is the one place the framework uses it; see [mcp.md](mcp.md) for how a tool's scope is enforced there.

A live token whose account an operator gave a [temporary password](tenancy.md#temporary-passwords) answers `403` with the code `password_change_required`, the same refusal `CurrentUser` gives a session, and carries no challenge: the token is valid and signing in again would not help. The check runs after the token is resolved, so an unknown token still reads `401` and reveals nothing about any account. The consent routes take `CurrentUser`, so a flagged account cannot approve a new client either.

### When the password is replaced

Setting a temporary password and completing a password reset both say the old credentials may be in the wrong hands, so each **revokes every grant the account holds**, in the same transaction as the new password and beside the sessions it deletes: every access token and every refresh-token family. Each grant records `oauth.revoked`, attributed to whoever caused it (the operator for a temporary password, the account for a reset), and the request id ties it to the credential event beside it. A revoked token reads `401` with the challenge, which sends the program back through sign-in and consent. A voluntary `POST /auth/change-password` keeps every grant, because the person proved they hold the current password. Account deletion removes grants with the account.

## The account surface

The consent screen is the application's SPA at `/consent`, the path `anubis::oauth_server::CONSENT_PATH` names and the starter's `UrlTree.consent` matches. It names the client, the host that vouches for it (a metadata document's host, which the client cannot fake) or a warning that nobody does, the host the code goes to with a warning when that is a program on this device (or, for a device request, the [phishing](#phishing) warning in its place), the scopes with their descriptions, and the account it will act as. `GET /oauth/requests/{id}` says which with `delivery`: `{"kind": "redirect", "host", "is_loopback"}` or `{"kind": "device"}`, `AuthorizationDelivery` in `@jalapenolabs/anubis`.

The code-entry page is the SPA at `/device`, the path `anubis::oauth_server::DEVICE_PATH` names and the starter's `UrlTree.device` matches; it is the device flow's `verification_uri`. It takes a signed-in person, prefills the code from `?user_code=` (`USER_CODE_PARAM`) without submitting it, opens the consent screen for the request the code names (`verifyUserCode` in `@jalapenolabs/anubis`), counts a `429` down, and says how a decision went when the backend returns the browser with `?result=approved` or `denied` (`DEVICE_RESULT_PARAM`). A request that expired or was already answered says so in those words, whether it is read or decided, and an account on an operator's temporary password is told to choose its own and linked to the screen that does.

Security settings list the connected apps with a revoke button. A grant is listed once its program exchanged the code for tokens: approving creates the grant first, and a program that never came back for its tokens never connected, through `useConnectedClients` in `@jalapenolabs/anubis`. Revoking ends the grant on the server first, so a row disappears only once its tokens have stopped working.

## Audit

| Action | When |
|---|---|
| `oauth.granted` | A person approved a client on the consent screen, redirected there or arriving with a device's code |
| `oauth.revoked` | The person revoked a connection, the client revoked its refresh token, or a temporary password or password reset ended every connection |
| `oauth.code_reused` | A spent code was presented again, and its grant was revoked |
| `oauth.refresh_reused` | A rotated-away refresh token was presented again, and its grant was revoked |
| `oauth.device_code_reused` | A device code was polled again after its exchange, and its grant was revoked |

Each is an account-level event in the owner's log: the actor is the person, the subject is the grant (`OauthGrant`), labeled with the client's name, and the change set carries the grant's scopes.

## Metadata, field by field

Four fields decide how Claude and Codex register, and all four are present: `client_id_metadata_document_supported: true`, `token_endpoint_auth_methods_supported: ["none"]`, `code_challenge_methods_supported: ["S256"]` (a client refuses to proceed without it), and `authorization_response_iss_parameter_supported: true`, which the server backs by sending `iss` on every authorization response, errors included. A device client looks for `device_authorization_endpoint` and for `urn:ietf:params:oauth:grant-type:device_code` among `grant_types_supported` (RFC 8628 section 4), and both are present. `scopes_supported` is the application's declared scopes in both documents. The Protected Resource Metadata's `resource` is exactly `<APP_URL>/mcp`, which Claude requires to equal the URL a person typed.

## Why it is built here

No maintained Rust library implements an authorization server for this profile. `oxide-auth` is the nearest, has not released since 2024, and covers none of what MCP needs: metadata documents, Protected Resource Metadata, resource binding, `iss`, or port-agnostic loopback matching for both hosts. The profile itself is small and fully specified, and each piece of it is a few dozen lines on top of the framework's existing token, rate-limit, and audit discipline.

## Roadmap

- **A `403` step-up from the MCP endpoint.** A tool call lacking a scope is answered today as a tool error naming the scope. Answering it as an HTTP `403` with `insufficient_scope` means reading the JSON-RPC body before the protocol layer does.
- **An operator view of registered clients**, to see and remove what registered itself.
- **A periodic sweep** of expired requests, codes, and tokens across every grant, once recurring jobs exist (see [jobs.md](jobs.md#roadmap)). Today they are swept as each grant is used.
- **`private_key_jwt` clients**, which the metadata-document draft allows and no target client uses yet.

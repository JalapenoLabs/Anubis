# The OAuth authorization server

`anubis::oauth_server` makes a deployment an OAuth 2.1 authorization server, so a person can let a program act for them: add the deployment's `/mcp` to Claude Code, Claude Desktop, or Codex, sign in once in the browser, approve the program, and from then on it calls the application's [MCP tools](mcp.md) as that person.

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
| `GET /oauth/requests/{id}` | the consent screen | What the person is asked to approve (session) |
| `POST /oauth/requests/{id}` | the consent screen | Approve or deny, answering where the browser goes (session) |
| `POST /oauth/token` | the program | Exchange a code, or rotate a refresh token |
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

## Clients

The [MCP authorization specification](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization) orders a client's choices: a Client ID Metadata Document when the server advertises support, Dynamic Client Registration otherwise. Claude (every surface) and Codex both take the document when the metadata says `client_id_metadata_document_supported: true` and lists `none` among `token_endpoint_auth_methods_supported`, which it does. Registration remains for clients that predate documents.

Every client is **public**. No secret is ever issued, and a client proves itself by holding its PKCE verifier and its current refresh token. Both kinds of client are rows in `oauth_clients`, and the row records which kind it is rather than inferring it from the id.

### Client ID Metadata Documents

A client whose `client_id` is an `https` URL, such as Claude Code's `https://claude.ai/oauth/claude-code-client-metadata`, is asking this server to fetch that URL and believe it. The URL arrives in an unauthenticated query string, so the fetch follows `draft-ietf-oauth-client-id-metadata-document` to the letter:

- The URL is `https`, has a path, carries no fragment, credentials, or dot segments, and is already canonical.
- The host is resolved here, **every** address must be public (no private, loopback, link-local, shared, documentation, reserved, or multicast range, and no IPv6 prefix that maps one back in), and the connection is pinned to the checked address, so a DNS answer that changes in between cannot point the fetch inside the network.
- No redirects, only a `200` is a document, at most five kilobytes are read, five seconds in all.
- `/oauth/authorize`, which is where a fetch is triggered, takes thirty requests an hour per client address (`Budget::Authorization`), so the endpoint cannot be used to make this server fetch the internet on a stranger's behalf.
- The document's `client_id` must equal the URL, it must name the client and 1 to 10 redirect URIs this server would register, and it must not describe a client that authenticates with a shared secret.
- It is cached per its `Cache-Control`, between five minutes and a day, an hour when it says nothing. A failed fetch is never cached and refuses the request rather than falling back to a stale row.

Outside production, documents may be fetched from loopback hosts over `http`, which the draft permits for development and testing. That is how the integration tests serve one, and how a developer tries a local client.

### Dynamic registration

`POST /oauth/register` is open to anybody, so it is bounded three ways: ten registrations an hour per client address (`Budget::ClientRegistration`); a public client, the `authorization_code` and `refresh_token` grants, the `code` response type, 1 to 10 redirect URIs, and a name of at most 100 characters; and a registration nobody completed a grant with is swept a day later, while 5,000 such rows exist the endpoint answers `503`. A flood from rotating addresses therefore fills a bounded table rather than an unbounded one.

A registered client's name is its own word, and the consent screen says so.

### Redirect URIs

A redirect URI registers when it is `https`, or `http` on a loopback host (`localhost`, `127.0.0.1`, `[::1]`). A presented one must match a registration exactly, with the one exception RFC 8252 requires: a loopback registration matches any port, because a native program binds a random port at sign-in and registers the URI without one. Claude Code and Codex register both `http://localhost/callback` and `http://127.0.0.1/callback` for that reason, and host, path, and query still match exactly.

## Errors before and after the redirect URI is trusted

Until the client and its redirect URI check out, nothing is sent to that URI, because sending a browser to an unverified address is an open redirect. Those failures land on the consent screen as `/consent?error=<code>`: `invalid_client`, `invalid_redirect_uri`, `invalid_request`, or `server_error`. After that, failures go back to the client as `error=<code>` with its `state` and this server's `iss`: `unsupported_response_type`, `invalid_request` (a missing or non-`S256` challenge, or an oversized `state`), `invalid_scope`, `invalid_target` (a `resource` that is not this server's), `access_denied`, or `server_error`.

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
| Access token | 1 hour | SHA-256, with its scopes |
| Refresh token | 30 days, renewed by every rotation | SHA-256 |

Tokens are opaque and looked up on every request, so revocation is immediate. Every token is bound to the resource `<APP_URL>/mcp` (RFC 8707); a token for any other resource is refused exactly like an unknown one, which is the audience check the specification requires. A client that omits `resource` gets the one resource there is; one that names another is refused with `invalid_target`. The comparison parses the URL first, so an uppercase scheme or host and a trailing slash still match.

### Spending, and reuse

A code or a refresh token is spent with one conditional update, `SET used_at = now() WHERE used_at IS NULL`, never a read followed by a write, so two exchanges racing for one credential cannot both win. A credential presented **after** it was spent means two parties hold it, so the server revokes the whole grant and records why:

- A replayed code revokes everything that code produced (`oauth.code_reused`).
- A rotated-away refresh token revokes the family (`oauth.refresh_reused`).

The consequence for a client is that two refreshes sent at the same moment with one token end its connection. That is the trade OAuth 2.1 makes for public clients, and a client that serializes its refreshes never meets it.

A refresh may ask for fewer scopes than the grant holds and is answered with an access token carrying only those. Asking for one the grant does not hold is refused with `invalid_scope`, and checked before the token is spent, so the client can retry.

Expired rows are swept as a grant issues tokens and as requests are stored. A spent refresh token is kept until it expires, because presenting it again is the reuse being watched for.

## The resource side

`Bearer` is to a connected client what `CurrentUser` is to a browser: an extractor yielding the same `User`, plus the token's scopes, the client's name and id, and the grant. A session cookie never satisfies it. A refusal answers `401` with the challenge above; `error="invalid_token"` is added when a token was presented and failed. `Bearer::require_scope` answers `403` with `error="insufficient_scope"` and the scope needed, the step-up challenge the specification describes, for a handler an application protects with it. The MCP endpoint is the one place the framework uses it; see [mcp.md](mcp.md) for how a tool's scope is enforced there.

## The account surface

The consent screen is the application's SPA at `/consent`, the path `anubis::oauth_server::CONSENT_PATH` names and the starter's `UrlTree.consent` matches. It names the client, the host that vouches for it (a metadata document's host, which the client cannot fake) or a warning that nobody does, the host the code goes to with a warning when that is a program on this device, the scopes with their descriptions, and the account it will act as.

Security settings list the connected apps with a revoke button, through `useConnectedClients` in `@jalapenolabs/anubis`. Revoking ends the grant on the server first, so a row disappears only once its tokens have stopped working.

## Audit

| Action | When |
|---|---|
| `oauth.granted` | A person approved a client on the consent screen |
| `oauth.revoked` | The person revoked a connection, or the client revoked its refresh token |
| `oauth.code_reused` | A spent code was presented again, and its grant was revoked |
| `oauth.refresh_reused` | A rotated-away refresh token was presented again, and its grant was revoked |

Each is an account-level event in the owner's log: the actor is the person, the subject is the grant (`OauthGrant`), labeled with the client's name, and the change set carries the grant's scopes.

## Metadata, field by field

Four fields decide how Claude and Codex register, and all four are present: `client_id_metadata_document_supported: true`, `token_endpoint_auth_methods_supported: ["none"]`, `code_challenge_methods_supported: ["S256"]` (a client refuses to proceed without it), and `authorization_response_iss_parameter_supported: true`, which the server backs by sending `iss` on every authorization response, errors included. `scopes_supported` is the application's declared scopes in both documents. The Protected Resource Metadata's `resource` is exactly `<APP_URL>/mcp`, which Claude requires to equal the URL a person typed.

## Why it is built here

No maintained Rust library implements an authorization server for this profile. `oxide-auth` is the nearest, has not released since 2024, and covers none of what MCP needs: metadata documents, Protected Resource Metadata, resource binding, `iss`, or port-agnostic loopback matching for both hosts. The profile itself is small and fully specified, and each piece of it is a few dozen lines on top of the framework's existing token, rate-limit, and audit discipline.

## Roadmap

- **A `403` step-up from the MCP endpoint.** A tool call lacking a scope is answered today as a tool error naming the scope. Answering it as an HTTP `403` with `insufficient_scope` means reading the JSON-RPC body before the protocol layer does.
- **An operator view of registered clients**, to see and remove what registered itself.
- **A periodic sweep** of expired requests, codes, and tokens across every grant, once recurring jobs exist (see [jobs.md](jobs.md#roadmap)). Today they are swept as each grant is used.
- **`private_key_jwt` clients**, which the metadata-document draft allows and no target client uses yet.

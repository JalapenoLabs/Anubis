# The MCP endpoint

`anubis::mcp` serves a [Model Context Protocol](https://modelcontextprotocol.io) endpoint at `/mcp`, and the application fills it with tools. A person adds the deployment to their agent once:

```sh
claude mcp add --transport http acme https://acme.example.com/mcp
```

In Codex the same URL is added as a remote MCP server, and `codex mcp login acme` starts the sign-in.

The first call answers `401`, the agent discovers the [authorization server](oauth-server.md), the person signs in and approves it in the browser, and from then on every tool call runs as that person, under the scopes they granted.

## Tools

A tool is a name, a description, an optional JSON Schema for its arguments, the scopes it requires, and an async function:

```rust
let list_projects = anubis::mcp::Tool::new(
    "list_projects",
    "List the projects you can see.",
    |call: anubis::mcp::ToolCall| async move {
        let projects = projects::visible_to(&call.pool, call.caller.user.id).await?;
        Ok(serde_json::json!({ "projects": projects }))
    },
)
.input_schema(serde_json::json!({
    "type": "object",
    "properties": { "search": { "type": "string" } },
    "additionalProperties": false,
}))
.requires("projects:read");

let tools = anubis::mcp::Registry::new("Acme", env!("CARGO_PKG_VERSION"))
    .tool(anubis::mcp::whoami())
    .tool(list_projects);
```

`ToolCall` carries `caller` (an `oauth_server::Bearer`: the `User`, the scopes, the client), the `arguments`, and the `pool`. A tool answers a JSON value, which is returned as `structuredContent` and as a text block, or a `ToolError`, whose message the person's agent reads. `ToolError::internal()` and `From<diesel::result::Error>` log the cause and answer a generic sentence, the same discipline `ApiError::internal` follows.

**A tool is the person, never more.** It runs behind no guard of its own, so it calls the same functions the application's handlers call, with the same authorization: an ownership check that answers `404` to a handler answers a tool error to an agent. Wrap the existing checks rather than writing new ones.

The starter declares its tools in `mcp_tools` in `lib.rs`. The framework ships one, `whoami`, which needs no scope and answers the account, the client, and the scopes the connection holds: the quickest way for a person to confirm they connected as who they meant.

### What is checked before a tool runs

1. **A bearer token.** Every request to `/mcp` needs one; see below.
2. **The scopes.** A connection lacking a scope the tool requires gets a tool error naming the scope, and the tool never runs. Booting with a tool that requires a scope the authorization server never declared panics, because nobody could ever grant it.
3. **The arguments.** They are validated against the tool's input schema with the `jsonschema` crate. A violation is a tool error rather than a protocol error, so the agent's model reads which argument was wrong and can try again. `rmcp` advertises schemas without enforcing them, which is why the framework does.

Names are 1 to 128 characters of `[A-Za-z0-9_.-]`, input schemas must be JSON objects that compile, and a name registers once; each is checked at boot, with a panic naming the mistake.

## Authentication

`/mcp` is bearer-only. A request without a token, or with one that is unknown, expired, revoked, or bound to another resource, answers `401` with:

```
WWW-Authenticate: Bearer resource_metadata="<APP_URL>/.well-known/oauth-protected-resource/mcp", scope="<declared scopes>"
```

and `error="invalid_token"` added when a token was presented. A valid token for an account on an operator's temporary password answers `403` with the code `password_change_required` instead, as every session route does; see [tenancy.md](tenancy.md#temporary-passwords). A session cookie never authenticates here: the browser's credential and the program's are different things, and a page on this origin making MCP calls with the person's cookie is exactly what the endpoint must not allow.

## Protocol and transport

The protocol layer is [`rmcp`](https://crates.io/crates/rmcp), the official Rust SDK, pinned at 3.5 with only its `server` and `transport-streamable-http-server` features. It serves both protocol eras on one endpoint:

- **The 2025 revisions**, which shipped clients send today: the `initialize` handshake and `notifications/initialized`, then requests.
- **The 2026-07-28 revision**: no handshake, `server/discover`, per-request `_meta`, and the `MCP-Protocol-Version`, `Mcp-Method`, and `Mcp-Name` headers, which `rmcp` validates.

It runs **without sessions** and answers plain JSON whenever a tool emits nothing before its result, so any instance behind a load balancer can serve any request and nothing is held in memory between them. `GET /mcp`, the server-to-client stream the 2026 revision removed, answers `405`.

The transport checks `Host` and `Origin` against `APP_URL`, which is the DNS-rebinding defense the specification requires: a request naming another host is refused, and a request from a browser page on another origin answers `403`. Programs such as Claude Code send no `Origin` and pass. `rmcp` accepts only loopback hosts unless told otherwise, so this is configured from `APP_URL` rather than left at its default.

## The resource is fixed

The endpoint is always `/mcp`, and `<APP_URL>/mcp` is the resource every token is bound to. The Protected Resource Metadata's path, the challenge's URL, the `resource` the authorization server binds, and the development proxy all name it, so it is one constant (`oauth_server::RESOURCE_PATH`) rather than a setting four places would have to agree on.

In development, `APP_URL` is the Vite server, and the starter's `vite.config.ts` proxies `/mcp`, `/oauth`, and `/.well-known` to the backend, so `claude mcp add --transport http dev http://localhost:5173/mcp` connects to a local stack.

## Roadmap

- **A `403` step-up challenge** for a tool call lacking a scope, so a client can ask the person for more without starting over. Today it is a tool error naming the scope; see [oauth-server.md](oauth-server.md#roadmap).
- **Listing tools by scope**, which the specification permits, so a connection sees only what it can call.
- **Resources and prompts**, the protocol's other two primitives, when an application needs them.

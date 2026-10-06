//! A Model Context Protocol endpoint the application fills with tools.
//!
//! [`router`] serves Streamable HTTP at `/mcp`, the resource
//! [`crate::oauth_server`] issues tokens for. Every request must carry a
//! bearer token from that server: a request without one is answered `401`
//! with the `WWW-Authenticate` challenge that sends a client to discover the
//! authorization server, sign the person in, and come back. A tool therefore
//! always runs as a person, through [`ToolCall::caller`], and never as an
//! anonymous caller; a session cookie does not authenticate here.
//!
//! The application supplies the tools in a [`Registry`]; the framework ships
//! [`whoami`], which needs no scope and answers who the connection acts for.
//!
//! ```ignore
//! let server = anubis::oauth_server::Server::new(pool.clone(), &config, scopes);
//! let tools = anubis::mcp::Registry::new("Acme", env!("CARGO_PKG_VERSION"))
//!     .tool(anubis::mcp::whoami());
//! let app = Router::new()
//!     .merge(anubis::oauth_server::router(&server, &rate_limit))
//!     .merge(anubis::mcp::router(&server, tools));
//! ```
//!
//! # The protocol layer
//!
//! JSON-RPC, both protocol eras, and the transport are `rmcp`'s, the official
//! Rust SDK: the 2025 revisions' `initialize` handshake that shipped clients
//! send, and the stateless 2026-07-28 revision with its `server/discover` and
//! per-request metadata. It runs without sessions and answers plain JSON
//! whenever a tool emits nothing before its result, so any instance can serve
//! any request. Host and Origin are checked against `APP_URL`, which is the
//! DNS-rebinding defense the transport specification requires.
//!
//! Arguments are validated against a tool's input schema before it runs, by
//! the framework: `rmcp` advertises a schema without enforcing it. A
//! connection lacking a scope a tool requires gets a tool error naming the
//! scope, and the tool never runs; answering it as an HTTP `403` step-up
//! challenge instead is on the roadmap in `docs/mcp.md`.

mod tool;

use std::sync::Arc;

use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::{Extension, Router};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::json;
use url::Url;

use crate::db::DbPool;
use crate::oauth_server::{Bearer, RESOURCE_PATH, Server};

#[doc(inline)]
pub use tool::{Registry, Tool, ToolCall, ToolError, ToolResult};

/// Returns the MCP endpoint, to merge at the application's root.
///
/// # Panics
/// Panics when a tool requires a scope the authorization server did not
/// declare: no person could ever grant it, so the tool could never run, and
/// that is a mistake in the application's own composition.
pub fn router(server: &Server, registry: Registry) -> Router {
    for scope in registry.required_scopes() {
        assert!(
            server.scopes().contains(scope),
            "an MCP tool requires the scope {scope:?}, which the authorization server's \
             Scopes do not declare",
        );
    }

    let handler = Handler {
        registry: Arc::new(registry),
        pool: server.pool().clone(),
    };
    let service = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(NeverSessionManager::default()),
        transport_config(server.issuer()),
    );

    Router::new()
        .route_service(RESOURCE_PATH, service)
        .layer(middleware::from_fn(require_bearer))
        .layer(Extension(server.clone()))
}

/// The transport's settings for a public deployment at `issuer`.
///
/// `rmcp` accepts only loopback `Host` headers unless told otherwise, which is
/// right for a server on a laptop and refuses every request a deployment
/// receives, so the host is `APP_URL`'s. The origin is too, with its port
/// spelled out: a browser page on another origin is exactly what the
/// specification's Origin check exists to refuse, and a program such as
/// Claude Code sends no Origin at all.
fn transport_config(issuer: &str) -> StreamableHttpServerConfig {
    let mut hosts = Vec::new();
    let mut origins = Vec::new();
    if let Ok(url) = Url::parse(issuer)
        && let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default())
    {
        hosts.push(host.to_owned());
        hosts.push(format!("{host}:{port}"));
        origins.push(format!("{}://{host}:{port}", url.scheme()));
    }
    if hosts.is_empty() {
        tracing::error!(
            mcp.issuer = issuer,
            "APP_URL names no host, so the MCP endpoint will refuse every request: {{mcp.issuer}}",
        );
    }

    StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(hosts)
        .with_allowed_origins(origins)
}

/// Refuses a request without a live bearer token, before the protocol sees it.
async fn require_bearer(bearer: Bearer, mut request: Request, next: Next) -> Response {
    request.extensions_mut().insert(bearer);
    next.run(request).await
}

/// The `rmcp` handler: the registry, answering the protocol's tool calls.
#[derive(Clone)]
struct Handler {
    registry: Arc<Registry>,
    pool: DbPool,
}

impl Handler {
    /// The protocol's description of one tool.
    fn definition(tool: &Tool) -> rmcp::model::Tool {
        rmcp::model::Tool::new(
            tool.name.clone(),
            tool.description.clone(),
            Arc::new(tool.input_schema.clone()),
        )
    }
}

impl ServerHandler for Handler {
    fn get_info(&self) -> ServerConfig {
        let config = ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                self.registry.name.clone(),
                self.registry.version.clone(),
            ));
        match &self.registry.instructions {
            Some(instructions) => config.with_instructions(instructions.clone()),
            None => config,
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = self.registry.tools.iter().map(Self::definition).collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    /// Lets the transport read a tool's input schema, which it uses to check
    /// the 2026 revision's `Mcp-Param-*` headers against the body.
    fn get_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
        self.registry.find(name).map(Self::definition)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some(tool) = self.registry.find(&request.name) else {
            return Err(ErrorData::invalid_params(
                format!("There is no tool named {}.", request.name),
                None,
            ));
        };

        // The bearer middleware runs before every request reaches the
        // protocol, and the transport hands the request's parts to the
        // handler, so a call without a caller is a composition bug.
        let Some(caller) = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<Bearer>())
            .cloned()
        else {
            tracing::error!(
                mcp.tool = %request.name,
                "an MCP tool call arrived without a bearer caller: {{mcp.tool}}",
            );
            return Err(ErrorData::internal_error("The caller is unknown.", None));
        };

        if let Some(missing) = tool.scopes.iter().find(|scope| !caller.has_scope(scope)) {
            return Ok(CallToolResponse::Complete(CallToolResult::error(vec![
                ContentBlock::text(format!(
                    "This connection was not granted the {missing} permission this tool \
                     needs. Reconnect and approve it to use {}.",
                    tool.name,
                )),
            ])));
        }

        // Checked here rather than by the protocol layer, which advertises a
        // schema without enforcing it. A violation is a tool error rather
        // than a protocol error, so the caller's model reads it and can try
        // again with corrected arguments.
        let arguments = request.arguments.unwrap_or_default();
        if let Err(violation) = tool.check_arguments(&arguments) {
            return Ok(CallToolResponse::Complete(CallToolResult::error(vec![
                ContentBlock::text(violation),
            ])));
        }

        let call = ToolCall {
            caller,
            arguments,
            pool: self.pool.clone(),
        };
        let result = match (tool.run)(call).await {
            Ok(value) => CallToolResult::structured(value),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error.message())]),
        };
        Ok(CallToolResponse::Complete(result))
    }
}

/// The framework's own tool: who this connection acts for.
///
/// It requires no scope, because identity is what every token proves, and it
/// is the quickest way for a person to confirm a client connected as the
/// account they meant.
#[must_use]
pub fn whoami() -> Tool {
    Tool::new(
        "whoami",
        "Answer which account this connection acts for, the client it was granted to, and \
         the scopes it holds.",
        |call: ToolCall| async move {
            let user = &call.caller.user;
            Ok(json!({
                "user": {
                    "id": user.id,
                    "email": user.email,
                    "first_name": user.first_name,
                    "last_name": user.last_name,
                },
                "client": call.caller.client_name,
                "scopes": call.caller.scopes,
            }))
        },
    )
}

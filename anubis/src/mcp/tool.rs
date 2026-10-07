//! What an application registers: tools, and the registry that holds them.

use std::collections::BTreeSet;
use std::fmt::{self, Debug, Formatter};
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::db::DbPool;
use crate::oauth_server::Bearer;

/// The longest tool name the MCP specification recommends.
const MAX_TOOL_NAME_LENGTH: usize = 128;

/// One call to a tool: who is calling, with what, and the database.
pub struct ToolCall {
    /// The person the connected client acts for, and what they granted it.
    pub caller: Bearer,
    /// The arguments, already checked against the tool's input schema.
    pub arguments: Map<String, Value>,
    /// The application's pool, so a tool reads and writes like a handler.
    pub pool: DbPool,
}

impl Debug for ToolCall {
    /// The pool holds connections that do not implement `Debug`.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolCall")
            .field("caller", &self.caller)
            .field("arguments", &self.arguments)
            .finish_non_exhaustive()
    }
}

/// A tool that ran and could not do what was asked.
///
/// The message reaches the person through their client, so it is written for
/// them; a failure of this server's own is logged where it happened and
/// answered with [`ToolError::internal`], exactly as a handler answers
/// [`crate::http::ApiError::internal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    message: String,
}

impl ToolError {
    /// A failure the caller should read.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// A failure of this server's own, with a deliberately generic message.
    #[must_use]
    pub fn internal() -> Self {
        Self::new("Something went wrong on our side.")
    }

    /// The message the caller reads.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ToolError {}

impl From<diesel::result::Error> for ToolError {
    /// Renders a failed query as [`ToolError::internal`], logging the cause.
    fn from(source: diesel::result::Error) -> Self {
        tracing::error!(
            error.message = %source,
            "an MCP tool's query failed: {{error.message}}",
        );
        Self::internal()
    }
}

/// What a tool answers: a JSON value, or a failure the caller reads.
pub type ToolResult = Result<Value, ToolError>;

type BoxedRun =
    Arc<dyn Fn(ToolCall) -> Pin<Box<dyn Future<Output = ToolResult> + Send>> + Send + Sync>;

/// One tool a connected client may call.
///
/// ```ignore
/// let list_samples = anubis::mcp::Tool::new(
///     "list_samples",
///     "List the published samples.",
///     |call: anubis::mcp::ToolCall| async move {
///         let samples = samples::published(&call.pool).await?;
///         Ok(serde_json::json!({ "samples": samples }))
///     },
/// )
/// .requires("samples:read");
/// ```
#[derive(Clone)]
pub struct Tool {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) input_schema: Map<String, Value>,
    /// The input schema, compiled once at registration.
    validator: Arc<jsonschema::Validator>,
    pub(crate) scopes: Vec<String>,
    pub(crate) run: BoxedRun,
}

impl Debug for Tool {
    /// The handler is a closure, so it is left out.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tool")
            .field("name", &self.name)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

impl Tool {
    /// A tool taking no arguments and requiring no scope beyond a connection.
    ///
    /// # Panics
    /// Panics when `name` is empty, longer than 128 characters, or holds a
    /// character outside `[A-Za-z0-9_.-]`, which the MCP specification
    /// recommends and clients rely on. A bad name is a mistake in the
    /// application's own source, caught the first time it boots.
    #[must_use]
    pub fn new<Run, Answer>(
        name: impl Into<String>,
        description: impl Into<String>,
        run: Run,
    ) -> Self
    where
        Run: Fn(ToolCall) -> Answer + Send + Sync + 'static,
        Answer: Future<Output = ToolResult> + Send + 'static,
    {
        let name = name.into();
        assert!(
            !name.is_empty()
                && name.len() <= MAX_TOOL_NAME_LENGTH
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-')),
            "tool name {name:?} must be 1 to 128 characters of [A-Za-z0-9_.-]",
        );

        // What the specification recommends for a tool without parameters.
        let no_arguments = json!({ "type": "object", "additionalProperties": false });
        let (input_schema, validator) = compile(no_arguments);
        Self {
            name,
            description: description.into(),
            input_schema,
            validator,
            scopes: Vec::new(),
            run: Arc::new(move |call| Box::pin(run(call))),
        }
    }

    /// Sets the JSON Schema the arguments must satisfy.
    ///
    /// The framework checks every call against it before the tool runs (the
    /// protocol layer only advertises it), so a tool reads
    /// [`ToolCall::arguments`] knowing their shape.
    ///
    /// # Panics
    /// Panics when `schema` is not a JSON object, which the specification
    /// requires of an input schema, or is not a schema at all.
    #[must_use]
    pub fn input_schema(mut self, schema: Value) -> Self {
        (self.input_schema, self.validator) = compile(schema);
        self
    }

    /// Checks `arguments` against the input schema, answering the first
    /// violation in words the caller's model can correct itself from.
    pub(crate) fn check_arguments(&self, arguments: &Map<String, Value>) -> Result<(), String> {
        let arguments = Value::Object(arguments.clone());
        match self.validator.iter_errors(&arguments).next() {
            None => Ok(()),
            Some(violation) => Err(format!(
                "The arguments do not match the tool's input schema at {}: {violation}",
                violation.instance_path(),
            )),
        }
    }

    /// Requires `scope` of the connection calling this tool.
    ///
    /// A call from a connection without it is answered with an error naming
    /// the scope, and the tool never runs.
    #[must_use]
    pub fn requires(mut self, scope: impl Into<String>) -> Self {
        self.scopes.push(scope.into());
        self
    }
}

/// Compiles an input schema, keeping the object form the protocol advertises.
fn compile(schema: Value) -> (Map<String, Value>, Arc<jsonschema::Validator>) {
    let validator = jsonschema::validator_for(&schema)
        .unwrap_or_else(|error| panic!("a tool's input schema does not compile: {error}"));
    match schema {
        Value::Object(object) => (object, Arc::new(validator)),
        other => panic!("a tool's input schema must be a JSON object, got {other}"),
    }
}

/// The application's tools, and how the endpoint introduces itself.
///
/// ```ignore
/// let registry = anubis::mcp::Registry::new("Acme", env!("CARGO_PKG_VERSION"))
///     .tool(anubis::mcp::whoami())
///     .tool(list_samples);
/// ```
#[derive(Debug, Clone)]
pub struct Registry {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) instructions: Option<String>,
    pub(crate) tools: Vec<Tool>,
}

impl Registry {
    /// An empty registry for the server named `name` at `version`.
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            instructions: None,
            tools: Vec::new(),
        }
    }

    /// Sets what a client's model is told about this server as a whole.
    #[must_use]
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Adds a tool.
    ///
    /// # Panics
    /// Panics when a tool of the same name is already registered.
    #[must_use]
    pub fn tool(mut self, tool: Tool) -> Self {
        assert!(
            self.find(&tool.name).is_none(),
            "MCP tool {:?} is registered twice",
            tool.name,
        );
        self.tools.push(tool);
        self
    }

    pub(crate) fn find(&self, name: &str) -> Option<&Tool> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    /// Every scope some tool requires, for checking against what the
    /// authorization server declared.
    pub(crate) fn required_scopes(&self) -> BTreeSet<&str> {
        self.tools
            .iter()
            .flat_map(|tool| tool.scopes.iter().map(String::as_str))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Registry, Tool};

    fn noop(name: &str) -> Tool {
        Tool::new(name, "Does nothing.", |_call| async { Ok(json!({})) })
    }

    #[test]
    fn a_tool_starts_with_an_argumentless_schema() {
        let tool = noop("noop");

        assert_eq!(tool.input_schema["type"], "object");
        assert_eq!(tool.input_schema["additionalProperties"], false);
    }

    #[test]
    fn a_registry_knows_which_scopes_its_tools_need() {
        let registry = Registry::new("Test", "1.0.0")
            .tool(noop("read").requires("samples:read"))
            .tool(
                noop("write")
                    .requires("entries:write")
                    .requires("samples:read"),
            );

        let required: Vec<&str> = registry.required_scopes().into_iter().collect();
        assert_eq!(required, ["entries:write", "samples:read"]);
    }

    #[test]
    #[should_panic(expected = "registered twice")]
    fn a_tool_name_is_registered_once() {
        let _registry = Registry::new("Test", "1.0.0")
            .tool(noop("same"))
            .tool(noop("same"));
    }

    #[test]
    #[should_panic(expected = "[A-Za-z0-9_.-]")]
    fn a_tool_name_with_a_space_is_a_bug() {
        let _tool = noop("list samples");
    }

    #[test]
    #[should_panic(expected = "must be a JSON object")]
    fn an_input_schema_must_be_an_object() {
        let _tool = noop("noop").input_schema(json!(true));
    }

    #[test]
    fn arguments_are_held_to_the_schema() {
        let tool = noop("note").input_schema(json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"],
        }));
        let arguments = |value: serde_json::Value| match value {
            serde_json::Value::Object(object) => object,
            _ => unreachable!("the test passes objects"),
        };

        assert_eq!(
            tool.check_arguments(&arguments(json!({ "text": "hi" }))),
            Ok(())
        );
        let refused = tool
            .check_arguments(&arguments(json!({ "text": 42 })))
            .expect_err("a number is not a string");
        assert!(refused.contains("/text"), "{refused}");
        assert!(
            noop("bare")
                .check_arguments(&arguments(json!({ "extra": 1 })))
                .is_err()
        );
    }
}

//! `anubis routes`: print the framework route table.
//!
//! Combines two sources: the framework's curated route manifest (drift-gated
//! by tests in `anubis::manifest`) and the operations of the OpenAPI 3.1
//! document, which contributes every versioned `/api/v1` endpoint without any
//! hand maintenance. Application-defined routes live in the application's own
//! router and do not appear here.

use std::process::ExitCode;

/// HTTP method keys an OpenAPI path item can carry; its other keys
/// (`parameters`, `summary`, ...) are not operations.
const OPENAPI_METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

pub(crate) fn run() -> ExitCode {
    let mut rows = anubis::manifest::framework_routes()
        .iter()
        .map(|entry| {
            (
                entry.area.to_owned(),
                entry.method.to_owned(),
                entry.path.to_owned(),
            )
        })
        .collect::<Vec<_>>();

    let document = match serde_json::to_value(anubis::api::v1::openapi()) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("error: failed to serialize the OpenAPI document: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let Some(paths) = document["paths"].as_object() {
        for (path, item) in paths {
            let Some(item) = item.as_object() else {
                continue;
            };
            for method in item.keys() {
                if OPENAPI_METHODS.contains(&method.as_str()) {
                    // Documented paths already carry the /api/v1 prefix.
                    rows.push(("api".to_owned(), method.to_ascii_uppercase(), path.clone()));
                }
            }
        }
    }

    let area_width = rows.iter().map(|(area, ..)| area.len()).max().unwrap_or(0);
    let method_width = rows
        .iter()
        .map(|(_, method, _)| method.len())
        .max()
        .unwrap_or(0);
    for (area, method, path) in &rows {
        println!("{area:<area_width$}  {method:<method_width$}  {path}");
    }
    println!();
    println!(
        "{} routes. Application routes live in the application's router.",
        rows.len()
    );
    ExitCode::SUCCESS
}

//! The `anubis` command line interface.
//!
//! The CLI is the front door to the framework: stamping new applications,
//! scaffolding models and fields, and compiling `roles.yml`. `scaffold model`
//! generates a model's backend slice today; the rest of the `scaffold` family
//! lands with milestone M4 (see the repository's `docs/scaffolding.md`).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod cli;

/// The Anubis framework CLI.
#[derive(Debug, Parser)]
#[command(name = "anubis", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Stamp a new application from the starter template.
    New {
        /// The application name: lowercase letters, digits, and hyphens.
        name: String,
        /// License of the new application: private, or MIT.
        #[arg(long, value_enum, default_value_t = cli::new::License::Unlicensed)]
        license: cli::new::License,
    },
    /// Generate application code from the living templates.
    Scaffold {
        #[command(subcommand)]
        command: ScaffoldCommand,
    },
    /// Verify toolchain, database, and config health.
    Doctor,
    /// Print the framework route table.
    Routes,
    /// Validate and compile the application's roles.yml.
    Roles {
        #[command(subcommand)]
        command: RolesCommand,
    },
    /// Export the framework's OpenAPI 3.1 document as JSON.
    Openapi {
        /// Write to this path instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Generate clients from the OpenAPI document.
    Client {
        #[command(subcommand)]
        command: ClientCommand,
    },
    /// Mint the secrets an application's environment needs.
    Secret {
        #[command(subcommand)]
        command: SecretCommand,
    },
}

#[derive(Debug, Subcommand)]
enum SecretCommand {
    /// Print a fresh `ANUBIS_SECRET_KEY`.
    Generate,
}

#[derive(Debug, Subcommand)]
enum ScaffoldCommand {
    /// Generate a model's backend slice: migration, schema, model, routes,
    /// permissions, and an integration test.
    Model {
        /// The model name, e.g. `Project`.
        model: String,
        /// The ownership chain ending in `Team`, e.g. `Team` or `Project,Team`.
        ownership: String,
        /// Fields as `name:type`, e.g. `name:text_field`.
        fields: Vec<String>,
    },
    /// Print how to enable an OAuth sign-in provider: its redirect URI and the
    /// two variables that turn its button on. Writes no file.
    Oauth {
        /// The provider key, e.g. `google`.
        provider: String,
    },
    /// Generate the join model two existing team-owned models need before a
    /// has-many-through association can reach between them.
    Join {
        /// The join model name, e.g. `AppliedTag`.
        model: String,
        /// The side that owns the association, e.g. `project_id{class_name=Project}`.
        owner: String,
        /// The side it reaches, e.g. `tag_id{class_name=Tag}`.
        target: String,
    },
    /// Generate a receiving endpoint for a third party's webhooks: the table
    /// they are stored in, the route that stores them, the signature check, and
    /// the background job that processes them.
    Webhook {
        /// The provider key, e.g. `stripe`.
        provider: String,
    },
    /// Add one field to an existing model, propagated through its migration,
    /// schema, model, handlers, test, API module, form, table, and locale file.
    Field {
        /// The model name, e.g. `Project`.
        model: String,
        /// The field as `name:type`, e.g. `priority:text_field`.
        field: String,
    },
}

#[derive(Debug, Subcommand)]
enum ClientCommand {
    /// Generate the TypeScript client for the public API.
    GenerateTs {
        /// An exported OpenAPI document to render instead of the framework's.
        ///
        /// An application's document carries its scaffolded endpoints as well
        /// as the framework's; the starter binary exports it with
        /// `cargo run -p <app> -- openapi`.
        #[arg(long)]
        from: Option<PathBuf>,
        /// Path the generated module is written to.
        #[arg(long, default_value = "frontend/src/api/v1.generated.ts")]
        out: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum RolesCommand {
    /// Parse and resolve roles.yml, reporting any definition errors.
    Check {
        /// Path to the roles file.
        #[arg(long, default_value = "config/roles.yml")]
        file: PathBuf,
    },
    /// Generate the TypeScript permissions module from roles.yml.
    GenerateTs {
        /// Path to the roles file.
        #[arg(long, default_value = "config/roles.yml")]
        file: PathBuf,
        /// Path the generated module is written to.
        #[arg(long, default_value = "frontend/src/roles.generated.ts")]
        out: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let Some(command) = cli.command else {
        println!("Anubis {}", anubis::VERSION);
        println!("Run `anubis --help` for available commands.");
        println!("Scaffold a model with `anubis scaffold model <Model> <ParentChain>`.");
        println!("Roadmap: https://github.com/JalapenoLabs/Anubis/milestones");
        return ExitCode::SUCCESS;
    };

    match command {
        Command::New { name, license } => cli::new::run(&name, license),
        Command::Scaffold {
            command:
                ScaffoldCommand::Model {
                    model,
                    ownership,
                    fields,
                },
        } => cli::scaffold::model(&model, &ownership, &fields),
        Command::Scaffold {
            command:
                ScaffoldCommand::Join {
                    model,
                    owner,
                    target,
                },
        } => cli::scaffold::join(&model, &owner, &target),
        Command::Scaffold {
            command: ScaffoldCommand::Field { model, field },
        } => cli::scaffold::field(&model, &field),
        Command::Scaffold {
            command: ScaffoldCommand::Oauth { provider },
        } => cli::scaffold::oauth(&provider),
        Command::Scaffold {
            command: ScaffoldCommand::Webhook { provider },
        } => cli::scaffold::webhook(&provider),
        Command::Doctor => cli::doctor::run(),
        Command::Routes => cli::routes::run(),
        Command::Roles { command } => run_roles(command),
        Command::Openapi { out } => run_openapi(out.as_deref()),
        Command::Client {
            command: ClientCommand::GenerateTs { from, out },
        } => run_generate_ts(from.as_deref(), &out),
        Command::Secret {
            command: SecretCommand::Generate,
        } => cli::secret::generate(),
    }
}

/// Renders the TypeScript client, from an exported document or the framework's.
fn run_generate_ts(from: Option<&std::path::Path>, out: &std::path::Path) -> ExitCode {
    let client = match from {
        None => anubis::api::v1::typescript_client(),
        Some(path) => {
            let exported = match std::fs::read_to_string(path) {
                Ok(exported) => exported,
                Err(error) => {
                    eprintln!("error: failed to read {}: {error}", path.display());
                    return ExitCode::FAILURE;
                }
            };
            match serde_json::from_str(&exported) {
                Ok(document) => anubis::api::v1::typescript_client_from(&document),
                Err(error) => {
                    eprintln!(
                        "error: {} is not an OpenAPI document: {error}",
                        path.display(),
                    );
                    return ExitCode::FAILURE;
                }
            }
        }
    };

    if let Err(error) = std::fs::write(out, client) {
        eprintln!("error: failed to write {}: {error}", out.display());
        return ExitCode::FAILURE;
    }
    println!("wrote {}", out.display());
    ExitCode::SUCCESS
}

fn run_openapi(out: Option<&std::path::Path>) -> ExitCode {
    let document = anubis::api::v1::openapi();
    let rendered = match serde_json::to_string_pretty(&document) {
        Ok(rendered) => rendered,
        Err(error) => {
            eprintln!("error: failed to serialize the OpenAPI document: {error}");
            return ExitCode::FAILURE;
        }
    };

    match out {
        None => {
            println!("{rendered}");
            ExitCode::SUCCESS
        }
        Some(path) => {
            if let Err(error) = std::fs::write(path, rendered) {
                eprintln!("error: failed to write {}: {error}", path.display());
                return ExitCode::FAILURE;
            }
            println!("wrote {}", path.display());
            ExitCode::SUCCESS
        }
    }
}

fn run_roles(command: RolesCommand) -> ExitCode {
    match command {
        RolesCommand::Check { file } => {
            let set = match load_role_set(&file) {
                Ok(set) => set,
                Err(exit) => return exit,
            };
            let roles = set.role_keys().collect::<Vec<_>>();
            println!(
                "{} is valid: {} roles ({})",
                file.display(),
                roles.len(),
                roles.join(", ")
            );
            ExitCode::SUCCESS
        }
        RolesCommand::GenerateTs { file, out } => {
            let set = match load_role_set(&file) {
                Ok(set) => set,
                Err(exit) => return exit,
            };
            if let Err(error) = std::fs::write(&out, set.to_typescript()) {
                eprintln!("error: failed to write {}: {error}", out.display());
                return ExitCode::FAILURE;
            }
            println!("wrote {}", out.display());
            ExitCode::SUCCESS
        }
    }
}

fn load_role_set(file: &PathBuf) -> Result<anubis::roles::RoleSet, ExitCode> {
    let yaml = match std::fs::read_to_string(file) {
        Ok(yaml) => yaml,
        Err(error) => {
            eprintln!("error: failed to read {}: {error}", file.display());
            return Err(ExitCode::FAILURE);
        }
    };

    match anubis::roles::RoleSet::from_yaml(&yaml) {
        Ok(set) => Ok(set),
        Err(error) => {
            eprintln!("error: {error}");
            Err(ExitCode::FAILURE)
        }
    }
}

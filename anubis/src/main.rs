//! The `anubis` command line interface.
//!
//! The CLI is the front door to the framework: stamping new applications,
//! scaffolding models and fields, and compiling `roles.yml`. Scaffolding
//! subcommands land with milestone M4 (see the repository's
//! `docs/scaffolding.md` for the planned surface).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// The Anubis framework CLI.
#[derive(Debug, Parser)]
#[command(name = "anubis", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
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
        println!("Scaffolding commands arrive with milestone M4.");
        println!("Roadmap: https://github.com/JalapenoLabs/Anubis/milestones");
        return ExitCode::SUCCESS;
    };

    match command {
        Command::Roles { command } => run_roles(command),
        Command::Openapi { out } => run_openapi(out.as_deref()),
    }
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

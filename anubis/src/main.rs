//! The `anubis` command line interface.
//!
//! The CLI is the front door to the framework: stamping new applications, scaffolding
//! models and fields, and inspecting a running project. Subcommands land with milestone
//! M4 (see the repository's `docs/scaffolding.md` for the planned surface).

use clap::Parser;

/// The Anubis framework CLI.
#[derive(Debug, Parser)]
#[command(name = "anubis", version, about)]
struct Cli;

fn main() {
    Cli::parse();

    println!("Anubis {}", anubis::VERSION);
    println!("Scaffolding commands arrive with milestone M4.");
    println!("Roadmap: https://github.com/JalapenoLabs/Anubis/milestones");
}

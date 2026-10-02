mod load;
mod model;
mod size;
mod validate;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check a declaration without accessing any device
    Validate {
        /// Declaration file (.yaml, .yml or .json)
        file: PathBuf,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Validate { file } => {
            let decl = match load::load(&file) {
                Ok(decl) => decl,
                Err(e) => {
                    eprintln!("error: {}: {e}", file.display());
                    return ExitCode::FAILURE;
                }
            };
            let issues = validate::validate(&decl);
            for issue in &issues {
                eprintln!("error: {}: {issue}", file.display());
            }
            if issues.is_empty() {
                println!("{}: valid", file.display());
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use diskform::model::Declaration;
use diskform::{load, validate};

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

/// Loads and validates a declaration, reporting every problem.
fn checked(file: &Path) -> Option<Declaration> {
    let decl = match load::load(file) {
        Ok(decl) => decl,
        Err(e) => {
            eprintln!("error: {}: {e}", file.display());
            return None;
        }
    };
    let issues = validate::validate(&decl);
    for issue in &issues {
        eprintln!("error: {}: {issue}", file.display());
    }
    issues.is_empty().then_some(decl)
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Validate { file } => {
            if checked(&file).is_none() {
                return ExitCode::FAILURE;
            }
            println!("{}: valid", file.display());
            ExitCode::SUCCESS
        }
    }
}

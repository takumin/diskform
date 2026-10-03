use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use diskform::model::Declaration;
use diskform::{load, plan, system, validate};

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
    /// Show what apply would do, reading devices without changing them
    Plan {
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
        Command::Plan { file } => {
            let Some(decl) = checked(&file) else {
                return ExitCode::FAILURE;
            };
            let plan = plan::plan(&decl, &system::Host);
            print!("{plan}");
            for warning in &plan.warnings {
                eprintln!("warning: {}: {warning}", file.display());
            }
            for issue in &plan.issues {
                eprintln!("error: {}: {issue}", file.display());
            }
            if plan.issues.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{ArgGroup, Parser, Subcommand};

use diskform::destroy::{self, Targets};
use diskform::model::Declaration;
use diskform::system::{Host, System};
use diskform::{apply, load, mount, plan, validate};

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
    /// Create the declared storage on empty disks
    Apply {
        /// Declaration file (.yaml, .yml or .json)
        file: PathBuf,
        /// Do not ask for confirmation
        #[arg(long)]
        auto_approve: bool,
    },
    /// Open, activate and mount the configured storage under a root directory
    Mount {
        /// Declaration file (.yaml, .yml or .json)
        file: PathBuf,
        /// The directory that the mount points of the declaration are under
        #[arg(long, value_name = "DIR")]
        root: PathBuf,
    },
    /// Unmount, deactivate and close what mount put to use
    Unmount {
        /// Declaration file (.yaml, .yml or .json)
        file: PathBuf,
        /// The directory that the mount points of the declaration are under
        #[arg(long, value_name = "DIR")]
        root: PathBuf,
    },
    /// Erase the existing storage on the given disks of the declaration
    #[command(group(ArgGroup::new("targets").required(true).args(["target", "all"])))]
    Destroy {
        /// Declaration file (.yaml, .yml or .json)
        file: PathBuf,
        /// A disk to erase, such as `disk.data0`; may be repeated
        #[arg(long, value_name = "DISK")]
        target: Vec<String>,
        /// Erase every disk of the declaration
        #[arg(long)]
        all: bool,
        /// Do not ask for confirmation
        #[arg(long)]
        auto_approve: bool,
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
            let plan = plan::plan(&decl, &Host);
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
        Command::Apply { file, auto_approve } => apply_command(&file, auto_approve),
        Command::Mount { file, root } => mount_command(&file, &root, false),
        Command::Unmount { file, root } => mount_command(&file, &root, true),
        Command::Destroy {
            file,
            target,
            all,
            auto_approve,
        } => {
            let targets = if all {
                Targets::All
            } else {
                Targets::Disks(target)
            };
            destroy_command(&file, &targets, auto_approve)
        }
    }
}

fn report(file: &Path, warnings: &[String], issues: &[validate::Issue]) -> bool {
    for warning in warnings {
        eprintln!("warning: {}: {warning}", file.display());
    }
    for issue in issues {
        eprintln!("error: {}: {issue}", file.display());
    }
    issues.is_empty()
}

/// ADR 0004: asks the user, refusing when nobody can answer.
fn confirmed(auto_approve: bool, command: &str, question: &str) -> Result<bool, String> {
    if auto_approve {
        return Ok(true);
    }
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err(format!(
            "standard input is not a terminal, so {command} cannot ask for confirmation; \
             pass --auto-approve to {command} without it"
        ));
    }
    print!("{question} Only `yes` is accepted: ");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    let mut answer = String::new();
    stdin
        .lock()
        .read_line(&mut answer)
        .map_err(|e| e.to_string())?;
    Ok(answer.trim_end_matches(['\n', '\r']) == "yes")
}

fn destroy_command(file: &Path, targets: &Targets, auto_approve: bool) -> ExitCode {
    let Some(decl) = checked(file) else {
        return ExitCode::FAILURE;
    };
    let shown = destroy::destroy(&decl, &Host, targets);
    print!("{shown}");
    if !report(file, &shown.warnings, &shown.issues) {
        return ExitCode::FAILURE;
    }
    if shown.steps.is_empty() {
        return ExitCode::SUCCESS;
    }
    match confirmed(auto_approve, "destroy", "Erase the existing storage above?") {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("destroy cancelled; nothing was changed");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    }

    // ADR 0016: the disks may have changed while the user was answering.
    let now = destroy::destroy(&decl, &Host, targets);
    if !shown.same_as(&now) {
        report(file, &now.warnings, &now.issues);
        eprintln!(
            "error: the disks changed after the operations were shown, so nothing was \
             changed; run destroy again"
        );
        return ExitCode::FAILURE;
    }
    let mut done = |step: &destroy::Step| println!("done: {step}");
    match destroy::execute(&shown, &Host, &Host, &mut done) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("destroy stopped; the steps above it were done, and the disks are now:");
            for (name, disk) in &shown.disks {
                let state = Host
                    .disk_state(disk)
                    .map_or_else(|e| format!("unknown ({e})"), |s| destroy::describe(&s));
                eprintln!("  disk.{name} ({}): {state}", disk.path.display());
            }
            ExitCode::FAILURE
        }
    }
}

fn apply_command(file: &Path, auto_approve: bool) -> ExitCode {
    let Some(decl) = checked(file) else {
        return ExitCode::FAILURE;
    };
    let shown = plan::plan(&decl, &Host);
    print!("{shown}");
    if !report(file, &shown.warnings, &shown.issues) {
        eprintln!("apply refused; nothing was changed");
        return ExitCode::FAILURE;
    }
    if shown.operations.is_empty() {
        println!("nothing to apply");
        return ExitCode::SUCCESS;
    }
    match confirmed(auto_approve, "apply", "Perform the operations above?") {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("apply cancelled; nothing was changed");
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    }

    // ADR 0018: the disks may have changed while the user was answering.
    let now = plan::plan(&decl, &Host);
    if !shown.same_as(&now) {
        report(file, &now.warnings, &now.issues);
        eprintln!(
            "error: the disks changed after the operations were shown, so nothing was \
             changed; run apply again"
        );
        return ExitCode::FAILURE;
    }
    let mut done = |op: &plan::Operation| println!("done: {op}");
    match apply::execute(&decl, &shown, &Host, &Host, &mut done) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("apply stopped; the operations above it were done, and the disks are now:");
            for (name, disk) in &shown.disks {
                let state = Host
                    .disk_state(disk)
                    .map_or_else(|e| format!("unknown ({e})"), |s| destroy::describe(&s));
                eprintln!("  disk.{name} ({}): {state}", disk.path.display());
            }
            eprintln!(
                "apply refuses disks that it left partly configured; erase them with destroy \
                 before applying again (ADR 0011)"
            );
            ExitCode::FAILURE
        }
    }
}

fn mount_command(file: &Path, root: &Path, unmount: bool) -> ExitCode {
    let Some(decl) = checked(file) else {
        return ExitCode::FAILURE;
    };
    let mut warn = |w: String| eprintln!("warning: {}: {w}", file.display());
    let mut done = |step: String| println!("done: {step}");
    let result = if unmount {
        mount::unmount(&decl, &Host, &Host, root, &mut warn, &mut done).map(|changed| {
            if !changed {
                println!("nothing to unmount");
            }
        })
    } else {
        mount::mount(&decl, &Host, &Host, root, &mut warn, &mut done)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(issues) => {
            report(file, &[], &issues);
            let command = if unmount { "unmount" } else { "mount" };
            eprintln!("{command} stopped; the steps shown as done above were done");
            ExitCode::FAILURE
        }
    }
}

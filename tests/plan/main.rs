//! Plan-layer acceptance tests: run without privileges and never touch a
//! device (AGENTS.md 5).

mod destroy;
mod fake;
mod plan;
mod validate;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Outcome {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub fn run(args: &[&str], file: &Path) -> Outcome {
    let out = Command::new(env!("CARGO_BIN_EXE_diskform"))
        .args(args)
        .arg(file)
        .output()
        .expect("failed to run diskform");
    Outcome {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Writes `text` to a fresh file with the given extension.
pub fn write(ext: &str, text: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("plan-{n}.{ext}"));
    std::fs::write(&path, text).unwrap();
    path
}

/// The declarations under `examples/`.
pub fn examples() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path().join("diskform.yaml"))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    files
}

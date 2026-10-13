//! `mount` and `unmount` before they read any device (ADR 0004, ADR 0019).

use crate::{run, write};

#[test]
fn the_root_is_required() {
    let file = write("yaml", "version: 1\n");
    for command in ["mount", "unmount"] {
        let out = run(&[command], &file);
        assert!(!out.success, "{command} ran without --root");
        assert!(out.stderr.contains("--root"), "{}", out.stderr);
    }
}

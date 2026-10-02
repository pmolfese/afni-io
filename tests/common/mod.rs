//! Shared helpers for integration tests.
//!
//! Two kinds of fixture exist:
//!
//! * **Committed** fixtures under `tests/data/` (see `tests/data/README.md`).
//!   Always present; [`data`] panics if one is missing.
//! * **Reference** fixtures: large or non-public files (subject anatomy,
//!   unpublished results, live AFNI talk recordings) that stay outside the
//!   repo. Point `AFNI_IO_REFERENCE_DIR` at a directory holding them (for
//!   example `../sumaru/testing`). [`reference`] returns `None` when the
//!   variable is unset, so those tests skip; when it *is* set, a missing file
//!   is a hard failure rather than a silent skip.

#![allow(dead_code)]

use std::path::PathBuf;

/// Path to a committed fixture, relative to `tests/data/`.
pub fn data(relative: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(relative);
    assert!(
        path.exists(),
        "missing committed fixture {}",
        path.display()
    );
    path
}

/// Path to a reference fixture under `$AFNI_IO_REFERENCE_DIR`, or `None` (with
/// a note on stderr) when the variable is unset.
pub fn reference(relative: &str) -> Option<PathBuf> {
    let Some(root) = std::env::var_os("AFNI_IO_REFERENCE_DIR") else {
        eprintln!("skipping: AFNI_IO_REFERENCE_DIR is unset (wanted {relative})");
        return None;
    };
    let path = PathBuf::from(root).join(relative);
    assert!(
        path.exists(),
        "AFNI_IO_REFERENCE_DIR is set but {} is missing",
        path.display()
    );
    Some(path)
}

/// Parse a whitespace-separated numeric text dump (`3dmaskdump`,
/// `ConvertDset -o_1D_stdout`), skipping blank lines and `#` comments.
pub fn read_numeric_dump(relative: &str) -> Vec<Vec<f64>> {
    let text = std::fs::read_to_string(data(relative)).expect("read dump");
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            line.split_whitespace()
                .map(|value| value.parse().expect("numeric dump value"))
                .collect()
        })
        .collect()
}

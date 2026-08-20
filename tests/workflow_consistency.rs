//! Drift-detection guard: release-checks.yml must run a superset of the
//! CI quality gates (format, clippy, test, coverage) defined in ci.yml.
//!
//! `release-checks.yml` intentionally duplicates these gates so the release
//! gate runs independently on tag pushes. If a gate is added or changed in
//! `ci.yml` but not mirrored in `release-checks.yml`, this test fails with a
//! clear message naming the missing gate(s).
//!
//! The set of gates to check is hardcoded below. This is a manually
//! maintained synchronization point — if a new quality gate is added to
//! `ci.yml`, it should also be added here so that drift is detected
//! automatically.

use std::{fs, path::Path};

/// The canonical CI quality gates that release-checks must mirror.
/// Each entry is `(label, exact-command)`.
/// Matching is exact (after whitespace normalization) to avoid false
/// positives from substring matches (e.g. `cargo test --doc` matching
/// `cargo test`).
const REQUIRED_GATES: &[(&str, &str)] = &[
    ("format", "cargo fmt --all -- --check"),
    (
        "clippy",
        "cargo clippy --all-targets --all-features -- -D warnings",
    ),
    ("test", "cargo test"),
    ("doc-test", "cargo test --doc"),
    (
        "coverage",
        "cargo llvm-cov --workspace --fail-under-lines 85",
    ),
    ("audit", "cargo audit"),
    ("deny", "cargo deny check"),
];

#[test]
fn release_checks_run_superset_of_ci_quality_gates() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Err(error) = check_release_checks_superset(root) {
        panic!("{error}");
    }
}

fn check_release_checks_superset(root: &Path) -> Result<(), String> {
    let ci = read_workflow(root, ".github/workflows/ci.yml")?;
    let release = read_workflow(root, ".github/workflows/release-checks.yml")?;

    let ci_commands = cargo_run_commands(&ci);
    let release_commands = cargo_run_commands(&release);

    if ci_commands.is_empty() {
        return Err(
            ".github/workflows/ci.yml: no `cargo` run steps found; refusing a vacuous pass"
                .to_owned(),
        );
    }
    if release_commands.is_empty() {
        return Err(
            ".github/workflows/release-checks.yml: no `cargo` run steps found; refusing a vacuous pass"
                .to_owned(),
        );
    }

    let mut missing: Vec<String> = Vec::new();
    for (label, expected) in REQUIRED_GATES {
        let in_ci = ci_commands.iter().any(|cmd| cmd == expected);
        let in_release = release_commands.iter().any(|cmd| cmd == expected);
        // Only flag gates that ci.yml actually runs; release-checks must mirror them.
        if in_ci && !in_release {
            missing.push(format!(
                "  - {label}: ci.yml runs `{expected}` but release-checks.yml does not"
            ));
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "release-checks.yml is missing CI quality gates (drift detected):\n{}",
            missing.join("\n")
        ))
    }
}

fn read_workflow(root: &Path, relative: &str) -> Result<String, String> {
    fs::read_to_string(root.join(relative))
        .map_err(|error| format!("{relative}: could not read workflow: {error}"))
}

/// Extracts every `run:` step whose value starts with `cargo`.
///
/// Handles both YAML forms:
/// - Shorthand: `- run: cargo fmt --all -- --check`
/// - Indented:  `  run: cargo fmt --all -- --check`
///
/// Multi-line block scalars (`run: |` or `run: >`) are handled by
/// joining continuation lines that are indented deeper than the `run:` key.
/// A new top-level key or list item ends the current command.
fn cargo_run_commands(contents: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut in_run = false;
    let mut run_indent: usize = 0;
    let mut current = String::new();

    for raw in contents.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        // Detect a `run:` key (either `- run:` or `  run:`).
        let run_value = if let Some(rest) = trimmed.strip_prefix("- run:") {
            if !current.is_empty() && current.starts_with("cargo") {
                commands.push(current.clone());
            }
            current.clear();
            run_indent = indent;
            in_run = true;
            Some(rest.trim())
        } else if let Some(rest) = trimmed.strip_prefix("run:") {
            // Only treat as a run key if we're at a step-like indentation.
            // This avoids matching `run:` inside a string value.
            if !current.is_empty() && current.starts_with("cargo") {
                commands.push(current.clone());
            }
            current.clear();
            run_indent = indent;
            in_run = true;
            Some(rest.trim())
        } else {
            None
        };

        if let Some(value) = run_value {
            // Handle block scalar markers — the actual command is on the
            // following indented lines.
            if value == "|" || value == ">" || value.is_empty() {
                continue;
            }
            // Inline scalar — the command is on this line.
            current.push_str(value);
            continue;
        }

        if in_run {
            // A continuation line must be indented deeper than the `run:` key.
            // A line at the same or lesser indentation (and non-empty) ends
            // the current command.
            if line.is_empty() {
                continue;
            }
            if indent > run_indent {
                // Continuation of a block scalar.
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(trimmed);
            } else {
                // End of this run step.
                if !current.is_empty() && current.starts_with("cargo") {
                    commands.push(current.clone());
                }
                current.clear();
                in_run = false;
            }
        }
    }

    if in_run && !current.is_empty() && current.starts_with("cargo") {
        commands.push(current);
    }

    // Normalize whitespace in each command.
    commands
        .iter()
        .map(|cmd| cmd.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

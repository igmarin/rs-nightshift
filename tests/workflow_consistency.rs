//! Drift-detection guard: release-checks.yml must run a superset of the
//! CI quality gates (format, clippy, test, coverage) defined in ci.yml.
//!
//! `release-checks.yml` intentionally duplicates these gates so the release
//! gate runs independently on tag pushes. If a gate is added or changed in
//! `ci.yml` but not mirrored in `release-checks.yml`, this test fails with a
//! clear message naming the missing gate(s).

use std::{fs, path::Path};

/// The canonical CI quality gates that release-checks must mirror.
/// Each entry is `(label, command-substring-to-match)`.
const REQUIRED_GATES: &[(&str, &str)] = &[
    ("format", "cargo fmt --all -- --check"),
    (
        "clippy",
        "cargo clippy --all-targets --all-features -- -D warnings",
    ),
    ("test", "cargo test"),
    (
        "coverage",
        "cargo llvm-cov --workspace --fail-under-lines 85",
    ),
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
    for (label, needle) in REQUIRED_GATES {
        let in_ci = ci_commands.iter().any(|cmd| cmd.contains(needle));
        let in_release = release_commands.iter().any(|cmd| cmd.contains(needle));
        // Only flag gates that ci.yml actually runs; release-checks must mirror them.
        if in_ci && !in_release {
            missing.push(format!(
                "  - {label}: ci.yml runs `{needle}` but release-checks.yml does not"
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

/// Extracts the text of every `- run:` step whose value starts with `cargo`.
/// Handles multi-line scalar values by joining continuation lines.
fn cargo_run_commands(contents: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut in_run = false;
    let mut current = String::new();
    for raw in contents.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.starts_with("- run:") {
            // Flush any previous run step.
            if in_run && current.trim_start().starts_with("cargo") {
                commands.push(current.trim().to_owned());
            }
            current.clear();
            in_run = true;
            if let Some(value) = trimmed.strip_prefix("- run:") {
                current.push_str(value.trim());
            }
            continue;
        }
        if in_run {
            // A continuation line of a multi-line scalar is indented deeper
            // than the `- run:` key. A new top-level key ends the scalar.
            if !line.is_empty()
                && !raw.starts_with(' ')
                && !raw.starts_with('\t')
                && !raw.starts_with('-')
            {
                if current.trim_start().starts_with("cargo") {
                    commands.push(current.trim().to_owned());
                }
                current.clear();
                in_run = false;
            } else if !line.is_empty() && in_run {
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(trimmed);
            }
        }
    }
    if in_run && current.trim_start().starts_with("cargo") {
        commands.push(current.trim().to_owned());
    }
    commands
}

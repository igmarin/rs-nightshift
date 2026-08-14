//! Tech Lead stage: validated `02_tech_spec.md` grounded in context tools.

use crate::artifacts::RunDir;
use crate::context::{extract_paths, path_allowed, ContextBundle};
use crate::error::Error;
use crate::generate::{Generator, ROLE_TEMPERATURE};
use crate::models::{model_for, Role};
use crate::pm::has_atx_heading;
use std::path::PathBuf;

/// Required ATX headings in `02_tech_spec.md`.
pub const TECH_SPEC_HEADINGS: [&str; 4] = [
    "Impacted files",
    "Interfaces / signatures",
    "TDD plan",
    "Out of scope",
];

/// Artifact file written by the Tech Lead stage.
pub const TECH_SPEC_FILE: &str = "02_tech_spec.md";

/// Headings missing from `markdown`.
#[must_use]
pub fn missing_tech_spec_headings(markdown: &str) -> Vec<&'static str> {
    TECH_SPEC_HEADINGS
        .into_iter()
        .filter(|title| !has_atx_heading(markdown, title))
        .collect()
}

/// Paths listed under `## Impacted files`.
#[must_use]
pub fn impacted_files(markdown: &str) -> Vec<PathBuf> {
    extract_paths(&section_body(markdown, "Impacted files"))
}

fn section_body(markdown: &str, title: &str) -> String {
    let mut lines = markdown.lines();
    let mut out = String::new();
    let mut in_section = false;
    for line in lines.by_ref() {
        if has_atx_heading(line, title) {
            in_section = true;
            continue;
        }
        if in_section && line.trim_start().starts_with('#') {
            break;
        }
        if in_section {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Headings plus every impacted path must appear in `allowed`.
pub fn validate_tech_spec(markdown: &str, allowed: &[PathBuf]) -> Result<(), Error> {
    let missing = missing_tech_spec_headings(markdown);
    if !missing.is_empty() {
        return Err(Error::InvalidArtifact {
            artifact: TECH_SPEC_FILE,
            reason: format!("missing headings: {}", missing.join(", ")),
        });
    }
    let listed = impacted_files(markdown);
    if listed.is_empty() {
        return Err(Error::InvalidArtifact {
            artifact: TECH_SPEC_FILE,
            reason: "Impacted files listed no repo paths".into(),
        });
    }
    let unknown: Vec<_> = listed
        .iter()
        .filter(|path| !path_allowed(path, allowed))
        .cloned()
        .collect();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(Error::InvalidArtifact {
            artifact: TECH_SPEC_FILE,
            reason: format!(
                "impacted paths not in codegraph/graphify output: {}",
                unknown
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        })
    }
}

fn tech_lead_prompt(goal: &str, story: &str, context: &ContextBundle) -> String {
    format!(
        "You are the tech lead for one overnight engineering job.\n\
         Goal:\n{goal}\n\n\
         User story:\n{story}\n\n\
         Context from codegraph/graphify (do not invent files outside this list):\n\
         {}\n\n\
         Allowed files:\n{}\n\n\
         Write markdown with exactly these ATX headings:\n\
         ## Impacted files\n\
         ## Interfaces / signatures\n\
         ## TDD plan\n\
         ## Out of scope\n\
         List only allowed file paths under Impacted files.\n",
        truncate(&context.text, 12_000),
        context
            .files
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn repair_prompt(draft: &str, reason: &str, allowed: &[PathBuf]) -> String {
    format!(
        "Rewrite the tech spec. Problem: {reason}.\n\
         Allowed files:\n{}\n\
         Required headings: {}.\n\n\
         Original:\n{draft}\n",
        allowed
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        TECH_SPEC_HEADINGS.join(", "),
    )
}

fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        text
    } else {
        &text[..max]
    }
}

/// Generate, validate against context files, optionally repair once, write spec.
pub async fn write_tech_spec<G: Generator>(
    generator: &G,
    run: &RunDir,
    goal: &str,
    story: &str,
    context: &ContextBundle,
) -> Result<(), Error> {
    let draft = generator
        .generate(
            model_for(Role::TechLead),
            &tech_lead_prompt(goal, story, context),
            ROLE_TEMPERATURE,
        )
        .await?;
    let markdown = match validate_tech_spec(&draft, &context.files) {
        Ok(()) => draft,
        Err(error) => {
            let repaired = generator
                .generate(
                    model_for(Role::Router),
                    &repair_prompt(&draft, &error.to_string(), &context.files),
                    ROLE_TEMPERATURE,
                )
                .await?;
            validate_tech_spec(&repaired, &context.files)?;
            repaired
        }
    };
    std::fs::write(run.path.join(TECH_SPEC_FILE), markdown)?;
    Ok(())
}

/// Read the PM story from the run directory.
pub fn read_user_story(run: &RunDir) -> Result<String, Error> {
    std::fs::read_to_string(run.path.join(crate::pm::USER_STORY_FILE))
        .map_err(|e| Error::Artifact(format!("missing {}: {e}", crate::pm::USER_STORY_FILE)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::ArtifactStore;
    use crate::generate::ScriptedGenerator;

    fn complete_spec() -> String {
        r#"
## Impacted files
- src/cli.rs
- src/pipeline.rs

## Interfaces / signatures
run()

## TDD plan
1. failing CLI test

## Out of scope
Dev patch
"#
        .into()
    }

    fn allowed() -> Vec<PathBuf> {
        vec![
            PathBuf::from("src/cli.rs"),
            PathBuf::from("src/pipeline.rs"),
            PathBuf::from("src/pm.rs"),
        ]
    }

    #[test]
    fn complete_spec_is_valid() {
        validate_tech_spec(&complete_spec(), &allowed()).expect("valid");
    }

    #[test]
    fn unknown_impacted_file_is_rejected() {
        let md = r#"
## Impacted files
- src/secret.rs

## Interfaces / signatures
x

## TDD plan
y

## Out of scope
z
"#;
        let err = validate_tech_spec(md, &allowed()).expect_err("unknown");
        match err {
            Error::InvalidArtifact { reason, .. } => {
                assert!(reason.contains("src/secret.rs"), "{reason}");
            }
            other => panic!("expected InvalidArtifact, got {other:?}"),
        }
    }

    fn temp_run() -> (tempfile::TempDir, RunDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = ArtifactStore::new(tmp.path());
        let run = store.create_run("2026-08-14", "tl").expect("run");
        (tmp, run)
    }

    #[tokio::test]
    async fn writes_spec_when_model_stays_in_context() {
        let (_tmp, run) = temp_run();
        let gen = ScriptedGenerator::new();
        gen.push_text(complete_spec());
        let ctx = ContextBundle {
            text: "src/cli.rs src/pipeline.rs".into(),
            files: allowed(),
            warnings: Vec::new(),
        };
        write_tech_spec(&gen, &run, "goal", "story", &ctx)
            .await
            .expect("tl");
        assert!(run.path.join(TECH_SPEC_FILE).is_file());
        let calls = gen.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].model, model_for(Role::TechLead));
        assert!(calls[0].prompt.contains("goal"));
        assert!((calls[0].temperature - ROLE_TEMPERATURE).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn repairs_unknown_path_then_writes() {
        let (_tmp, run) = temp_run();
        let gen = ScriptedGenerator::new();
        gen.push_text("## Impacted files\n- src/secret.rs\n## Interfaces / signatures\n## TDD plan\n## Out of scope\n");
        gen.push_text(complete_spec());
        let ctx = ContextBundle {
            text: "ok".into(),
            files: allowed(),
            warnings: Vec::new(),
        };
        write_tech_spec(&gen, &run, "goal", "story", &ctx)
            .await
            .expect("repaired");
        assert_eq!(gen.calls()[1].model, model_for(Role::Router));
        assert!(run.path.join(TECH_SPEC_FILE).is_file());
    }

    #[tokio::test]
    async fn fails_when_repair_still_lists_unknown_path() {
        let (_tmp, run) = temp_run();
        let gen = ScriptedGenerator::new();
        gen.push_text("no headings");
        gen.push_text("still no headings");
        let ctx = ContextBundle {
            text: "ok".into(),
            files: allowed(),
            warnings: Vec::new(),
        };
        let err = write_tech_spec(&gen, &run, "g", "s", &ctx)
            .await
            .expect_err("invalid");
        assert!(matches!(err, Error::InvalidArtifact { .. }));
        assert!(!run.path.join(TECH_SPEC_FILE).exists());
    }
}

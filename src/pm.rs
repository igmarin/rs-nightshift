//! Product-manager stage: validated `01_user_story.md`.

use crate::artifacts::RunDir;
use crate::error::Error;
use crate::generate::{Generator, ROLE_TEMPERATURE};
use crate::models::{model_for, Role};

/// Required ATX headings in `01_user_story.md`.
pub const USER_STORY_HEADINGS: [&str; 4] = [
    "Problem Statement",
    "User Stories",
    "Acceptance Criteria",
    "Out of Scope",
];

/// Artifact file written by the PM stage.
pub const USER_STORY_FILE: &str = "01_user_story.md";

/// Headings from [`USER_STORY_HEADINGS`] that are not present as ATX headings.
#[must_use]
pub fn missing_user_story_headings(markdown: &str) -> Vec<&'static str> {
    USER_STORY_HEADINGS
        .into_iter()
        .filter(|title| !has_atx_heading(markdown, title))
        .collect()
}

pub(crate) fn has_atx_heading(markdown: &str, title: &str) -> bool {
    markdown.lines().any(|line| {
        let trimmed = line.trim();
        let rest = match trimmed.strip_prefix('#') {
            Some(rest) => rest.trim_start_matches('#').trim(),
            None => return false,
        };
        let rest = rest.trim_end_matches(':').trim();
        rest.eq_ignore_ascii_case(title)
    })
}

/// Fail if any required heading is missing.
pub fn validate_user_story(markdown: &str) -> Result<(), Error> {
    let missing = missing_user_story_headings(markdown);
    if missing.is_empty() {
        Ok(())
    } else {
        Err(Error::InvalidArtifact {
            artifact: USER_STORY_FILE,
            reason: format!("missing headings: {}", missing.join(", ")),
        })
    }
}

/// Prompt that asks the PM model for a four-section user story.
#[must_use]
pub fn pm_prompt(goal: &str) -> String {
    format!(
        "You are the product manager for one overnight engineering job.\n\
         Write markdown for this goal:\n\n{goal}\n\n\
         Use exactly these ATX headings:\n\
         ## Problem Statement\n\
         ## User Stories\n\
         ## Acceptance Criteria\n\
         ## Out of Scope\n"
    )
}

fn repair_prompt(draft: &str, missing: &[&str]) -> String {
    format!(
        "Rewrite the markdown so it contains these ATX headings: {}.\n\
         Missing now: {}.\n\n\
         Original:\n{draft}\n",
        USER_STORY_HEADINGS.join(", "),
        missing.join(", "),
    )
}

/// Generate, validate, optionally repair once, and write `01_user_story.md`.
pub async fn write_user_story<G: Generator>(
    generator: &G,
    run: &RunDir,
    goal: &str,
) -> Result<(), Error> {
    let draft = generator
        .generate(model_for(Role::Pm), &pm_prompt(goal), ROLE_TEMPERATURE)
        .await?;
    let markdown = match validate_user_story(&draft) {
        Ok(()) => draft,
        Err(_) => {
            let missing = missing_user_story_headings(&draft);
            let repaired = generator
                .generate(
                    model_for(Role::Router),
                    &repair_prompt(&draft, &missing),
                    ROLE_TEMPERATURE,
                )
                .await?;
            validate_user_story(&repaired)?;
            repaired
        }
    };
    std::fs::write(run.path.join(USER_STORY_FILE), markdown)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::ArtifactStore;
    use crate::generate::ScriptedGenerator;

    const COMPLETE: &str = r#"
# Overnight job

## Problem Statement
Operators need a scoped story.

## User Stories
As an operator I want out-of-scope listed.

## Acceptance Criteria
- Four headings exist

## Out of Scope
No auto-commit.
"#;

    #[test]
    fn complete_story_has_no_missing_headings() {
        assert!(missing_user_story_headings(COMPLETE).is_empty());
        validate_user_story(COMPLETE).expect("complete story");
    }

    #[test]
    fn missing_out_of_scope_is_reported() {
        let md = r#"
## Problem Statement
x

## User Stories
y

## Acceptance Criteria
z
"#;
        assert_eq!(missing_user_story_headings(md), ["Out of Scope"]);
        let err = validate_user_story(md).expect_err("incomplete");
        match err {
            Error::InvalidArtifact { artifact, reason } => {
                assert_eq!(artifact, USER_STORY_FILE);
                assert!(reason.contains("Out of Scope"), "{reason}");
            }
            other => panic!("expected InvalidArtifact, got {other:?}"),
        }
    }

    #[test]
    fn mention_without_heading_does_not_count() {
        let md = r#"
## Problem Statement
Out of Scope is discussed in prose.

## User Stories
n/a

## Acceptance Criteria
n/a
"#;
        assert_eq!(missing_user_story_headings(md), ["Out of Scope"]);
    }

    fn temp_run() -> (tempfile::TempDir, RunDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = ArtifactStore::new(tmp.path());
        let run = store.create_run("2026-08-14", "story").expect("run");
        (tmp, run)
    }

    #[tokio::test]
    async fn writes_story_when_pm_returns_all_headings() {
        let (_tmp, run) = temp_run();
        let gen = ScriptedGenerator::new();
        gen.push_text(COMPLETE);
        write_user_story(&gen, &run, "add status command")
            .await
            .expect("pm");
        let path = run.path.join(USER_STORY_FILE);
        let body = std::fs::read_to_string(&path).expect("story file");
        assert!(body.contains("## Out of Scope"), "{body}");
        let calls = gen.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].model, model_for(Role::Pm));
        assert!(calls[0].prompt.contains("add status command"));
        assert!((calls[0].temperature - ROLE_TEMPERATURE).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn repairs_once_with_router_then_writes() {
        let (_tmp, run) = temp_run();
        let gen = ScriptedGenerator::new();
        gen.push_text("## Problem Statement\nonly\n");
        gen.push_text(COMPLETE);
        write_user_story(&gen, &run, "goal")
            .await
            .expect("repaired");
        let calls = gen.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].model, model_for(Role::Pm));
        assert_eq!(calls[1].model, model_for(Role::Router));
        assert!(run.path.join(USER_STORY_FILE).is_file());
    }

    #[tokio::test]
    async fn fails_when_repair_still_invalid() {
        let (_tmp, run) = temp_run();
        let gen = ScriptedGenerator::new();
        gen.push_text("no headings");
        gen.push_text("still no headings");
        let err = write_user_story(&gen, &run, "goal")
            .await
            .expect_err("still invalid");
        match err {
            Error::InvalidArtifact { artifact, reason } => {
                assert_eq!(artifact, USER_STORY_FILE);
                assert!(reason.contains("missing headings"), "{reason}");
            }
            other => panic!("expected InvalidArtifact, got {other:?}"),
        }
        assert!(!run.path.join(USER_STORY_FILE).exists());
        assert_eq!(gen.calls().len(), 2);
    }
}

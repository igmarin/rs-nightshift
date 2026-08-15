//! Writer stage: `05_article_draft.md` after a passing run.

use crate::artifacts::RunDir;
use crate::error::Error;
use crate::generate::{Generator, WRITER_TEMPERATURE};
use crate::models::{model_for, Role};
use crate::pm::USER_STORY_FILE;
use crate::qa::QA_REPORT_FILE;
use crate::techlead::TECH_SPEC_FILE;

/// Artifact written by Writer.
pub const ARTICLE_FILE: &str = "05_article_draft.md";

fn writer_prompt(goal: &str, story: &str, spec: &str, qa: &str) -> String {
    format!(
        "Write a short markdown draft for operators about this overnight job.\n\
         Goal:\n{goal}\n\n\
         User story:\n{story}\n\n\
         Tech spec:\n{spec}\n\n\
         QA report:\n{qa}\n\n\
         Use headings. Do not claim the work was committed.\n"
    )
}

/// Generate and write `05_article_draft.md`.
pub async fn write_article<G: Generator>(
    generator: &G,
    run: &RunDir,
    goal: &str,
) -> Result<(), Error> {
    let story = std::fs::read_to_string(run.path.join(USER_STORY_FILE))?;
    let spec = std::fs::read_to_string(run.path.join(TECH_SPEC_FILE))?;
    let qa = std::fs::read_to_string(run.path.join(QA_REPORT_FILE))?;
    let draft = generator
        .generate(
            model_for(Role::Writer),
            &writer_prompt(goal, &story, &spec, &qa),
            WRITER_TEMPERATURE,
        )
        .await?;
    if draft.trim().is_empty() {
        return Err(Error::InvalidArtifact {
            artifact: ARTICLE_FILE,
            reason: "writer returned empty draft".into(),
        });
    }
    std::fs::write(run.path.join(ARTICLE_FILE), draft)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::ArtifactStore;
    use crate::generate::ScriptedGenerator;

    #[tokio::test]
    async fn writes_article_at_writer_temperature() {
        let tmp = tempfile::tempdir().expect("tmp");
        let run = ArtifactStore::new(tmp.path())
            .create_run("2026-08-14", "art")
            .expect("run");
        std::fs::write(run.path.join(USER_STORY_FILE), "story").expect("story");
        std::fs::write(run.path.join(TECH_SPEC_FILE), "spec").expect("spec");
        std::fs::write(run.path.join(QA_REPORT_FILE), "{}").expect("qa");
        let gen = ScriptedGenerator::new();
        gen.push_text("# Draft\nThe tree is dirty; nothing was committed.\n");
        write_article(&gen, &run, "greet").await.expect("writer");
        let body = std::fs::read_to_string(run.path.join(ARTICLE_FILE)).expect("file");
        assert!(body.contains("nothing was committed"));
        let calls = gen.calls();
        assert_eq!(calls[0].model, model_for(Role::Writer));
        assert!((calls[0].temperature - WRITER_TEMPERATURE).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn empty_draft_is_rejected() {
        let tmp = tempfile::tempdir().expect("tmp");
        let run = ArtifactStore::new(tmp.path())
            .create_run("2026-08-14", "empty")
            .expect("run");
        std::fs::write(run.path.join(USER_STORY_FILE), "story").expect("story");
        std::fs::write(run.path.join(TECH_SPEC_FILE), "spec").expect("spec");
        std::fs::write(run.path.join(QA_REPORT_FILE), "{}").expect("qa");
        let gen = ScriptedGenerator::new();
        gen.push_text("   \n");
        let err = write_article(&gen, &run, "x").await.expect_err("empty");
        assert!(matches!(err, Error::InvalidArtifact { .. }));
        assert!(!run.path.join(ARTICLE_FILE).exists());
    }
}

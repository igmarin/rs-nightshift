//! Role executor: run one role in the graph.

use crate::domain::rolegraph::config::RoleSpec;
use crate::domain::rolegraph::verdict::RoleOutput;
use crate::error::Error;
use crate::ports::{ArtifactStore, GenerateRequest, ModelClient};
use std::path::Path;

/// Default sampling temperature for role calls.
///
/// A per-role `options.temperature` overrides this at the adapter level, so
/// this is only the fallback.
pub const DEFAULT_TEMPERATURE: f32 = 0.2;

/// Standard instruction appended to a role's system prompt so the model returns
/// the verdict envelope the harness parses.
pub const OUTPUT_CONTRACT: &str = "Respond with a single JSON object and nothing else, \
matching this schema:
{
  \"verdict\": \"continue\" | \"issues\" | \"questions\" | \"done\" | \"fail\",
  \"summary\": \"one-line summary\",
  \"findings\": [\"…\"],
  \"questions\": [{\"text\": \"…\", \"blocking\": true}],
  \"block_reason\": \"none\" | \"ill_defined_task\" | \"tool_failure\" | \"version_mismatch\" | \"budget_exhausted\",
  \"content\": \"your deliverable (brief, patch, report, …)\"
}
Use \"continue\" to pass work on, \"issues\" to send findings back for a fix, \
\"questions\" to ask for clarification, \"done\" when finished, \"fail\" on a \
hard error. Put your deliverable in \"content\".";

/// Loop-back context carried into a role when a back-edge fires.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoleContext {
    /// The operator's goal for the whole run.
    pub goal: String,
    /// Findings from the previous role, to address (an `issues` back-edge).
    pub findings: Vec<String>,
    /// Clarifying questions from the previous role (a `questions` back-edge).
    pub questions: Vec<String>,
}

/// Outcome of executing one role.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecuteOutcome {
    /// The parsed verdict envelope.
    pub output: RoleOutput,
    /// The artifact file written, when the role declares an `output`.
    pub artifact: Option<String>,
}

/// Execute one role: render the prompt, call the client, parse the envelope,
/// and write the artifact.
pub async fn execute<C, A>(
    client: &C,
    store: &A,
    run: &Path,
    role: &RoleSpec,
    context: &RoleContext,
    artifacts: &[String],
) -> Result<ExecuteOutcome, Error>
where
    C: ModelClient + ?Sized,
    A: ArtifactStore,
{
    let mut contents = Vec::with_capacity(artifacts.len());
    for name in artifacts {
        let content = store.read_artifact(run, name)?;
        contents.push((name.clone(), content));
    }
    let text = client
        .generate(&GenerateRequest {
            model: role.model.clone(),
            system: Some(system_prompt(role)),
            prompt: user_prompt(context, &contents),
            temperature: DEFAULT_TEMPERATURE,
        })
        .await?;
    let output = parse_role_output(&text)?;
    let artifact = role.output.as_deref().map(|name| {
        // An empty deliverable is still written so the morning report shows the
        // artifact exists (and what the role produced).
        let _ = store.write_artifact(run, name, &output.content);
        name.to_string()
    });
    Ok(ExecuteOutcome { output, artifact })
}

/// Assemble the system prompt: the role's job plus the output contract.
fn system_prompt(role: &RoleSpec) -> String {
    let prompt = role.prompt.trim();
    if prompt.is_empty() {
        OUTPUT_CONTRACT.to_string()
    } else {
        format!("{prompt}\n\n{OUTPUT_CONTRACT}")
    }
}

/// Assemble the user prompt from the goal, loop-back context, and prior artifacts.
fn user_prompt(context: &RoleContext, artifacts: &[(String, String)]) -> String {
    let mut parts = vec![format!("Goal: {}", context.goal)];
    if !context.findings.is_empty() {
        let findings = context
            .findings
            .iter()
            .map(|finding| format!("- {finding}"))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("Findings to address:\n{findings}"));
    }
    if !context.questions.is_empty() {
        let questions = context
            .questions
            .iter()
            .map(|question| format!("- {question}"))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!("Clarifying questions:\n{questions}"));
    }
    if !artifacts.is_empty() {
        let block = artifacts
            .iter()
            .map(|(name, content)| format!("### {name}\n{content}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        parts.push(format!("Prior artifacts:\n\n{block}"));
    }
    parts.join("\n\n")
}

/// Parse a model reply into a [`RoleOutput`], tolerating markdown fences and
/// surrounding prose.
fn parse_role_output(text: &str) -> Result<RoleOutput, Error> {
    let json = extract_json_object(text)?;
    serde_json::from_str::<RoleOutput>(&json).map_err(|error| Error::InvalidArtifact {
        artifact: "role output",
        reason: format!("not a valid role envelope: {error}"),
    })
}

/// Extract the outermost JSON object from a model reply.
fn extract_json_object(text: &str) -> Result<String, Error> {
    let trimmed = text.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let stripped = stripped.strip_suffix("```").unwrap_or(stripped).trim();
    if serde_json::from_str::<serde_json::Value>(stripped).is_ok() {
        return Ok(stripped.to_string());
    }
    let start = stripped
        .find('{')
        .ok_or_else(|| invalid_envelope("model did not return a JSON object"))?;
    let end = stripped
        .rfind('}')
        .ok_or_else(|| invalid_envelope("model did not return a JSON object"))?;
    if end <= start {
        return Err(invalid_envelope("model did not return a JSON object"));
    }
    Ok(stripped[start..=end].to_string())
}

fn invalid_envelope(reason: &str) -> Error {
    Error::InvalidArtifact {
        artifact: "role output",
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::rolegraph::routing::Routing;
    use crate::domain::rolegraph::verdict::Verdict;
    use crate::ports::{MemoryArtifactStore, ScriptedModelClient};
    use std::collections::BTreeMap;

    fn role(output: Option<&str>) -> RoleSpec {
        RoleSpec {
            id: "developer".into(),
            provider: "kimi".into(),
            model: "kimi3".into(),
            options: BTreeMap::new(),
            prompt: "You are a developer.".into(),
            output: output.map(String::from),
            tools: Vec::new(),
            on: Routing::default(),
            max_loop: 3,
        }
    }

    fn context() -> RoleContext {
        RoleContext {
            goal: "add /health".into(),
            findings: Vec::new(),
            questions: Vec::new(),
        }
    }

    #[tokio::test]
    async fn execute_writes_artifact_and_returns_outcome() {
        let client = ScriptedModelClient::new();
        client.push_text(r#"{"verdict":"continue","content":"pub fn health() {}"}"#);
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");
        let outcome = execute(
            &client,
            &store,
            run,
            &role(Some("02_patch.patch")),
            &context(),
            &[],
        )
        .await
        .expect("execute");
        assert_eq!(outcome.output.verdict, Verdict::Continue);
        assert_eq!(outcome.artifact.as_deref(), Some("02_patch.patch"));
        assert_eq!(
            store
                .read_artifact(run, "02_patch.patch")
                .expect("artifact"),
            "pub fn health() {}"
        );
    }

    #[tokio::test]
    async fn execute_injects_goal_and_prior_artifacts() {
        let client = ScriptedModelClient::new();
        client.push_text(r#"{"verdict":"done","content":""}"#);
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");
        store
            .write_artifact(run, "01_brief.md", "the brief")
            .expect("write");
        execute(
            &client,
            &store,
            run,
            &role(None),
            &context(),
            &["01_brief.md".into()],
        )
        .await
        .expect("execute");
        let calls = client.calls();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].prompt.contains("add /health"),
            "{}",
            calls[0].prompt
        );
        assert!(calls[0].prompt.contains("the brief"), "{}", calls[0].prompt);
        assert!(calls[0]
            .system
            .as_deref()
            .unwrap_or("")
            .contains("You are a developer."));
    }

    #[tokio::test]
    async fn execute_parses_fenced_json() {
        let client = ScriptedModelClient::new();
        client.push_text("```json\n{\"verdict\":\"done\",\"content\":\"x\"}\n```");
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");
        let outcome = execute(&client, &store, run, &role(None), &context(), &[])
            .await
            .expect("execute");
        assert_eq!(outcome.output.verdict, Verdict::Done);
    }

    #[tokio::test]
    async fn execute_maps_invalid_output_to_error() {
        let client = ScriptedModelClient::new();
        client.push_text("no json here");
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");
        let err = execute(&client, &store, run, &role(None), &context(), &[])
            .await
            .expect_err("invalid output");
        assert!(err.to_string().contains("JSON"), "{err}");
    }

    #[test]
    fn system_prompt_appends_contract() {
        let system = system_prompt(&role(None));
        assert!(system.contains("You are a developer."));
        assert!(system.contains("verdict"));
    }

    #[test]
    fn user_prompt_renders_findings_and_questions() {
        let ctx = RoleContext {
            goal: "g".into(),
            findings: vec!["compile error".into()],
            questions: vec!["which port?".into()],
        };
        let prompt = user_prompt(&ctx, &[]);
        assert!(prompt.contains("Goal: g"), "{prompt}");
        assert!(prompt.contains("compile error"), "{prompt}");
        assert!(prompt.contains("which port?"), "{prompt}");
    }
}

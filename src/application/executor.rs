//! Role executor: run one role in the graph.

use crate::domain::rolegraph::config::RoleSpec;
use crate::domain::rolegraph::verdict::RoleOutput;
use crate::error::Error;
use crate::ports::{ArtifactStore, ContextProvider, GenerateRequest, ModelClient, ToolRunner};
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
    /// Human answers from a pre-flight Q&A round (the `plan` mode).
    pub clarifications: Vec<String>,
}

/// Outcome of executing one role.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecuteOutcome {
    /// The parsed verdict envelope.
    pub output: RoleOutput,
    /// The artifact file written, when the role declares an `output`.
    pub artifact: Option<String>,
}

/// Per-execution inputs (borrowed data; ports are passed separately).
pub struct ExecuteParams<'a> {
    /// Run directory (for reading prior artifacts and writing the output).
    pub run: &'a Path,
    /// Target repo (for capabilities like `run-tests` / `apply-patch`).
    pub repo: &'a Path,
    /// The role being executed.
    pub role: &'a RoleSpec,
    /// Goal + loop-back context.
    pub context: &'a RoleContext,
    /// Names of prior artifacts to inject into the prompt.
    pub artifacts: &'a [String],
}

/// Execute one role: run pre-tools, render the prompt, call the client, parse
/// the envelope, run post-tools, and write the artifact.
pub async fn execute<C, A, T, P>(
    client: &C,
    store: &A,
    tools: &T,
    context_provider: &P,
    params: &ExecuteParams<'_>,
) -> Result<ExecuteOutcome, Error>
where
    C: ModelClient + ?Sized,
    A: ArtifactStore,
    T: ToolRunner,
    P: ContextProvider,
{
    // Pre-tools: gather repo context and run the test suite, injecting their
    // output into the prompt so the role reasons over real results.
    let mut tool_output = String::new();
    for tool in &params.role.tools {
        match tool.as_str() {
            "gather-context" => {
                let text = context_provider
                    .gather(params.repo, &params.context.goal)
                    .await?;
                if !text.is_empty() {
                    tool_output.push_str(&format!("### repo context\n{text}\n\n"));
                }
            }
            "run-tests" => {
                let out = tools.run("run-tests", params.repo, "").await?;
                tool_output.push_str(&format!("### test results\n{out}\n\n"));
            }
            _ => {}
        }
    }

    let mut contents = Vec::with_capacity(params.artifacts.len());
    for name in params.artifacts {
        let content = store.read_artifact(params.run, name)?;
        contents.push((name.clone(), content));
    }
    let text = client
        .generate(&GenerateRequest {
            model: params.role.model.clone(),
            system: Some(system_prompt(params.role)),
            prompt: user_prompt(params.context, &contents, &tool_output),
            temperature: DEFAULT_TEMPERATURE,
        })
        .await?;
    let output = parse_role_output(&text)?;

    // Post-tools: apply the patch after the role produces its deliverable.
    for tool in &params.role.tools {
        if tool == "apply-patch" {
            tools
                .run("apply-patch", params.repo, &output.content)
                .await?;
        }
    }

    let artifact = params.role.output.as_deref().map(|name| {
        // An empty deliverable is still written so the morning report shows the
        // artifact exists (and what the role produced).
        let _ = store.write_artifact(params.run, name, &output.content);
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

/// Assemble the user prompt from the goal, pre-tool output, loop-back context,
/// and prior artifacts.
fn user_prompt(context: &RoleContext, artifacts: &[(String, String)], tool_output: &str) -> String {
    let mut parts = vec![format!("Goal: {}", context.goal)];
    if !tool_output.is_empty() {
        parts.push(format!("Tool output:\n{tool_output}"));
    }
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
    if !context.clarifications.is_empty() {
        let clarifications = context.clarifications.join("\n");
        parts.push(format!("Clarifications:\n{clarifications}"));
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
    use crate::ports::{
        MemoryArtifactStore, ScriptedModelClient, StubContextProvider, StubToolRunner,
    };
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

    fn role_with_tools(output: Option<&str>, tools: &[&str]) -> RoleSpec {
        let mut role = role(output);
        role.tools = tools.iter().map(|t| (*t).to_string()).collect();
        role
    }

    fn context() -> RoleContext {
        RoleContext {
            goal: "add /health".into(),
            findings: Vec::new(),
            questions: Vec::new(),
            clarifications: Vec::new(),
        }
    }

    fn params<'a>(
        run: &'a Path,
        role: &'a RoleSpec,
        context: &'a RoleContext,
    ) -> ExecuteParams<'a> {
        ExecuteParams {
            run,
            repo: Path::new("/repo"),
            role,
            context,
            artifacts: &[],
        }
    }

    #[tokio::test]
    async fn execute_writes_artifact_and_returns_outcome() {
        let client = ScriptedModelClient::new();
        client.push_text(r#"{"verdict":"continue","content":"pub fn health() {}"}"#);
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");
        let role = role(Some("02_patch.patch"));
        let ctx = context();
        let outcome = execute(
            &client,
            &store,
            &StubToolRunner::default(),
            &StubContextProvider::default(),
            &params(run, &role, &ctx),
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
        let role = role(None);
        let ctx = context();
        execute(
            &client,
            &store,
            &StubToolRunner::default(),
            &StubContextProvider::default(),
            &ExecuteParams {
                run,
                repo: Path::new("/repo"),
                role: &role,
                context: &ctx,
                artifacts: &["01_brief.md".into()],
            },
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
    }

    #[tokio::test]
    async fn execute_parses_fenced_json() {
        let client = ScriptedModelClient::new();
        client.push_text("```json\n{\"verdict\":\"done\",\"content\":\"x\"}\n```");
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");
        let role = role(None);
        let ctx = context();
        let outcome = execute(
            &client,
            &store,
            &StubToolRunner::default(),
            &StubContextProvider::default(),
            &params(run, &role, &ctx),
        )
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
        let role = role(None);
        let ctx = context();
        let err = execute(
            &client,
            &store,
            &StubToolRunner::default(),
            &StubContextProvider::default(),
            &params(run, &role, &ctx),
        )
        .await
        .expect_err("invalid output");
        assert!(err.to_string().contains("JSON"), "{err}");
    }

    #[tokio::test]
    async fn execute_injects_pre_tool_output() {
        let client = ScriptedModelClient::new();
        client.push_text(r#"{"verdict":"done","content":""}"#);
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");
        let role = role_with_tools(None, &["run-tests"]);
        let ctx = context();
        let tools = StubToolRunner::new("exit code: 0\nok");
        execute(
            &client,
            &store,
            &tools,
            &StubContextProvider::default(),
            &params(run, &role, &ctx),
        )
        .await
        .expect("execute");
        let calls = client.calls();
        assert!(
            calls[0].prompt.contains("Tool output"),
            "{}",
            calls[0].prompt
        );
        assert!(
            calls[0].prompt.contains("exit code: 0"),
            "{}",
            calls[0].prompt
        );
    }

    #[tokio::test]
    async fn execute_applies_patch_after_generation() {
        let client = ScriptedModelClient::new();
        client.push_text(r#"{"verdict":"continue","content":"--- patch ---"}"#);
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");
        let role = role_with_tools(Some("02_patch.patch"), &["apply-patch"]);
        let ctx = context();
        let tools = StubToolRunner::new("patch applied");
        execute(
            &client,
            &store,
            &tools,
            &StubContextProvider::default(),
            &params(run, &role, &ctx),
        )
        .await
        .expect("execute");
        let calls = tools.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "apply-patch");
        assert_eq!(calls[0].1, "--- patch ---");
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
            clarifications: vec![],
        };
        let prompt = user_prompt(&ctx, &[], "");
        assert!(prompt.contains("Goal: g"), "{prompt}");
        assert!(prompt.contains("compile error"), "{prompt}");
        assert!(prompt.contains("which port?"), "{prompt}");
    }

    #[test]
    fn user_prompt_renders_clarifications() {
        let ctx = RoleContext {
            goal: "g".into(),
            findings: vec![],
            questions: vec![],
            clarifications: vec!["Q: port?\nA: 8080".into()],
        };
        let prompt = user_prompt(&ctx, &[], "");
        assert!(prompt.contains("Clarifications:"), "{prompt}");
        assert!(prompt.contains("A: 8080"), "{prompt}");
    }
}

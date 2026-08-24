//! Fast harness-compatibility micro-benchmarks for a candidate model.
//!
//! [`run_bench`] is generic over [`ModelClient`] so unit tests inject
//! [`crate::ports::ScriptedModelClient`] and never talk to a live Ollama
//! server. The CLI edge builds an Ollama adapter and prints the report.

use crate::error::Error;
use crate::ports::{GenerateRequest, ModelClient};

/// Sampling temperature used for the three micro-tasks.
pub const BENCH_TEMPERATURE: f32 = 0.2;

/// Per-task generate timeout used by the live Ollama client (seconds).
///
/// CPU models are slow; 120s is enough for a short reply without waiting
/// the full 60-minute pipeline timeout. Documented on `nightshift bench --help`.
pub const BENCH_TIMEOUT_SECS: u64 = 120;

/// Default recommended `max_tokens` when JSON validity passes.
pub const DEFAULT_MAX_TOKENS: u32 = 2048;

/// Raised `max_tokens` when JSON validity fails (thinking models often
/// spend the budget on reasoning and never close a JSON object).
pub const RAISED_MAX_TOKENS: u32 = 4096;

/// Result of one micro-task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskResult {
    /// Stable task id (`json-validity`, `text-quoting`, `instruction-following`).
    pub name: String,
    /// Whether the model's reply met the task's check.
    pub passed: bool,
    /// Operator-facing reason (pass or fail).
    pub detail: String,
}

/// Full bench report: per-task scores plus recommended sampling settings.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchReport {
    /// Model tag that was probed.
    pub model: String,
    /// Ordered task results (JSON, quoting, instruction following).
    pub tasks: Vec<TaskResult>,
    /// Recommended `max_tokens` for a subsequent harness run.
    pub recommended_max_tokens: u32,
    /// Recommended sampling temperature for a subsequent harness run.
    pub recommended_temperature: f32,
}

impl BenchReport {
    /// Every micro-task passed.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        !self.tasks.is_empty() && self.tasks.iter().all(|task| task.passed)
    }
}

/// Render a human-readable report for stdout.
#[must_use]
pub fn render_report(report: &BenchReport) -> String {
    let mut lines = Vec::with_capacity(report.tasks.len() + 4);
    lines.push(format!("model: {}", report.model));
    for task in &report.tasks {
        let mark = if task.passed { "PASS" } else { "FAIL" };
        lines.push(format!("[{mark}] {} - {}", task.name, task.detail));
    }
    lines.push(format!(
        "recommended max_tokens: {}",
        report.recommended_max_tokens
    ));
    lines.push(format!(
        "recommended temperature: {}",
        report.recommended_temperature
    ));
    if !json_passed(&report.tasks) {
        lines.push(
            "note: JSON failure often means a thinking model spent the token budget on reasoning; raise max_tokens and avoid thinking variants."
                .into(),
        );
    }
    if report.all_passed() {
        lines.push("harness compatibility: pass".into());
    } else {
        lines.push("harness compatibility: fail".into());
    }
    lines.join("\n")
}

/// Run the three micro-tasks against `client` for `model`.
///
/// JSON validity uses a **simpler** check than the role executor: strip a
/// markdown fence if present, extract the first balanced `{…}` object, and
/// `serde_json`-parse it. It does not apply the executor's YAML / backtick /
/// truncation repairs (those live in `application::executor` and are owned
/// by issue #98). A thinking-only reply with no JSON object fails.
pub async fn run_bench(client: &dyn ModelClient, model: &str) -> Result<BenchReport, Error> {
    let json = run_json_validity(client, model).await?;
    let quoting = run_text_quoting(client, model).await?;
    let following = run_instruction_following(client, model).await?;
    let tasks = vec![json, quoting, following];
    let recommended_max_tokens = if json_passed(&tasks) {
        DEFAULT_MAX_TOKENS
    } else {
        RAISED_MAX_TOKENS
    };
    Ok(BenchReport {
        model: model.to_string(),
        tasks,
        recommended_max_tokens,
        recommended_temperature: BENCH_TEMPERATURE,
    })
}

fn json_passed(tasks: &[TaskResult]) -> bool {
    tasks
        .iter()
        .find(|task| task.name == "json-validity")
        .is_some_and(|task| task.passed)
}

async fn run_json_validity(client: &dyn ModelClient, model: &str) -> Result<TaskResult, Error> {
    let reply = generate(client, model, JSON_PROMPT).await?;
    Ok(score_json_validity(&reply))
}

async fn run_text_quoting(client: &dyn ModelClient, model: &str) -> Result<TaskResult, Error> {
    let reply = generate(client, model, QUOTE_PROMPT).await?;
    Ok(score_text_quoting(&reply))
}

async fn run_instruction_following(
    client: &dyn ModelClient,
    model: &str,
) -> Result<TaskResult, Error> {
    let reply = generate(client, model, SEARCH_REPLACE_PROMPT).await?;
    Ok(score_instruction_following(&reply))
}

async fn generate(client: &dyn ModelClient, model: &str, prompt: &str) -> Result<String, Error> {
    client
        .generate(&GenerateRequest {
            model: model.to_string(),
            system: None,
            prompt: prompt.to_string(),
            temperature: BENCH_TEMPERATURE,
        })
        .await
}

const JSON_PROMPT: &str =
    "Respond with a single JSON object and nothing else, matching this schema:\n\
{\"verdict\":\"done\",\"summary\":\"one-line summary\",\"content\":\"\"}\n\
Do not include markdown fences, reasoning, or any text outside the JSON object.";

/// Unique line the quoting task must echo exactly.
const QUOTE_LINE: &str = "    println!(\"hello from NS_BENCH_UNIQUE_LINE\");";

const QUOTE_PROMPT: &str = "Here is a short file snippet:\n\
\n\
fn greet() {\n\
    println!(\"hello from NS_BENCH_UNIQUE_LINE\");\n\
}\n\
\n\
Quote the unique line that contains NS_BENCH_UNIQUE_LINE.\n\
Reply with that exact line and nothing else.";

const SEARCH_REPLACE_PROMPT: &str =
    "Produce a search-replace pair using this exact format and nothing else:\n\
\n\
file: src/hello.rs\n\
old: println!(\"hello\");\n\
new: println!(\"hello world\");\n\
\n\
The file: / old: / new: headers must each appear on their own line.\n\
Do not write a file; only emit the formatted pair.";

/// Score a model reply for JSON validity.
///
/// Strips a leading/trailing markdown fence, extracts the first balanced
/// `{…}` object, and parses it with `serde_json`. Prose-only and think-only
/// replies (no `{`) fail.
fn score_json_validity(text: &str) -> TaskResult {
    match extract_json_object(text) {
        Some(json) => match serde_json::from_str::<serde_json::Value>(&json) {
            Ok(value) if value.is_object() => TaskResult {
                name: "json-validity".into(),
                passed: true,
                detail: "parsed a JSON object".into(),
            },
            Ok(_) => TaskResult {
                name: "json-validity".into(),
                passed: false,
                detail: "extracted JSON was not an object".into(),
            },
            Err(error) => TaskResult {
                name: "json-validity".into(),
                passed: false,
                detail: format!("extracted text is not valid JSON: {error}"),
            },
        },
        None => TaskResult {
            name: "json-validity".into(),
            passed: false,
            detail: "no JSON object in reply (thinking-only or non-JSON output)".into(),
        },
    }
}

/// Strip a markdown fence and return the first balanced `{…}` substring.
fn extract_json_object(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let stripped = stripped.strip_suffix("```").unwrap_or(stripped).trim();
    extract_balanced_object(stripped)
}

fn extract_balanced_object(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.iter().position(|&c| c == '{')?;
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &c) in chars.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            if c == '\\' {
                escaped = true;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            continue;
        }
        if c == '{' {
            depth += 1;
        } else if c == '}' {
            depth -= 1;
            if depth == 0 {
                return Some(chars[start..=i].iter().collect());
            }
        }
    }
    None
}

fn score_text_quoting(text: &str) -> TaskResult {
    let got = normalize_quoted_line(text);
    let expected = QUOTE_LINE.trim();
    if got == expected {
        TaskResult {
            name: "text-quoting".into(),
            passed: true,
            detail: "quoted the unique line exactly".into(),
        }
    } else {
        TaskResult {
            name: "text-quoting".into(),
            passed: false,
            detail: format!("reply did not match the unique line (got {got:?})"),
        }
    }
}

fn normalize_quoted_line(text: &str) -> String {
    let trimmed = text.trim();
    let stripped = trimmed
        .strip_prefix("```")
        .map(|rest| rest.trim_start_matches("rust").trim_start())
        .unwrap_or(trimmed);
    let stripped = stripped.strip_suffix("```").unwrap_or(stripped).trim();
    let stripped = stripped.trim_matches('`').trim();
    stripped.to_string()
}

/// Check `file:` / `old:` / `new:` headers are present with non-empty values.
///
/// This is a format check only — it does not write a file (INV-9: model
/// output is never used as argv).
fn score_instruction_following(text: &str) -> TaskResult {
    let file = header_value(text, "file:");
    let old = header_value(text, "old:");
    let new = header_value(text, "new:");
    match (file, old, new) {
        (Some(_), Some(_), Some(_)) => TaskResult {
            name: "instruction-following".into(),
            passed: true,
            detail: "emitted file:/old:/new: headers".into(),
        },
        _ => TaskResult {
            name: "instruction-following".into(),
            passed: false,
            detail: "missing file: / old: / new: search-replace format".into(),
        },
    }
}

fn header_value<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(prefix) {
            let rest = rest.strip_prefix(' ').unwrap_or(rest).trim();
            if !rest.is_empty() {
                return Some(rest);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::ScriptedModelClient;

    const ENVELOPE: &str = r#"{"verdict":"done","summary":"ok","content":"{}"}"#;

    #[tokio::test]
    async fn json_validity_passes_on_valid_envelope() {
        let client = ScriptedModelClient::new();
        client.push_text(ENVELOPE);
        client.push_text(QUOTE_LINE);
        client.push_text("file: src/hello.rs\nold: println!(\"hello\");\nnew: println!(\"hi\");\n");
        let report = run_bench(&client, "llama3.1:8b").await.expect("bench");
        let json = report
            .tasks
            .iter()
            .find(|t| t.name == "json-validity")
            .expect("json-validity task");
        assert!(json.passed, "{}", json.detail);
        assert!(render_report(&report).contains("[PASS] json-validity"));
        assert_eq!(report.recommended_max_tokens, DEFAULT_MAX_TOKENS);
        assert!((report.recommended_temperature - BENCH_TEMPERATURE).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn json_validity_fails_on_think_only_reply() {
        let client = ScriptedModelClient::new();
        client.push_text("<think>\nI should emit JSON after reasoning about the schema.\n</think>");
        client.push_text(QUOTE_LINE);
        client.push_text("file: src/hello.rs\nold: a\nnew: b\n");
        let report = run_bench(&client, "qwen3:4b").await.expect("bench");
        let json = report
            .tasks
            .iter()
            .find(|t| t.name == "json-validity")
            .expect("json-validity task");
        assert!(!json.passed, "{}", json.detail);
        assert!(json.detail.contains("no JSON object"), "{}", json.detail);
        assert_eq!(report.recommended_max_tokens, RAISED_MAX_TOKENS);
        let text = render_report(&report);
        assert!(text.contains("[FAIL] json-validity"), "{text}");
        assert!(text.contains("recommended max_tokens: 4096"), "{text}");
        assert!(text.contains("recommended temperature: 0.2"), "{text}");
        assert!(text.contains("avoid thinking"), "{text}");
    }

    #[tokio::test]
    async fn json_validity_fails_on_non_json_prose() {
        let client = ScriptedModelClient::new();
        client.push_text("Sure, here is a status: ok");
        client.push_text(QUOTE_LINE);
        client.push_text("file: src/hello.rs\nold: a\nnew: b\n");
        let report = run_bench(&client, "llama3.2:3b").await.expect("bench");
        let json = report
            .tasks
            .iter()
            .find(|t| t.name == "json-validity")
            .expect("json-validity task");
        assert!(!json.passed, "{}", json.detail);
    }

    #[tokio::test]
    async fn json_validity_passes_on_fenced_envelope() {
        let client = ScriptedModelClient::new();
        client.push_text(format!("```json\n{ENVELOPE}\n```"));
        client.push_text(QUOTE_LINE);
        client.push_text("file: src/hello.rs\nold: a\nnew: b\n");
        let report = run_bench(&client, "llama3.1:8b").await.expect("bench");
        let json = report
            .tasks
            .iter()
            .find(|t| t.name == "json-validity")
            .expect("json-validity task");
        assert!(json.passed, "{}", json.detail);
    }

    fn scripted_all_pass() -> ScriptedModelClient {
        let client = ScriptedModelClient::new();
        client.push_text(ENVELOPE);
        client.push_text(QUOTE_LINE);
        client.push_text("file: src/hello.rs\nold: println!(\"hello\");\nnew: println!(\"hi\");\n");
        client
    }

    #[tokio::test]
    async fn text_quoting_passes_on_exact_line() {
        let client = scripted_all_pass();
        let report = run_bench(&client, "llama3.1:8b").await.expect("bench");
        let quoting = report
            .tasks
            .iter()
            .find(|t| t.name == "text-quoting")
            .expect("text-quoting task");
        assert!(quoting.passed, "{}", quoting.detail);
        let calls = client.calls();
        assert_eq!(calls.len(), 3);
        assert!(
            calls[1].prompt.contains("NS_BENCH_UNIQUE_LINE"),
            "{}",
            calls[1].prompt
        );
        assert!((calls[0].temperature - BENCH_TEMPERATURE).abs() < f32::EPSILON);
        assert_eq!(calls[0].model, "llama3.1:8b");
    }

    #[tokio::test]
    async fn text_quoting_fails_on_placeholder() {
        let client = ScriptedModelClient::new();
        client.push_text(ENVELOPE);
        client.push_text("change from '...' to '...'");
        client.push_text("file: src/hello.rs\nold: a\nnew: b\n");
        let report = run_bench(&client, "llama3.2:3b").await.expect("bench");
        let quoting = report
            .tasks
            .iter()
            .find(|t| t.name == "text-quoting")
            .expect("text-quoting task");
        assert!(!quoting.passed, "{}", quoting.detail);
    }

    #[tokio::test]
    async fn instruction_following_passes_on_search_replace_format() {
        let client = scripted_all_pass();
        let report = run_bench(&client, "llama3.1:8b").await.expect("bench");
        let task = report
            .tasks
            .iter()
            .find(|t| t.name == "instruction-following")
            .expect("instruction-following task");
        assert!(task.passed, "{}", task.detail);
        assert!(report.all_passed());
        let text = render_report(&report);
        assert!(text.contains("[PASS] instruction-following"), "{text}");
        assert!(text.contains("recommended max_tokens: 2048"), "{text}");
        assert!(text.contains("recommended temperature: 0.2"), "{text}");
        assert!(text.contains("harness compatibility: pass"), "{text}");
    }

    #[tokio::test]
    async fn instruction_following_fails_when_headers_missing() {
        let client = ScriptedModelClient::new();
        client.push_text(ENVELOPE);
        client.push_text(QUOTE_LINE);
        client.push_text("I would replace hello with hi in src/hello.rs");
        let report = run_bench(&client, "llama3.2:3b").await.expect("bench");
        let task = report
            .tasks
            .iter()
            .find(|t| t.name == "instruction-following")
            .expect("instruction-following task");
        assert!(!task.passed, "{}", task.detail);
        assert!(!report.all_passed());
    }

    #[tokio::test]
    async fn instruction_following_fails_on_empty_headers() {
        let client = ScriptedModelClient::new();
        client.push_text(ENVELOPE);
        client.push_text(QUOTE_LINE);
        client.push_text("file: src/hello.rs\nold:\nnew: hi\n");
        let report = run_bench(&client, "llama3.2:3b").await.expect("bench");
        let task = report
            .tasks
            .iter()
            .find(|t| t.name == "instruction-following")
            .expect("instruction-following task");
        assert!(!task.passed, "{}", task.detail);
    }

    #[tokio::test]
    async fn json_validity_fails_on_truncated_object() {
        let client = ScriptedModelClient::new();
        client.push_text("{\"verdict\":\"done\",\"summary\":");
        client.push_text(QUOTE_LINE);
        client.push_text("file: src/hello.rs\nold: a\nnew: b\n");
        let report = run_bench(&client, "qwen3:4b").await.expect("bench");
        let json = report
            .tasks
            .iter()
            .find(|t| t.name == "json-validity")
            .expect("json-validity task");
        assert!(!json.passed, "{}", json.detail);
        assert_eq!(report.recommended_max_tokens, RAISED_MAX_TOKENS);
    }

    #[tokio::test]
    async fn generate_error_surfaces_without_scoring() {
        let client = ScriptedModelClient::new();
        client.push_err(Error::from(crate::error::ProviderError::Timeout));
        let err = run_bench(&client, "llama3.1:8b")
            .await
            .expect_err("timeout");
        assert!(matches!(
            err,
            Error::Provider(crate::error::ProviderError::Timeout)
        ));
    }
}

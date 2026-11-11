//! `nightshift bench`: fast harness-compatibility checks for a candidate model.
//!
//! Three micro-tasks, each repeated `runs` times, scored with the same parsers
//! a real run uses:
//!
//! 1. `json-validity`: the reply must parse as a role envelope
//!    ([`parse_role_output`], with its fence/think/repair handling).
//! 2. `text-quoting`: the reply must be two lines from a snippet, exact except
//!    that the first line may drop leading spaces (the same leeway `search-replace` gives,
//!    since `old:` is matched as a substring), because `old:` text must match
//!    exactly.
//! 3. `search-replace`: the reply must parse as one `file:`/`old:`/`new:` block
//!    ([`parse_search_replace`]) that, applied to the sample file, gives the
//!    expected result. Nothing is written to disk (INV-9).
//!
//! [`run_bench`] takes any [`ModelClient`], so tests inject a scripted client
//! and the CLI edge builds the real one through the provider factory.

use crate::adapters::capabilities::parse_search_replace;
use crate::application::executor::parse_role_output;
use crate::ports::{GenerateRequest, ModelClient};
use std::time::{Duration, Instant};

/// Sampling temperature for every bench call (a provider `temperature`
/// option, when configured, overrides it in the adapter).
pub const BENCH_TEMPERATURE: f32 = 0.2;

/// Default per-call timeout in seconds (`--timeout`).
pub const DEFAULT_BENCH_TIMEOUT_SECS: u64 = 120;

/// Default number of times each task runs (`--runs`).
pub const DEFAULT_BENCH_RUNS: u32 = 3;

/// Longest reply excerpt shown in a failure detail.
const PREVIEW_CHARS: usize = 80;

/// What to bench.
#[derive(Debug, Clone)]
pub struct BenchParams {
    /// Model tag sent to the provider.
    pub model: String,
    /// Times each task runs (at least 1).
    pub runs: u32,
    /// Per-call timeout.
    pub timeout: Duration,
}

/// Score for one task across all runs.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskScore {
    /// Stable task id.
    pub name: &'static str,
    /// Runs that passed.
    pub passed: u32,
    /// Wall-clock time of each call, in run order.
    pub latencies: Vec<Duration>,
    /// Why the most recent failing run failed.
    pub last_failure: Option<String>,
}

impl TaskScore {
    /// Every run passed.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        !self.latencies.is_empty() && self.passed as usize == self.latencies.len()
    }
}

/// Full bench result.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchReport {
    /// Model tag that was probed.
    pub model: String,
    /// One score per task, in task order.
    pub tasks: Vec<TaskScore>,
}

impl BenchReport {
    /// Every run of every task passed.
    #[must_use]
    pub fn all_passed(&self) -> bool {
        !self.tasks.is_empty() && self.tasks.iter().all(TaskScore::all_passed)
    }
}

type Scorer = fn(&str) -> Result<(), String>;

const TASKS: [(&str, &str, Scorer); 3] = [
    ("json-validity", JSON_PROMPT, score_json),
    ("text-quoting", QUOTE_PROMPT, score_quote),
    (
        "search-replace",
        SEARCH_REPLACE_PROMPT,
        score_search_replace,
    ),
];

/// Run every task `params.runs` times against `client`.
///
/// A failed generate call (timeout, server down) counts as a failed run with
/// the error as its detail, so one slow reply lowers the pass rate instead of
/// aborting the bench.
pub async fn run_bench(client: &dyn ModelClient, params: &BenchParams) -> BenchReport {
    let mut tasks: Vec<TaskScore> = TASKS
        .iter()
        .map(|(name, _, _)| TaskScore {
            name,
            passed: 0,
            latencies: Vec::new(),
            last_failure: None,
        })
        .collect();
    for _ in 0..params.runs.max(1) {
        for ((_, prompt, score), task) in TASKS.iter().zip(tasks.iter_mut()) {
            let request = GenerateRequest {
                model: params.model.clone(),
                system: None,
                prompt: (*prompt).to_string(),
                temperature: BENCH_TEMPERATURE,
                timeout: Some(params.timeout),
            };
            let started = Instant::now();
            let reply = client.generate(&request).await;
            task.latencies.push(started.elapsed());
            let outcome = match reply {
                Ok(text) => score(&text),
                Err(error) => Err(format!("generate failed: {error}")),
            };
            match outcome {
                Ok(()) => task.passed += 1,
                Err(reason) => task.last_failure = Some(reason),
            }
        }
    }
    BenchReport {
        model: params.model.clone(),
        tasks,
    }
}

/// Render the report for stdout.
#[must_use]
pub fn render_report(report: &BenchReport, origin: Option<&str>) -> String {
    let mut lines = Vec::with_capacity(report.tasks.len() + 4);
    match origin {
        Some(origin) => lines.push(format!("model: {} at {origin}", report.model)),
        None => lines.push(format!("model: {}", report.model)),
    }
    for task in &report.tasks {
        let mark = if task.all_passed() { "PASS" } else { "FAIL" };
        let runs = task.latencies.len();
        let mut line = format!(
            "[{mark}] {:<15} {}/{runs} passed  avg {}  max {}",
            task.name,
            task.passed,
            secs(average(&task.latencies)),
            secs(task.latencies.iter().max().copied().unwrap_or_default()),
        );
        if let Some(reason) = &task.last_failure {
            line.push_str(&format!("  last failure: {reason}"));
        }
        lines.push(line);
    }
    if report
        .tasks
        .iter()
        .any(|task| task.name == "json-validity" && !task.all_passed())
    {
        lines.push(
            "note: JSON failures often mean a thinking model spent max_tokens on reasoning; \
             raise max_tokens or use a non-thinking variant."
                .into(),
        );
    }
    let verdict = if report.all_passed() { "pass" } else { "fail" };
    lines.push(format!("harness compatibility: {verdict}"));
    lines.join("\n")
}

fn average(latencies: &[Duration]) -> Duration {
    match u32::try_from(latencies.len()) {
        Ok(n) if n > 0 => latencies.iter().sum::<Duration>() / n,
        _ => Duration::ZERO,
    }
}

fn secs(duration: Duration) -> String {
    format!("{:.1}s", duration.as_secs_f64())
}

fn preview(text: &str) -> String {
    let mut out: String = text.chars().take(PREVIEW_CHARS).collect();
    if text.chars().count() > PREVIEW_CHARS {
        out.push('…');
    }
    format!("{out:?}")
}

const JSON_PROMPT: &str =
    "Respond with a single JSON object and nothing else, matching this schema:\n\
{\"verdict\":\"done\",\"summary\":\"one-line summary\",\"content\":\"\"}\n\
Do not include markdown fences, reasoning, or any text outside the JSON object.";

/// The lines the quoting task must return. Indentation of the first line is
/// optional (substring match); the second line's must be exact.
const QUOTE_LINES: &str = "    if loud {\n        println!(\"HELLO from NS_BENCH_UNIQUE_LINE\");";

const QUOTE_PROMPT: &str = "Here is a short file snippet:\n\
\n\
fn greet(loud: bool) {\n\
\x20   if loud {\n\
\x20       println!(\"HELLO from NS_BENCH_UNIQUE_LINE\");\n\
\x20   }\n\
}\n\
\n\
Quote the two lines from `if loud {` through the line containing NS_BENCH_UNIQUE_LINE,\n\
exactly as they appear, keeping each line's leading spaces.\n\
Reply with those two lines and nothing else.";

const HELLO_PATH: &str = "src/hello.rs";
const HELLO_BEFORE: &str = "fn main() {\n    println!(\"hello\");\n}\n";
const HELLO_AFTER: &str = "fn main() {\n    println!(\"hello world\");\n}\n";

const SEARCH_REPLACE_PROMPT: &str = "This is src/hello.rs:\n\
\n\
fn main() {\n\
\x20   println!(\"hello\");\n\
}\n\
\n\
Change the printed text from \"hello\" to \"hello world\".\n\
Reply with exactly one search-replace block in this format and nothing else:\n\
\n\
file: <path>\n\
old: <exact existing text>\n\
new: <replacement text>";

fn score_json(text: &str) -> Result<(), String> {
    parse_role_output(text)
        .map(|_| ())
        .map_err(|error| format!("{error} (reply {})", preview(text)))
}

fn score_quote(text: &str) -> Result<(), String> {
    // One trailing newline is allowed; otherwise the reply must be a substring
    // of the snippet that ends the second line, which leaves only some of the
    // first line's leading spaces optional.
    let got = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    if got.trim_start() == QUOTE_LINES.trim_start() && QUOTE_LINES.ends_with(got) {
        Ok(())
    } else {
        Err(format!("not the exact lines (reply {})", preview(text)))
    }
}

fn score_search_replace(text: &str) -> Result<(), String> {
    let blocks =
        parse_search_replace(text).map_err(|error| format!("{error} (reply {})", preview(text)))?;
    let [block] = blocks.as_slice() else {
        return Err(format!("expected 1 block, got {}", blocks.len()));
    };
    if block.path != HELLO_PATH {
        return Err(format!("wrong file {:?}", block.path));
    }
    if HELLO_BEFORE.matches(block.old.as_str()).count() != 1 {
        return Err(format!(
            "old: text must match the file exactly once (old {})",
            preview(&block.old)
        ));
    }
    if HELLO_BEFORE.replacen(block.old.as_str(), &block.new, 1) == HELLO_AFTER {
        Ok(())
    } else {
        Err(format!(
            "applying the block does not give the expected file (new {})",
            preview(&block.new)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{Error, ProviderError};
    use crate::ports::ScriptedModelClient;

    const ENVELOPE: &str = r#"{"verdict":"done","summary":"ok","content":""}"#;
    const BLOCK: &str =
        "file: src/hello.rs\nold: println!(\"hello\");\nnew: println!(\"hello world\");\n";

    fn params(runs: u32) -> BenchParams {
        BenchParams {
            model: "qwen3.6-35b-4bit".into(),
            runs,
            timeout: Duration::from_secs(7),
        }
    }

    fn push_round(client: &ScriptedModelClient, json: &str, quote: &str, block: &str) {
        client.push_text(json);
        client.push_text(quote);
        client.push_text(block);
    }

    fn task<'a>(report: &'a BenchReport, name: &str) -> &'a TaskScore {
        report.tasks.iter().find(|t| t.name == name).expect("task")
    }

    #[tokio::test]
    async fn all_tasks_pass_and_requests_carry_model_and_timeout() {
        let client = ScriptedModelClient::new();
        push_round(&client, ENVELOPE, QUOTE_LINES, BLOCK);
        let report = run_bench(&client, &params(1)).await;
        assert!(report.all_passed(), "{report:?}");
        let calls = client.calls();
        assert_eq!(calls.len(), 3);
        assert!(calls
            .iter()
            .all(|c| c.model == "qwen3.6-35b-4bit" && c.timeout == Some(Duration::from_secs(7))));
        let text = render_report(&report, Some("http://127.0.0.1:8000"));
        assert!(text.contains("model: qwen3.6-35b-4bit at http://127.0.0.1:8000"));
        assert!(text.contains("[PASS] json-validity"));
        assert!(text.ends_with("harness compatibility: pass"));
    }

    #[tokio::test]
    async fn runs_repeat_each_task_and_report_a_pass_rate() {
        let client = ScriptedModelClient::new();
        push_round(&client, ENVELOPE, QUOTE_LINES, BLOCK);
        push_round(&client, "<think>still thinking", QUOTE_LINES, BLOCK);
        push_round(
            &client,
            &format!("```json\n{ENVELOPE}\n```"),
            QUOTE_LINES,
            BLOCK,
        );
        let report = run_bench(&client, &params(3)).await;
        assert_eq!(client.calls().len(), 9);
        let json = task(&report, "json-validity");
        assert_eq!((json.passed, json.latencies.len()), (2, 3));
        assert!(json.last_failure.is_some());
        assert!(!report.all_passed());
        let text = render_report(&report, None);
        assert!(text.contains("[FAIL] json-validity   2/3 passed"), "{text}");
        assert!(text.contains("note: JSON failures"));
        assert!(text.ends_with("harness compatibility: fail"));
    }

    #[tokio::test]
    async fn generate_error_counts_as_a_failed_run() {
        let client = ScriptedModelClient::new();
        client.push_err(Error::from(ProviderError::Ollama("timed out".into())));
        client.push_text(QUOTE_LINES);
        client.push_text(BLOCK);
        let report = run_bench(&client, &params(1)).await;
        let json = task(&report, "json-validity");
        assert_eq!(json.passed, 0);
        assert!(json
            .last_failure
            .as_deref()
            .is_some_and(|r| r.contains("generate failed") && r.contains("timed out")));
        assert!(task(&report, "text-quoting").all_passed());
    }

    #[test]
    fn json_rejects_prose_and_accepts_repairable_envelopes() {
        assert!(score_json(ENVELOPE).is_ok());
        assert!(score_json(&format!("<think>plan</think>\n{ENVELOPE}")).is_ok());
        assert!(score_json("Sure! Here is the answer.").is_err());
        assert!(score_json(r#"{"summary":"no verdict"}"#).is_err());
    }

    #[test]
    fn quote_must_keep_inner_indentation_and_nothing_else() {
        assert!(score_quote(QUOTE_LINES).is_ok());
        assert!(score_quote(&format!("{QUOTE_LINES}\n")).is_ok());
        assert!(score_quote(QUOTE_LINES.trim_start()).is_ok());
        let flattened = QUOTE_LINES.replace("\n        ", "\n");
        assert!(score_quote(&flattened).is_err());
        assert!(score_quote(&format!("`{QUOTE_LINES}`")).is_err());
        assert!(score_quote(&format!("```rust\n{QUOTE_LINES}\n```")).is_err());
        assert!(score_quote(&format!("Here it is:\n{QUOTE_LINES}")).is_err());
        assert!(score_quote(&format!("\n{QUOTE_LINES}")).is_err());
        assert!(score_quote(&format!("\t{}", QUOTE_LINES.trim_start())).is_err());
        assert!(score_quote(&format!("  {QUOTE_LINES}")).is_err());
        assert!(score_quote(&format!("{QUOTE_LINES}\n\n")).is_err());
        assert!(score_quote(&format!("{QUOTE_LINES}\r\n")).is_ok());
        assert!(score_quote(&QUOTE_LINES[2..]).is_ok());
    }

    #[test]
    fn search_replace_must_apply_cleanly_to_the_sample_file() {
        assert!(score_search_replace(BLOCK).is_ok());
        let indented = "file: src/hello.rs\nold:     println!(\"hello\");\nnew:     println!(\"hello world\");";
        assert!(score_search_replace(indented).is_ok());
        let cases = [
            ("Here you go:\n{BLOCK}", "stray"),
            ("file: src/main.rs\nold: println!(\"hello\");\nnew: println!(\"hello world\");", "wrong file"),
            ("file: src/hello.rs\nold: println!(\"helo\");\nnew: println!(\"hello world\");", "exactly once"),
            ("file: src/hello.rs\nold: println!(\"hello\");\nnew: println!(\"hello world\");\nDone!", "expected file"),
            ("file: src/hello.rs\nold: x\nnew: y\nfile: src/hello.rs\nold: a\nnew: b", "expected 1 block"),
            ("file:\nold:\nnew:", "search-replace"),
        ];
        for (reply, want) in cases {
            let reply = reply.replace("{BLOCK}", BLOCK);
            let err = score_search_replace(&reply).expect_err(&reply);
            assert!(err.contains(want), "{reply:?} -> {err}");
        }
    }

    #[test]
    fn preview_truncates_long_replies() {
        let long = "x".repeat(200);
        assert!(preview(&long).ends_with("…\""));
        assert_eq!(preview("ok"), "\"ok\"");
    }
}

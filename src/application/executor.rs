//! Role executor: run one role in the graph.

use crate::domain::rolegraph::config::{is_secret_path, RoleSpec};
use crate::domain::rolegraph::verdict::{RoleOutput, Verdict};
use crate::error::{ArtifactError, Error};
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
  \"content\": \"your deliverable as a plain string (brief, patch, report, …)\"
}
CRITICAL: \"content\" MUST be a JSON string — never an array or object. \
Escape newlines inside it as \\n. Use \"continue\" to pass work on, \"issues\" \
to send findings back for a fix, \"questions\" to ask for clarification, \
\"done\" when finished, \"fail\" on a hard error. Put your full deliverable \
text in \"content\".";

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
                // Suppress output that is only CodeGraph status noise with
                // no actual code findings — it confuses small models into
                // ignoring the output contract.
                if !text.is_empty() && !is_empty_context(&text) {
                    tool_output.push_str(&format!("### repo context\n{text}\n\n"));
                }
                // Inject raw file content for files the role declared
                // (non-code files that codegraph/graphify don't index).
                // Truncate at 64 KiB so large files don't exhaust the model's
                // context window or cause inference timeouts.
                const MAX_FILE_BYTES: usize = 64 * 1024;
                if !params.role.context_files.is_empty() {
                    let repo_root = params.repo.to_path_buf();
                    let context_files = params.role.context_files.clone();
                    let file_contents = tokio::task::spawn_blocking(move || {
                        read_context_files(&repo_root, &context_files, MAX_FILE_BYTES)
                    })
                    .await
                    .map_err(|error| Error::Context(format!("context_files join: {error}")))?;
                    for (file, result) in file_contents {
                        match result {
                            Ok(content) => {
                                tool_output.push_str(&format!(
                                    "### file: {file}\n<!-- begin file content -->\n{content}\n<!-- end file content -->\n\n"
                                ));
                            }
                            Err(warning) => {
                                tool_output.push_str(&format!("<!-- warning: {warning} -->\n\n"));
                            }
                        }
                    }
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
    let output = parse_role_output(&text).inspect_err(|_error| {
        // Write the raw model response to a debug artifact so the operator can
        // inspect what the model actually returned when parsing fails. Include
        // the role id in the filename so multiple failing roles don't overwrite
        // each other's evidence. Sanitize the role id to prevent path traversal.
        let safe_id = sanitize_filename(&params.role.id);
        let debug_name = format!("raw_response_{safe_id}.txt");
        if let Err(write_err) = store.write_artifact(params.run, &debug_name, &text) {
            eprintln!("warning: failed to write debug artifact {debug_name}: {write_err}");
        }
    })?;

    // Write the artifact first (even when empty) so a later tool failure still
    // leaves the deliverable on disk for inspection, and propagate write errors.
    let artifact = match params.role.output.as_deref() {
        Some(name) => {
            store.write_artifact(params.run, name, &output.content)?;
            Some(name.to_string())
        }
        None => None,
    };

    // Post-tools: apply the patch or write the file only for verdicts that
    // carry a deliverable.
    if matches!(output.verdict, Verdict::Continue | Verdict::Done) {
        for tool in &params.role.tools {
            if tool == "apply-patch" {
                tools
                    .run("apply-patch", params.repo, &output.content)
                    .await?;
            } else if tool == "write-file" {
                tools
                    .run("write-file", params.repo, &output.content)
                    .await?;
            }
        }
    }

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

/// Parse a model reply into a [`RoleOutput`], tolerating markdown fences,
/// surrounding prose, and common model JSON mistakes (literal newlines in
/// strings, non-string `content` fields, trailing commas).
fn parse_role_output(text: &str) -> Result<RoleOutput, Error> {
    let json = extract_json_object(text)?;
    // Fast path: strict parse.
    match serde_json::from_str::<RoleOutput>(&json) {
        Ok(output) => Ok(output),
        Err(_) => {
            // Slow path: parse as raw Value, coerce content to string, retry.
            let value = sanitize_json_value(&json)?;
            let repaired = serde_json::to_string(&value).unwrap_or_else(|_| json.clone());
            serde_json::from_str::<RoleOutput>(&repaired).map_err(|error| {
                ArtifactError::invalid("role output", format!("not a valid role envelope: {error}"))
                    .into()
            })
        }
    }
}

/// Extract the outermost JSON object from a model reply.
fn extract_json_object(text: &str) -> Result<String, Error> {
    let trimmed = text.trim();
    let stripped = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    let stripped = stripped.strip_suffix("```").unwrap_or(stripped).trim();
    // Repair YAML block scalars (| or >) that small models emit instead of
    // JSON strings for multi-line content fields.
    let repaired = repair_yaml_block_scalars(stripped);
    if serde_json::from_str::<serde_json::Value>(&repaired).is_ok() {
        return Ok(repaired);
    }
    // Repair backtick-delimited strings (JS template literals) that small
    // models emit instead of JSON strings.
    let backtick_fixed = repair_backtick_strings(&repaired);
    if serde_json::from_str::<serde_json::Value>(&backtick_fixed).is_ok() {
        return Ok(backtick_fixed);
    }
    // Try sanitizing raw newlines before giving up on the fenced block.
    let sanitized = sanitize_string_literals(&backtick_fixed);
    if serde_json::from_str::<serde_json::Value>(&sanitized).is_ok() {
        return Ok(sanitized);
    }
    // Try escaping unescaped quotes inside string values (common when models
    // embed HTML attributes like href="..." inside JSON content strings).
    let quote_fixed = escape_unescaped_quotes_in_strings(&sanitized);
    if serde_json::from_str::<serde_json::Value>(&quote_fixed).is_ok() {
        return Ok(quote_fixed);
    }
    // Try closing truncated JSON (model hit max_tokens before closing the
    // string and object). Append missing closing quotes and braces.
    let closed = close_truncated_json(&quote_fixed);
    if serde_json::from_str::<serde_json::Value>(&closed).is_ok() {
        return Ok(closed);
    }
    // Find the first `{` and track brace depth (respecting strings) to find
    // the matching `}`. This handles trailing text after the JSON object.
    // Return the extracted text even when it is still invalid; the caller
    // attempts value-level repair before failing.
    extract_balanced_object(&sanitized)
}

/// Find the first `{` and return the substring through the matching `}`,
/// tracking brace depth and skipping over string literals.
fn extract_balanced_object(text: &str) -> Result<String, Error> {
    let chars: Vec<char> = text.chars().collect();
    let start = chars
        .iter()
        .position(|&c| c == '{')
        .ok_or_else(|| invalid_envelope("model did not return a JSON object"))?;
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
                return Ok(chars[start..=i].iter().collect());
            }
        }
    }
    Err(invalid_envelope("model did not return a JSON object"))
}

/// Parse JSON into a `Value`, repairing non-string `content` fields.
/// The input has already been sanitized by `extract_json_object`, so no
/// second pass of `sanitize_string_literals` is needed here.
fn sanitize_json_value(json: &str) -> Result<serde_json::Value, Error> {
    let mut value: serde_json::Value = serde_json::from_str(json).map_err(|e| {
        invalid_envelope(&format!(
            "not a valid role envelope after repair: {e} (see raw_response_*.txt)"
        ))
    })?;
    // Coerce `content` to a string if the model put a non-string there.
    // Null is left as-is so the caller's `#[serde(default)]` produces an empty
    // string only when the field is absent, not when the model explicitly sent
    // null — that case is treated as a missing deliverable and rejected.
    if let Some(obj) = value.as_object_mut() {
        if let Some(content) = obj.get("content") {
            if !content.is_string() && !content.is_null() {
                let text = match content {
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Number(n) => n.to_string(),
                    other => other.to_string(),
                };
                obj.insert("content".into(), serde_json::Value::String(text));
            }
        }
    }
    Ok(value)
}

/// Replace literal control characters (anything below U+0020) inside JSON
/// string values with their escaped equivalents, and strip trailing commas
/// before closing braces/brackets. Leaves non-string content untouched.
fn sanitize_string_literals(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escaped = false;
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            if escaped {
                // Even in the escaped branch, a literal control character is
                // invalid JSON — escape it so the backslash precedes a valid
                // escape sequence rather than a raw byte.
                if (c as u32) < 0x20 {
                    out.push_str(&escape_control_char(c));
                } else {
                    out.push(c);
                }
                escaped = false;
                i += 1;
                continue;
            }
            if c == '\\' {
                out.push(c);
                escaped = true;
                i += 1;
                continue;
            }
            if c == '"' {
                out.push(c);
                in_string = false;
                i += 1;
                continue;
            }
            // Replace all literal control chars below U+0020 with escapes.
            if (c as u32) < 0x20 {
                out.push_str(&escape_control_char(c));
            } else {
                out.push(c);
            }
            i += 1;
        } else {
            if c == '"' {
                in_string = true;
                out.push(c);
                i += 1;
                continue;
            }
            // Strip trailing commas before } or ].
            if c == ',' {
                // Look ahead past whitespace.
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                    i += 1; // skip the comma
                    continue;
                }
            }
            out.push(c);
            i += 1;
        }
    }
    out
}

/// Return the JSON escape sequence for a control character below U+0020.
fn escape_control_char(c: char) -> String {
    match c {
        '\n' => "\\n".into(),
        '\r' => "\\r".into(),
        '\t' => "\\t".into(),
        '\u{08}' => "\\b".into(),
        '\u{0c}' => "\\f".into(),
        _ => format!("\\u{:04x}", c as u32),
    }
}

fn invalid_envelope(reason: &str) -> Error {
    ArtifactError::invalid("role output", reason.to_string()).into()
}

/// Repair YAML block scalars (`|` or `>`) that small models emit instead of
/// JSON strings for multi-line content fields.
///
/// Converts patterns like:
/// ```text
/// "content": |
///   ### Change 1
///   - **Current:** "old"
/// ```
/// to a proper JSON string:
/// ```text
/// "content": "### Change 1\n- **Current:** \"old\""
/// ```
fn repair_yaml_block_scalars(input: &str) -> String {
    // Only attempt repair if the input looks like it has a YAML block scalar.
    if !input.contains("\"content\": |") && !input.contains("\"content\": >") {
        return input.to_string();
    }
    let mut result = String::with_capacity(input.len());
    let lines: Vec<&str> = input.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        // Detect "content": | or "content": > (with optional trailing whitespace)
        let trimmed = line.trim_end();
        if trimmed.ends_with('|') || trimmed.ends_with('>') {
            // Check if this is a content field with a block scalar
            let before = trimmed[..trimmed.len() - 1].trim_end();
            if before.ends_with("\"content\":") {
                // Collect indented lines that follow
                let mut block_lines = Vec::new();
                let mut j = i + 1;
                while j < lines.len() {
                    let next = lines[j];
                    // Block scalar content is indented (starts with spaces)
                    // or is a blank line. Stop at the first non-indented,
                    // non-blank line.
                    if next.trim().is_empty() {
                        block_lines.push("");
                        j += 1;
                    } else if next.starts_with(' ') || next.starts_with('\t') {
                        // Strip the common indent (one level)
                        block_lines.push(next.trim_start());
                        j += 1;
                    } else {
                        break;
                    }
                }
                // Trim trailing blank lines
                while block_lines.last().is_some_and(|l| l.is_empty()) {
                    block_lines.pop();
                }
                // Join with \n and escape for JSON
                let content = block_lines.join("\n");
                let escaped = escape_json_string(&content);
                result.push_str("\"content\": \"");
                result.push_str(&escaped);
                result.push('"');
                i = j;
                continue;
            }
        }
        result.push_str(line);
        result.push('\n');
        i += 1;
    }
    result.trim_end().to_string()
}

/// Escape a string for use inside a JSON string value.
fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Heuristic repair: escape unescaped double quotes inside JSON string values.
///
/// Small models often embed HTML with attributes like `href="/about"` inside
/// JSON content strings without escaping the inner quotes. This function
/// walks the input tracking string boundaries (respecting `\"` escapes) and
/// when it detects an unescaped `"` that is followed by characters that don't
/// look like JSON structure (`,`, `}`, `]`, `:`, whitespace), it escapes it.
///
/// This is a best-effort heuristic — it may not fix all cases, but it handles
/// the common HTML-in-JSON pattern.
fn escape_unescaped_quotes_in_strings(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            if escaped {
                escaped = false;
                out.push(c);
                i += 1;
                continue;
            }
            if c == '\\' {
                escaped = true;
                out.push(c);
                i += 1;
                continue;
            }
            if c == '"' {
                // Check if this quote ends the string or is an unescaped
                // quote inside the string. If the next non-whitespace char
                // is a JSON structural char (, } ] :), it ends the string.
                // Otherwise, it's likely an unescaped quote (e.g., href=").
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                let next_structural =
                    matches!(chars.get(j), Some(',') | Some('}') | Some(']') | Some(':'));
                if next_structural {
                    in_string = false;
                    out.push(c);
                } else {
                    // Unescaped quote inside string — escape it
                    out.push_str("\\\"");
                }
                i += 1;
                continue;
            }
            out.push(c);
            i += 1;
            continue;
        }
        if c == '"' {
            in_string = true;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Repair backtick-delimited strings (JS template literals) that small models
/// emit instead of JSON strings. Converts:
/// ```text
/// "content": `
/// ## Heading
/// some text
/// `
/// ```
/// to a proper JSON string:
/// ```text
/// "content": "## Heading\nsome text\n"
/// ```
///
/// Only triggers when the input contains a backtick after a colon (the JSON
/// value position). Walks the input, and when it finds a backtick in a value
/// position, collects everything until the closing backtick, escapes it for
/// JSON, and wraps it in double quotes.
fn repair_backtick_strings(input: &str) -> String {
    // Fast path: no backticks means nothing to repair.
    if !input.contains('`') {
        return input.to_string();
    }
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Detect a backtick in a value position: preceded by `:` (with
        // optional whitespace). This avoids false positives from backticks
        // inside properly-quoted JSON strings.
        if c == '`' && backtick_in_value_position(&chars, i) {
            // Collect the backtick string content until the closing backtick.
            let mut content = String::new();
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '`' {
                content.push(chars[j]);
                j += 1;
            }
            // j is at the closing backtick (or end of input).
            let escaped = escape_json_string(content.trim());
            out.push('"');
            out.push_str(&escaped);
            out.push('"');
            if j < chars.len() {
                i = j + 1; // skip past the closing backtick
            } else {
                i = j; // end of input
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Check if the backtick at position `i` is in a JSON value position (preceded
/// by a colon, with optional whitespace between the colon and the backtick).
fn backtick_in_value_position(chars: &[char], i: usize) -> bool {
    let mut j = i;
    while j > 0 {
        j -= 1;
        let c = chars[j];
        if c.is_whitespace() {
            continue;
        }
        return c == ':';
    }
    false
}

/// Heuristic repair: close truncated JSON that was cut off by max_tokens.
///
/// Models sometimes hit the token limit before closing all strings and
/// objects. This function tracks open strings and braces, then appends
/// the missing closing characters.
fn close_truncated_json(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut in_string = false;
    let mut escaped = false;
    let mut depth: i32 = 0;
    for &c in &chars {
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
        } else if c == '{' || c == '[' {
            depth += 1;
        } else if c == '}' || c == ']' {
            depth -= 1;
        }
    }
    let mut result = input.to_string();
    if in_string {
        result.push('"');
    }
    while depth > 0 {
        result.push('}');
        depth -= 1;
    }
    result
}

/// Check if the CodeGraph output contains only status noise with no actual
/// code findings. Suppresses output like "No relevant code found" and
/// CodeGraph status tables for empty repos.
fn is_empty_context(text: &str) -> bool {
    text.contains("No relevant code found")
        && !text.contains("fn ")
        && !text.contains("struct ")
        && !text.contains("class ")
        && !text.contains("def ")
        && !text.contains("```")
}

/// Reduce a role id to a safe filename component: keep `[A-Za-z0-9_-]`,
/// replace everything else (including path separators) with `_`.
fn sanitize_filename(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Read `context_files` from `repo_root`, returning `(file, result)` pairs.
///
/// Each file is canonicalized and checked against the canonical repo root to
/// prevent symlink escapes. Content is truncated to `max_bytes` at a safe
/// UTF-8 char boundary. Errors become warnings (the run continues).
///
/// Only `max_bytes + 1` bytes are read from disk (via `Read::take`), so a
/// very large file does not exhaust memory before truncation is applied.
fn read_context_files(
    repo_root: &Path,
    files: &[String],
    max_bytes: usize,
) -> Vec<(String, Result<String, String>)> {
    let canonical_root = match repo_root.canonicalize() {
        Ok(p) => p,
        Err(error) => {
            return files
                .iter()
                .map(|f| {
                    (
                        f.clone(),
                        Err(format!(
                            "could not canonicalize repo root {}: {error}",
                            repo_root.display()
                        )),
                    )
                })
                .collect();
        }
    };

    let mut results = Vec::with_capacity(files.len());
    for file in files {
        let path = repo_root.join(file);
        let canonical = match path.canonicalize() {
            Ok(p) => p,
            Err(error) => {
                results.push((file.clone(), Err(format!("could not read {file}: {error}"))));
                continue;
            }
        };
        if !canonical.starts_with(&canonical_root) {
            results.push((
                file.clone(),
                Err(format!(
                    "context_files: {file} resolves outside the repo root (symlink escape)"
                )),
            ));
            continue;
        }
        // Re-check is_secret_path on the canonical relative path, so a
        // symlink inside the repo pointing to .env or .git/config is caught.
        let rel = canonical
            .strip_prefix(&canonical_root)
            .unwrap_or(&canonical);
        let rel_str = rel.to_string_lossy();
        if is_secret_path(&rel_str) {
            results.push((
                file.clone(),
                Err(format!(
                    "context_files: {file} resolves to secret-bearing path {rel_str} — refusing to inject"
                )),
            ));
            continue;
        }
        // Reject non-regular files (FIFOs, devices, directories, etc.).
        let metadata = match std::fs::symlink_metadata(&canonical) {
            Ok(m) => m,
            Err(error) => {
                results.push((file.clone(), Err(format!("could not read {file}: {error}"))));
                continue;
            }
        };
        if !metadata.is_file() {
            results.push((
                file.clone(),
                Err(format!(
                    "context_files: {file} is not a regular file — skipping"
                )),
            ));
            continue;
        }
        // Read at most max_bytes+1 bytes: the +1 lets us detect truncation
        // without loading the entire file into memory.
        let mut file_handle = match std::fs::File::open(&canonical) {
            Ok(f) => f,
            Err(error) => {
                results.push((file.clone(), Err(format!("could not read {file}: {error}"))));
                continue;
            }
        };
        use std::io::Read;
        let mut buf = Vec::with_capacity(max_bytes + 1);
        if let Err(error) = file_handle
            .by_ref()
            .take((max_bytes + 1) as u64)
            .read_to_end(&mut buf)
        {
            results.push((file.clone(), Err(format!("could not read {file}: {error}"))));
            continue;
        }
        let is_truncated = buf.len() > max_bytes;
        if is_truncated {
            // Truncate at a safe UTF-8 char boundary.
            let end = floor_char_boundary_bytes(&buf, max_bytes);
            buf.truncate(end);
            let content = String::from_utf8_lossy(&buf);
            results.push((
                file.clone(),
                Ok(format!(
                    "{content}\n<!-- truncated to UTF-8 boundary, ≤ {max_bytes} bytes -->"
                )),
            ));
        } else {
            let content = String::from_utf8_lossy(&buf);
            results.push((file.clone(), Ok(content.into_owned())));
        }
    }
    results
}

/// Find the largest byte index `<= max_bytes` that is a UTF-8 char boundary,
/// working on raw bytes (avoids needing a valid &str).
fn floor_char_boundary_bytes(buf: &[u8], mut max_bytes: usize) -> usize {
    if max_bytes >= buf.len() {
        return buf.len();
    }
    // A UTF-8 char boundary is a byte that is NOT a continuation byte
    // (continuation bytes have the top bits 10xxxxxx, i.e. 0x80..=0xBF).
    while max_bytes > 0 && (buf[max_bytes] & 0xC0) == 0x80 {
        max_bytes -= 1;
    }
    max_bytes
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
            context_files: Vec::new(),
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
    async fn execute_repairs_yaml_block_scalar_content() {
        let client = ScriptedModelClient::new();
        // Model emits YAML | block scalar instead of JSON string
        client.push_text(
            r#"{
  "verdict": "continue",
  "summary": "Brief",
  "findings": [],
  "questions": [],
  "block_reason": "none",
  "content": |
    ### Change 1: Headline
    - **Current:** "old"
    - **Proposed:** "new"
}"#,
        );
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");
        let role = role(Some("01_brief.md"));
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
        let artifact = store.read_artifact(run, "01_brief.md").expect("artifact");
        assert!(artifact.contains("### Change 1"), "{artifact}");
        assert!(artifact.contains("**Current:**"), "{artifact}");
    }

    #[tokio::test]
    async fn execute_repairs_unescaped_quotes_in_content() {
        let client = ScriptedModelClient::new();
        // Model embeds HTML with unescaped quotes in href attribute
        client.push_text(
            r#"{"verdict":"continue","summary":"Patch","findings":[],"questions":[],"block_reason":"none","content":"diff --git a/index.html b/index.html\n--- a/index.html\n+++ b/index.html\n@@ -1,6 +1,6 @@\n <html>\n <body>\n-<h1>Welcome</h1>\n+<h1>Hello</h1>\n <a href="/about">About</a>\n </body>\n </html>"}"#,
        );
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
        let artifact = store
            .read_artifact(run, "02_patch.patch")
            .expect("artifact");
        assert!(artifact.contains("href"), "{artifact}");
        assert!(artifact.contains("/about"), "{artifact}");
    }

    #[tokio::test]
    async fn execute_repairs_backtick_template_literal_content() {
        let client = ScriptedModelClient::new();
        // Model wraps JSON in ```json fence and uses backtick template literal
        // for the content field instead of a JSON string. This is the exact
        // failure pattern observed from llama3.2:3b on the no-ai-slop run.
        client.push_text("```json\n{\n  \"verdict\": \"continue\",\n  \"summary\": \"Rewrite brief\",\n  \"findings\": [],\n  \"questions\": [],\n  \"block_reason\": \"none\",\n  \"content\": `\n## Positioning statement\n\nOur website is built by a real engineer.\n\n## public/index.html\n- Hero: change to \"Building Real Software\"\n  `\n}\n```");
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");
        let role = role(Some("01_brief.md"));
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
        let artifact = store.read_artifact(run, "01_brief.md").expect("artifact");
        assert!(artifact.contains("Positioning statement"), "{artifact}");
        assert!(artifact.contains("Building Real Software"), "{artifact}");
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

    // --- Beta fix tests: JSON sanitization, content coercion, debug artifact ---

    #[test]
    fn parse_coerces_array_content_to_string() {
        // Model puts an array in content instead of a string.
        let raw = r#"{"verdict":"continue","content":[{"a":1,"b":2}]}"#;
        let output = parse_role_output(raw).expect("coerced");
        assert_eq!(output.verdict, Verdict::Continue);
        assert!(
            output.content.contains("\"a\":1"),
            "content should contain serialized array: {}",
            output.content
        );
    }

    #[test]
    fn parse_coerces_object_content_to_string() {
        let raw = r#"{"verdict":"done","content":{"note":"hello"}}"#;
        let output = parse_role_output(raw).expect("coerced");
        assert_eq!(output.verdict, Verdict::Done);
        assert!(
            output.content.contains("hello"),
            "content should contain the object text: {}",
            output.content
        );
    }

    #[test]
    fn parse_repairs_literal_newlines_in_strings() {
        // Model puts literal newlines inside the content string instead of \n.
        let raw = "{\"verdict\":\"done\",\"content\":\"line one\nline two\"}";
        let output = parse_role_output(raw).expect("repaired");
        assert_eq!(output.verdict, Verdict::Done);
        assert!(
            output.content.contains("line one"),
            "content should preserve text: {}",
            output.content
        );
        assert!(
            output.content.contains("line two"),
            "content should preserve text: {}",
            output.content
        );
    }

    #[test]
    fn parse_strips_trailing_commas() {
        let raw = r#"{"verdict":"done","content":"ok",}"#;
        let output = parse_role_output(raw).expect("trailing comma stripped");
        assert_eq!(output.verdict, Verdict::Done);
        assert_eq!(output.content, "ok");
    }

    #[test]
    fn parse_preserves_escaped_newlines_in_strings() {
        // Properly escaped \n should survive sanitization unchanged.
        let raw = r#"{"verdict":"done","content":"line1\nline2"}"#;
        let output = parse_role_output(raw).expect("valid json");
        assert_eq!(output.verdict, Verdict::Done);
        assert_eq!(output.content, "line1\nline2");
    }

    #[tokio::test]
    async fn execute_writes_raw_response_on_parse_failure() {
        let client = ScriptedModelClient::new();
        client.push_text("totally not json at all");
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/raw");
        let role = role(Some("01_brief.md"));
        let ctx = context();
        let err = execute(
            &client,
            &store,
            &StubToolRunner::default(),
            &StubContextProvider::default(),
            &params(run, &role, &ctx),
        )
        .await
        .expect_err("parse failure");
        assert!(err.to_string().contains("role output"), "{err}");
        // The raw response should be written for inspection, named with the role id.
        let raw = store
            .read_artifact(run, "raw_response_developer.txt")
            .expect("raw response written");
        assert!(raw.contains("totally not json"), "{raw}");
    }

    #[tokio::test]
    async fn execute_writes_raw_response_even_without_output() {
        // A role with no output file should still get a raw_response artifact
        // on parse failure — the debug evidence is independent of the artifact.
        let client = ScriptedModelClient::new();
        client.push_text("not json");
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/no-output");
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
        .expect_err("parse failure");
        assert!(err.to_string().contains("role output"), "{err}");
        let raw = store
            .read_artifact(run, "raw_response_developer.txt")
            .expect("raw response written even without output");
        assert!(raw.contains("not json"), "{raw}");
    }

    #[test]
    fn output_contract_says_content_must_be_string() {
        assert!(
            OUTPUT_CONTRACT.contains("MUST be a JSON string"),
            "contract should enforce string content"
        );
    }

    #[test]
    fn parse_handles_trailing_text_after_json() {
        // Model puts valid JSON followed by extra prose.
        let raw = r#"{"verdict":"done","content":"ok"} This is extra text after the JSON."#;
        let output = parse_role_output(raw).expect("trailing text ignored");
        assert_eq!(output.verdict, Verdict::Done);
        assert_eq!(output.content, "ok");
    }

    #[test]
    fn parse_handles_trailing_text_with_braces() {
        // Trailing text contains } which must not confuse extraction.
        let raw = r#"{"verdict":"done","content":"ok"} here is a } brace"#;
        let output = parse_role_output(raw).expect("balanced extraction");
        assert_eq!(output.verdict, Verdict::Done);
        assert_eq!(output.content, "ok");
    }

    #[test]
    fn parse_escapes_backslash_followed_by_control_char() {
        // Backslash followed by a literal newline inside a string — the
        // escaped branch must still escape the control character.
        let raw = "{\"verdict\":\"done\",\"content\":\"line\\\nnext\"}";
        let output = parse_role_output(raw).expect("escaped control char");
        assert_eq!(output.verdict, Verdict::Done);
        assert!(output.content.contains("line"), "{}", output.content);
        assert!(output.content.contains("next"), "{}", output.content);
    }

    #[test]
    fn parse_escapes_ansi_escape_inside_string() {
        // ANSI escape byte (0x1b) inside content — must be escaped, not emitted raw.
        let raw = "{\"verdict\":\"done\",\"content\":\"\u{1b}[31mred\u{1b}[0m\"}";
        let output = parse_role_output(raw).expect("ansi escaped");
        assert_eq!(output.verdict, Verdict::Done);
        assert!(output.content.contains("red"), "{}", output.content);
    }

    #[test]
    fn sanitize_filename_strips_path_separators() {
        assert_eq!(sanitize_filename("developer"), "developer");
        assert_eq!(sanitize_filename("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitize_filename("qa-role"), "qa-role");
        assert_eq!(sanitize_filename("a/b\\c"), "a_b_c");
    }

    #[test]
    fn parse_rejects_null_content() {
        // Model sends "content": null — should not be coerced to empty string.
        let raw = r#"{"verdict":"done","content":null}"#;
        let err = parse_role_output(raw).expect_err("null content rejected");
        assert!(err.to_string().contains("role output"), "{err}");
    }

    #[tokio::test]
    async fn execute_injects_context_files_into_prompt() {
        let repo = tempfile::tempdir().expect("repo");
        std::fs::write(repo.path().join("index.html"), "<h1>Hello World</h1>").expect("write html");

        let client = ScriptedModelClient::new();
        client.push_text(r#"{"verdict":"done","content":""}"#);
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");

        let mut role = role_with_tools(None, &["gather-context"]);
        role.context_files = vec!["index.html".to_string()];

        let ctx = context();
        execute(
            &client,
            &store,
            &StubToolRunner::default(),
            &StubContextProvider::new("graph: src/lib.rs"),
            &ExecuteParams {
                run,
                repo: repo.path(),
                role: &role,
                context: &ctx,
                artifacts: &[],
            },
        )
        .await
        .expect("execute");

        let calls = client.calls();
        assert!(
            calls[0].prompt.contains("### file: index.html"),
            "{}",
            calls[0].prompt
        );
        assert!(
            calls[0].prompt.contains("<h1>Hello World</h1>"),
            "{}",
            calls[0].prompt
        );
    }

    #[tokio::test]
    async fn execute_missing_context_file_warns_not_errors() {
        let repo = tempfile::tempdir().expect("repo");
        // No file created — the run should still succeed.

        let client = ScriptedModelClient::new();
        client.push_text(r#"{"verdict":"done","content":""}"#);
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");

        let mut role = role_with_tools(None, &["gather-context"]);
        role.context_files = vec!["nonexistent.html".to_string()];

        let ctx = context();
        let outcome = execute(
            &client,
            &store,
            &StubToolRunner::default(),
            &StubContextProvider::default(),
            &ExecuteParams {
                run,
                repo: repo.path(),
                role: &role,
                context: &ctx,
                artifacts: &[],
            },
        )
        .await
        .expect("execute should succeed despite missing file");

        assert_eq!(outcome.output.verdict, Verdict::Done);
        // The warning should appear in the prompt (reviewable), not just stderr.
        let calls = client.calls();
        assert!(
            calls[0].prompt.contains("warning"),
            "prompt should contain warning: {}",
            calls[0].prompt
        );
    }

    #[tokio::test]
    async fn execute_truncates_large_context_file() {
        let repo = tempfile::tempdir().expect("repo");
        // Create a file larger than 64 KiB.
        let big_content = "x".repeat(70 * 1024);
        std::fs::write(repo.path().join("big.html"), &big_content).expect("write");

        let client = ScriptedModelClient::new();
        client.push_text(r#"{"verdict":"done","content":""}"#);
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");

        let mut role = role_with_tools(None, &["gather-context"]);
        role.context_files = vec!["big.html".to_string()];

        let ctx = context();
        execute(
            &client,
            &store,
            &StubToolRunner::default(),
            &StubContextProvider::default(),
            &ExecuteParams {
                run,
                repo: repo.path(),
                role: &role,
                context: &ctx,
                artifacts: &[],
            },
        )
        .await
        .expect("execute");

        let calls = client.calls();
        assert!(
            calls[0].prompt.contains("truncated"),
            "prompt should mention truncation: {}",
            calls[0].prompt
        );
    }

    #[tokio::test]
    async fn execute_truncates_multibyte_utf8_without_panic() {
        let repo = tempfile::tempdir().expect("repo");
        // "中" is 3 bytes in UTF-8. 22000 chars = 66000 bytes, so the 65536-byte
        // cutoff lands mid-character. This must not panic.
        let big_content = "中".repeat(22000);
        std::fs::write(repo.path().join("multi.html"), &big_content).expect("write");

        let client = ScriptedModelClient::new();
        client.push_text(r#"{"verdict":"done","content":""}"#);
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");

        let mut role = role_with_tools(None, &["gather-context"]);
        role.context_files = vec!["multi.html".to_string()];

        let ctx = context();
        // Must not panic.
        execute(
            &client,
            &store,
            &StubToolRunner::default(),
            &StubContextProvider::default(),
            &ExecuteParams {
                run,
                repo: repo.path(),
                role: &role,
                context: &ctx,
                artifacts: &[],
            },
        )
        .await
        .expect("execute");

        let calls = client.calls();
        assert!(
            calls[0].prompt.contains("truncated"),
            "prompt should mention truncation: {}",
            calls[0].prompt
        );
    }

    #[tokio::test]
    async fn execute_rejects_symlink_escape_in_context_files() {
        let repo = tempfile::tempdir().expect("repo");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("secret.txt"), "TOP SECRET").expect("write secret");
        // Create a symlink inside the repo pointing outside.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                outside.path().join("secret.txt"),
                repo.path().join("link.html"),
            )
            .expect("symlink");
        }
        #[cfg(not(unix))]
        {
            // Skip on non-Unix; just write a normal file so the test passes.
            std::fs::write(repo.path().join("link.html"), "ok").expect("write");
        }

        let client = ScriptedModelClient::new();
        client.push_text(r#"{"verdict":"done","content":""}"#);
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");

        let mut role = role_with_tools(None, &["gather-context"]);
        role.context_files = vec!["link.html".to_string()];

        let ctx = context();
        execute(
            &client,
            &store,
            &StubToolRunner::default(),
            &StubContextProvider::default(),
            &ExecuteParams {
                run,
                repo: repo.path(),
                role: &role,
                context: &ctx,
                artifacts: &[],
            },
        )
        .await
        .expect("execute");

        let calls = client.calls();
        #[cfg(unix)]
        {
            // The symlink escape should be blocked — secret must NOT appear.
            assert!(
                !calls[0].prompt.contains("TOP SECRET"),
                "symlink escape leaked secret: {}",
                calls[0].prompt
            );
            assert!(
                calls[0].prompt.contains("warning"),
                "prompt should contain warning for symlink: {}",
                calls[0].prompt
            );
        }
        #[cfg(not(unix))]
        {
            // On non-Unix, the file is a regular file — content should appear.
            assert!(
                calls[0].prompt.contains("ok"),
                "prompt should contain file content: {}",
                calls[0].prompt
            );
        }
    }

    #[tokio::test]
    async fn execute_rejects_symlink_to_secret_inside_repo() {
        let repo = tempfile::tempdir().expect("repo");
        // Create a .env file inside the repo (not a secret path at config
        // validation level since the symlink name is "link.html").
        std::fs::write(
            repo.path().join(".env"),
            "NIGHTSHIFT_TEST_SECRET=p0ison-t0ken-value",
        )
        .expect("write .env");
        // Create a symlink inside the repo pointing to .env.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(repo.path().join(".env"), repo.path().join("link.html"))
                .expect("symlink");
        }
        #[cfg(not(unix))]
        {
            std::fs::write(repo.path().join("link.html"), "ok").expect("write");
        }

        let client = ScriptedModelClient::new();
        client.push_text(r#"{"verdict":"done","content":""}"#);
        let store = MemoryArtifactStore::default();
        let run = Path::new("/tmp/run/x");

        let mut role = role_with_tools(None, &["gather-context"]);
        role.context_files = vec!["link.html".to_string()];

        let ctx = context();
        execute(
            &client,
            &store,
            &StubToolRunner::default(),
            &StubContextProvider::default(),
            &ExecuteParams {
                run,
                repo: repo.path(),
                role: &role,
                context: &ctx,
                artifacts: &[],
            },
        )
        .await
        .expect("execute");

        let calls = client.calls();
        #[cfg(unix)]
        {
            // The symlink resolves to .env — is_secret_path on the canonical
            // relative path must block it. The sentinel value must NOT appear.
            assert!(
                !calls[0].prompt.contains("p0ison-t0ken-value"),
                "symlink to .env leaked secret: {}",
                calls[0].prompt
            );
            assert!(
                calls[0].prompt.contains("warning"),
                "prompt should contain warning for symlink-to-secret: {}",
                calls[0].prompt
            );
        }
        #[cfg(not(unix))]
        {
            assert!(
                calls[0].prompt.contains("ok"),
                "prompt should contain file content: {}",
                calls[0].prompt
            );
        }
    }

    #[test]
    fn floor_char_boundary_handles_multibyte() {
        // "é" is 2 bytes; "中" is 3 bytes.
        let s = "é中";
        let buf = s.as_bytes();
        assert_eq!(floor_char_boundary_bytes(buf, 0), 0);
        assert_eq!(floor_char_boundary_bytes(buf, 1), 0); // mid-é → back to 0
        assert_eq!(floor_char_boundary_bytes(buf, 2), 2); // end of é
        assert_eq!(floor_char_boundary_bytes(buf, 3), 2); // mid-中 → back to 2
        assert_eq!(floor_char_boundary_bytes(buf, 4), 2); // mid-中 → back to 2
        assert_eq!(floor_char_boundary_bytes(buf, 5), 5); // end of 中
        assert_eq!(floor_char_boundary_bytes(buf, 100), 5); // beyond len → len
    }
}

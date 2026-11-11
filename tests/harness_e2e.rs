//! End-to-end tests for the packaged `nightshift` binary.
//!
//! Each test starts a local OpenAI-compatible mock server, points a temp
//! config at it, and runs the real executable. This covers the wiring in
//! `main.rs` (config loading, profiles, provider factory, executor,
//! orchestrator, state files, report, exit code) that unit tests cannot
//! reach. No Ollama, `codegraph`, or `graphify` is needed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::{tempdir, TempDir};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Queue `content` as the next chat completion the server returns.
///
/// Mocks answer once each, in the order they are mounted.
async fn reply(server: &MockServer, content: &str) {
    let body = serde_json::json!({
        "id": "chatcmpl-x",
        "created": 1,
        "model": "fake",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop",
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    });
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .up_to_n_times(1)
        .mount(server)
        .await;
}

/// A role envelope with the given verdict and content.
fn envelope(verdict: &str, content: &str) -> String {
    serde_json::json!({"verdict": verdict, "summary": "ok", "content": content}).to_string()
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "user.name=Dev", "-c", "user.email=dev@example.com"])
        .args(args)
        .current_dir(repo)
        .output()
        .expect("spawn git");
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A git repo with one committed file, `hello.txt`.
fn repo_with_hello() -> TempDir {
    let repo = tempdir().expect("tempdir");
    git(repo.path(), &["init", "-q", "-b", "main"]);
    fs::write(repo.path().join("hello.txt"), "hello\n").expect("write");
    git(repo.path(), &["add", "-A"]);
    git(repo.path(), &["commit", "-q", "-m", "init"]);
    repo
}

/// Run the binary off the async runtime so the mock server keeps answering.
async fn nightshift(args: Vec<String>) -> Output {
    tokio::task::spawn_blocking(move || {
        Command::new(env!("CARGO_BIN_EXE_nightshift"))
            .args(args)
            .env_remove("NIGHTSHIFT_PROFILE")
            .output()
            .expect("spawn nightshift")
    })
    .await
    .expect("join")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The one dated run directory under `out` (skipping the `latest` link).
fn run_dir(out: &Path) -> PathBuf {
    fs::read_dir(out)
        .expect("out dir")
        .map(|entry| entry.expect("entry"))
        .find(|entry| entry.file_type().expect("type").is_dir())
        .expect("a run directory")
        .path()
}

fn write_config(dir: &Path, server: &MockServer, roles: &str) -> PathBuf {
    let config = dir.join("nightshift.toml");
    let body = format!(
        "[providers.fake]\nbackend = \"openai-compatible\"\nbase_url = \"{}/v1\"\n\n\
         [profiles.test]\ncoder = {{ provider = \"fake\", model = \"fake-coder\" }}\n\n{roles}",
        server.uri()
    );
    fs::write(&config, body).expect("write config");
    config
}

const THREE_ROLES: &str = r#"
[run]
start = "po"
max_steps = 6

[[roles]]
id = "po"
provider = "fake"
model = "fake-planner"
prompt = "You are the Product Owner."
output = "01_brief.md"
on = { continue = "dev" }

[[roles]]
id = "dev"
model = "@coder"
prompt = "You are the Developer."
output = "02_change.txt"
tools = ["search-replace"]
on = { continue = "qa" }

[[roles]]
id = "qa"
provider = "fake"
model = "fake-planner"
prompt = "You are QA."
output = "03_qa.md"
"#;

#[tokio::test(flavor = "multi_thread")]
async fn harness_runs_three_roles_retries_a_bad_reply_and_edits_the_repo() {
    let server = MockServer::start().await;
    reply(&server, &envelope("continue", "Change the greeting.")).await;
    reply(&server, "BANANA").await;
    reply(
        &server,
        &envelope("continue", "file: hello.txt\nold: hello\nnew: hello world"),
    )
    .await;
    reply(&server, &envelope("done", "Looks good.")).await;

    let work = tempdir().expect("tempdir");
    let repo = repo_with_hello();
    let out = work.path().join("artifacts");
    let config = write_config(work.path(), &server, THREE_ROLES);

    let output = nightshift(vec![
        "--profile".into(),
        "test".into(),
        "harness".into(),
        "--config".into(),
        config.display().to_string(),
        "--goal".into(),
        "Greet the world".into(),
        "--repo".into(),
        repo.path().display().to_string(),
        "--out".into(),
        out.display().to_string(),
    ])
    .await;
    let (stdout, stderr) = (text(&output.stdout), text(&output.stderr));
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("Status: PASSED"), "{stdout}");
    assert!(stdout.contains("po → dev → qa"), "{stdout}");
    // The `@coder` slot was resolved against `--profile test`.
    assert!(
        stderr.contains("profile test: po: fake/fake-planner, dev: fake/fake-coder"),
        "{stderr}"
    );

    // The edit landed, and the harness committed nothing (INV-3).
    let hello = fs::read_to_string(repo.path().join("hello.txt")).expect("read");
    assert_eq!(hello, "hello world\n");
    assert_eq!(
        git(repo.path(), &["rev-list", "--count", "HEAD"]).trim(),
        "1"
    );
    assert_eq!(
        git(repo.path(), &["status", "--porcelain"]).trim(),
        "M hello.txt"
    );

    // Run directory: artifacts, the rejected reply, the event log, the snapshot.
    let run = run_dir(&out);
    for artifact in ["01_brief.md", "02_change.txt", "03_qa.md"] {
        assert!(run.join(artifact).is_file(), "missing {artifact}");
    }
    let rejected = fs::read_to_string(run.join("raw_response_dev_1.txt")).expect("rejected reply");
    assert_eq!(rejected, "BANANA");
    let events: Vec<String> = fs::read_to_string(run.join("actions.jsonl"))
        .expect("actions")
        .lines()
        .map(|line| {
            let event: serde_json::Value = serde_json::from_str(line).expect("event JSON");
            format!(
                "{}:{}",
                event["event"].as_str().expect("event"),
                event["role"].as_str().expect("role")
            )
        })
        .collect();
    assert_eq!(
        events,
        [
            "role_start:po",
            "role_end:po",
            "role_start:dev",
            "retry:dev",
            "role_end:dev",
            "role_start:qa",
            "role_end:qa",
            "done:qa",
        ]
    );
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(run.join("state.json")).expect("state"))
            .expect("state JSON");
    assert_eq!(state["status"], "done");
    assert_eq!(state["steps"], 3);

    // The retry sent the rejection back to the model, on the slot's model.
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 4);
    let retry: serde_json::Value = serde_json::from_slice(&requests[2].body).expect("request JSON");
    assert_eq!(retry["model"], "fake-coder");
    let prompt = retry["messages"][1]["content"].as_str().expect("prompt");
    assert!(
        prompt.contains("Your previous reply was rejected"),
        "{prompt}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn harness_fails_with_a_failed_snapshot_when_retries_are_off() {
    let server = MockServer::start().await;
    reply(&server, "BANANA").await;

    let work = tempdir().expect("tempdir");
    let repo = repo_with_hello();
    let out = work.path().join("artifacts");
    let config = write_config(
        work.path(),
        &server,
        "[run]\nstart = \"po\"\n\n[[roles]]\nid = \"po\"\nprovider = \"fake\"\n\
         model = \"fake-planner\"\nretries = 0\n",
    );

    let output = nightshift(vec![
        "harness".into(),
        "--config".into(),
        config.display().to_string(),
        "--goal".into(),
        "Greet the world".into(),
        "--repo".into(),
        repo.path().display().to_string(),
        "--out".into(),
        out.display().to_string(),
    ])
    .await;
    assert!(!output.status.success(), "{}", text(&output.stdout));
    assert!(
        text(&output.stderr).contains("JSON"),
        "{}",
        text(&output.stderr)
    );

    let run = run_dir(&out);
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(run.join("state.json")).expect("state"))
            .expect("state JSON");
    assert_eq!(state["status"], "failed");
    let actions = fs::read_to_string(run.join("actions.jsonl")).expect("actions");
    assert!(!actions.contains("\"retry\""), "{actions}");
    assert_eq!(server.received_requests().await.expect("requests").len(), 1);
    assert_eq!(git(repo.path(), &["status", "--porcelain"]).trim(), "");
}

#[tokio::test(flavor = "multi_thread")]
async fn bench_passes_against_a_provider_from_the_config() {
    let server = MockServer::start().await;
    reply(&server, &envelope("done", "")).await;
    reply(&server, "not the quoted lines").await;
    reply(
        &server,
        &envelope(
            "continue",
            "file: src/hello.rs\nold: println!(\"hello\");\nnew: println!(\"hello world\");",
        ),
    )
    .await;

    let work = tempdir().expect("tempdir");
    let config = write_config(
        work.path(),
        &server,
        "[run]\nstart = \"po\"\n\n[[roles]]\nid = \"po\"\nprovider = \"fake\"\nmodel = \"fake-planner\"\n",
    );

    let output = nightshift(vec![
        "bench".into(),
        "--config".into(),
        config.display().to_string(),
        "--provider".into(),
        "fake".into(),
        "--model".into(),
        "fake-coder".into(),
        "--runs".into(),
        "1".into(),
    ])
    .await;
    let stdout = text(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        text(&output.stderr)
    );
    assert!(stdout.contains("[PASS] json-validity"), "{stdout}");
    assert!(stdout.contains("[WARN] text-quoting"), "{stdout}");
    assert!(stdout.contains("[PASS] search-replace"), "{stdout}");
    assert!(
        stdout.trim_end().ends_with("harness compatibility: pass"),
        "{stdout}"
    );
}

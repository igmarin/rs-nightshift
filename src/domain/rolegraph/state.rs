//! Run-state domain: the append-only action log and the status snapshot.
//!
//! Pure data with serde support so the state-store adapter can serialize to
//! JSONL (actions) and JSON (snapshot) without the domain depending on I/O.

use crate::domain::rolegraph::verdict::{BlockReason, Verdict};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Overall status of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// The run is still in progress.
    Running,
    /// The run finished successfully (the terminal role emitted `done`).
    Done,
    /// The run halted for human review (blocking questions or a loop cap).
    Blocked,
    /// The run failed (a role emitted `fail` or a tool failed unrecoverably).
    Failed,
}

/// Kind of an action-log event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A role began.
    RoleStart,
    /// One model call finished or failed; [`ActionEvent::call`] has its
    /// time and token counts.
    LlmCall,
    /// A declared capability ran.
    ToolCall,
    /// A role's reply was rejected and the role is trying again.
    Retry,
    /// A role ended.
    RoleEnd,
    /// A loop back-edge was taken.
    Loop,
    /// The run was resumed from its last checkpoint after an interruption.
    Resumed,
    /// A resume was refused. The run is left as it was; nothing continued.
    ResumeRefused,
    /// The run halted for human review.
    Halt,
    /// The run finished successfully.
    Done,
    /// The run failed.
    Fail,
}

/// One append-only action-log record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionEvent {
    /// ISO-8601 timestamp supplied by the caller via a clock port.
    pub ts: String,
    /// What happened.
    pub event: EventKind,
    /// Role id (empty for run-level events).
    pub role: String,
    /// Provider name (empty when this is not an LLM event).
    pub provider: String,
    /// Model tag (empty when this is not an LLM event).
    pub model: String,
    /// The verdict a role emitted, when this event records one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    /// Artifact file written, when relevant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact: Option<String>,
    /// Why the run halted/failed, when relevant.
    #[serde(default)]
    pub block_reason: BlockReason,
    /// Free-text detail (a `retry` event's attempt number and reason).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Time and token counts of one model call (an `llm_call` event).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<ModelCall>,
}

/// Time and token counts of one model call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCall {
    /// Wall-clock time of the call, in milliseconds.
    pub duration_ms: u64,
    /// Prompt tokens, when the server reported them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    /// Completion tokens, when the server reported them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
}

/// Checkpoint of where a run stands, rewritten before each role starts.
///
/// While `status` is `running`, `current_role` is the role the run runs next,
/// and the other fields are what that role starts with, so `--resume` can
/// continue from it. A finished run records the role that ended it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusSnapshot {
    /// The role to run next (or the role that ended the run), if any.
    pub current_role: Option<String>,
    /// Total role executions so far.
    pub steps: u32,
    /// The last verdict recorded, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verdict: Option<Verdict>,
    /// Overall status.
    pub status: RunStatus,
    /// Why the run halted/failed, when `status` is `blocked` or `failed`.
    #[serde(default)]
    pub block_reason: BlockReason,
    /// Fingerprint of the config the run started with, for resume validation.
    #[serde(default)]
    pub config_hash: String,
    /// Git HEAD of the target repo at the start of the run, for resume validation.
    pub repo_head: Option<String>,
    /// Per back-edge loop counts, keyed by `"role:target"`.
    #[serde(default)]
    pub loop_counters: BTreeMap<String, u32>,
    /// The operator's goal, so a resume needs no `--goal`.
    #[serde(default)]
    pub goal: String,
    /// Role output files written so far, injected into later prompts.
    #[serde(default)]
    pub artifacts: Vec<String>,
    /// Findings a loop-back carries into `current_role`.
    #[serde(default)]
    pub findings: Vec<String>,
    /// Clarifying questions a loop-back carries into `current_role`.
    #[serde(default)]
    pub questions: Vec<String>,
    /// The target working tree when `current_role` started. A resume continues
    /// only if the tree still matches it. `None` when the tree could not be
    /// read, or in a snapshot written before resume checked the tree.
    #[serde(default)]
    pub tree: Option<TreeState>,
}

/// The target repo's working tree at one instant.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TreeState {
    /// Fingerprint of the tree: `git status` output plus the diff against `HEAD`.
    pub fingerprint: String,
    /// The `git status --porcelain` lines, so a mismatch can name the files.
    #[serde(default)]
    pub status: Vec<String>,
}

/// FNV-1a (64-bit) over `parts`, each part preceded by its length so that
/// different splits of the same bytes hash differently. Stable across builds,
/// so a resume can compare it. It detects edits; it is not a cryptographic hash.
#[must_use]
pub fn fingerprint(parts: &[&[u8]]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0100_0000_01b3;
    let feed = |hash: u64, bytes: &[u8]| {
        bytes.iter().fold(hash, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
        })
    };
    let hash = parts.iter().fold(OFFSET, |hash, part| {
        let length = u64::try_from(part.len()).unwrap_or(u64::MAX);
        feed(feed(hash, &length.to_le_bytes()), part)
    });
    format!("{hash:016x}")
}

/// How many differing files a tree mismatch names before it counts the rest.
const NAMED_FILES: usize = 10;

/// Why the target tree no longer matches the checkpoint, or `None` when it does.
///
/// `current` is the tree now, or the reason it could not be read. A checkpoint
/// without a recorded tree never matches.
#[must_use]
pub fn tree_mismatch(
    recorded: Option<&TreeState>,
    current: Result<&TreeState, String>,
) -> Option<String> {
    let Some(recorded) = recorded else {
        return Some(
            "the run did not record the target tree state (it predates resume checks)".to_string(),
        );
    };
    let current = match current {
        Ok(current) => current,
        Err(reason) => return Some(format!("the target tree could not be read: {reason}")),
    };
    if recorded.fingerprint == current.fingerprint {
        return None;
    }
    let before: BTreeSet<&str> = recorded.status.iter().map(String::as_str).collect();
    let after: BTreeSet<&str> = current.status.iter().map(String::as_str).collect();
    let differing: Vec<&str> = before.symmetric_difference(&after).copied().collect();
    let detail = if differing.is_empty() {
        "the contents of files already changed in the tree differ".to_string()
    } else {
        let named = differing
            .iter()
            .take(NAMED_FILES)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        match differing.len().checked_sub(NAMED_FILES) {
            Some(more) if more > 0 => format!("files differ: {named} and {more} more"),
            _ => format!("files differ: {named}"),
        }
    };
    Some(format!(
        "the target tree changed since the checkpoint ({detail})"
    ))
}

impl StatusSnapshot {
    /// The config and repo state this run recorded when it started.
    #[must_use]
    pub fn identity(&self) -> RunIdentity {
        RunIdentity {
            config_hash: self.config_hash.clone(),
            repo_head: self.repo_head.clone(),
        }
    }
}

/// The config and target-repo state a run starts with. A resume must match it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunIdentity {
    /// Fingerprint of the config (and profile) the run started with.
    pub config_hash: String,
    /// The target repo's `HEAD` when the run started, if it has one.
    pub repo_head: Option<String>,
}

impl RunIdentity {
    /// One sentence per way `current` differs from this recorded identity.
    /// Empty when the run may continue.
    #[must_use]
    pub fn mismatches(&self, current: &RunIdentity) -> Vec<String> {
        let mut found = Vec::new();
        if self.config_hash.is_empty() {
            found.push(
                "the run did not record its config (it started before resume was supported)"
                    .to_string(),
            );
        } else if self.config_hash != current.config_hash {
            found.push(format!(
                "the config changed since the run started (recorded hash {}, now {})",
                self.config_hash, current.config_hash
            ));
        }
        if self.repo_head != current.repo_head {
            found.push(format!(
                "the target repo HEAD changed (recorded {}, now {})",
                self.repo_head.as_deref().unwrap_or("none"),
                current.repo_head.as_deref().unwrap_or("none")
            ));
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_event_round_trips_snake_case() {
        let event = ActionEvent {
            ts: "2026-08-19T00:00:00Z".into(),
            event: EventKind::RoleEnd,
            role: "qa".into(),
            provider: "ollama".into(),
            model: "phi4".into(),
            verdict: Some(Verdict::Issues),
            artifact: Some("03_qa_report.json".into()),
            block_reason: BlockReason::None,
            detail: None,
            call: None,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"event\":\"role_end\""), "{json}");
        assert!(json.contains("\"verdict\":\"issues\""), "{json}");
        let round: ActionEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(round, event);
    }

    #[test]
    fn action_event_omits_none_optionals() {
        let event = ActionEvent {
            ts: "t".into(),
            event: EventKind::Done,
            role: String::new(),
            provider: String::new(),
            model: String::new(),
            verdict: None,
            artifact: None,
            block_reason: BlockReason::None,
            detail: None,
            call: None,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(!json.contains("verdict"), "{json}");
        assert!(!json.contains("artifact"), "{json}");
        assert!(!json.contains("call"), "{json}");
    }

    #[test]
    fn llm_call_event_round_trips_with_and_without_tokens() {
        let mut event = ActionEvent {
            ts: "2026-11-12T00:00:00Z".into(),
            event: EventKind::LlmCall,
            role: "dev".into(),
            provider: "mlx".into(),
            model: "qwen3.6-35b-4bit".into(),
            verdict: None,
            artifact: None,
            block_reason: BlockReason::None,
            detail: None,
            call: Some(ModelCall {
                duration_ms: 1500,
                prompt_tokens: Some(900),
                completion_tokens: Some(120),
            }),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"event\":\"llm_call\""), "{json}");
        assert!(
            json.contains(
                "\"call\":{\"duration_ms\":1500,\"prompt_tokens\":900,\"completion_tokens\":120}"
            ),
            "{json}"
        );
        assert_eq!(
            serde_json::from_str::<ActionEvent>(&json).expect("parse"),
            event
        );

        // A server that reports no usage: only the time is written.
        event.call = Some(ModelCall {
            duration_ms: 1500,
            prompt_tokens: None,
            completion_tokens: None,
        });
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"call\":{\"duration_ms\":1500}"), "{json}");
        assert_eq!(
            serde_json::from_str::<ActionEvent>(&json).expect("parse"),
            event
        );
    }

    #[test]
    fn action_log_lines_without_call_still_parse() {
        let line = r#"{"ts":"t","event":"role_start","role":"po","provider":"mlx","model":"m","block_reason":"none"}"#;
        let event: ActionEvent = serde_json::from_str(line).expect("old line parses");
        assert_eq!(event.call, None);
    }

    #[test]
    fn status_snapshot_round_trips() {
        let snap = StatusSnapshot {
            current_role: Some("developer".into()),
            steps: 4,
            last_verdict: Some(Verdict::Continue),
            status: RunStatus::Running,
            block_reason: BlockReason::None,
            loop_counters: BTreeMap::from([("qa:developer".into(), 2)]),
            config_hash: "c1".into(),
            repo_head: Some("abc".into()),
            goal: "add /health".into(),
            artifacts: vec!["01_brief.md".into()],
            findings: vec!["fix the test".into()],
            questions: vec!["which port?".into()],
            tree: None,
        };
        let json = serde_json::to_string(&snap).expect("serialize");
        let round: StatusSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(round, snap);
    }

    #[test]
    fn status_snapshot_without_resume_fields_still_parses() {
        // A snapshot written before the resume fields existed.
        let json = r#"{"current_role":null,"steps":1,"status":"running","block_reason":"none"}"#;
        let snap: StatusSnapshot = serde_json::from_str(json).expect("old snapshot parses");
        assert_eq!(snap.config_hash, "");
        assert_eq!(snap.repo_head, None);
        assert!(snap.loop_counters.is_empty());
        assert_eq!(snap.goal, "");
        assert!(snap.artifacts.is_empty());
        assert!(snap.findings.is_empty());
        assert!(snap.questions.is_empty());
    }

    #[test]
    fn identity_matches_when_config_and_head_are_unchanged() {
        let recorded = RunIdentity {
            config_hash: "c1".into(),
            repo_head: Some("abc".into()),
        };
        assert!(recorded.mismatches(&recorded.clone()).is_empty());
    }

    #[test]
    fn identity_names_a_changed_config() {
        let recorded = RunIdentity {
            config_hash: "c1".into(),
            repo_head: Some("abc".into()),
        };
        let now = RunIdentity {
            config_hash: "c2".into(),
            repo_head: Some("abc".into()),
        };
        let found = recorded.mismatches(&now);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("config changed"), "{found:?}");
        assert!(
            found[0].contains("c1") && found[0].contains("c2"),
            "{found:?}"
        );
    }

    #[test]
    fn identity_names_a_moved_head_and_reports_both_changes() {
        let recorded = RunIdentity {
            config_hash: "c1".into(),
            repo_head: Some("abc".into()),
        };
        let now = RunIdentity {
            config_hash: "c2".into(),
            repo_head: None,
        };
        let found = recorded.mismatches(&now);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[1].contains("HEAD changed"), "{found:?}");
        assert!(
            found[1].contains("abc") && found[1].contains("none"),
            "{found:?}"
        );
    }

    #[test]
    fn identity_from_a_run_that_recorded_no_config_is_a_mismatch() {
        let recorded = RunIdentity::default();
        let now = RunIdentity {
            config_hash: "c1".into(),
            repo_head: None,
        };
        let found = recorded.mismatches(&now);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("did not record"), "{found:?}");
    }

    fn tree(fingerprint: &str, status: &[&str]) -> TreeState {
        TreeState {
            fingerprint: fingerprint.into(),
            status: status.iter().map(|line| line.to_string()).collect(),
        }
    }

    #[test]
    fn fingerprint_sees_every_part_and_where_each_part_ends() {
        let base = fingerprint(&[b"ab", b"c"]);
        assert_eq!(base, fingerprint(&[b"ab", b"c"]));
        assert_ne!(base, fingerprint(&[b"a", b"bc"]));
        assert_ne!(base, fingerprint(&[b"abc"]));
        assert_ne!(base, fingerprint(&[b"ab", b"c", b""]));
        assert_eq!(base.len(), 16, "{base}");
    }

    #[test]
    fn tree_matches_when_the_fingerprint_is_equal() {
        let clean = tree("f1", &[]);
        assert_eq!(tree_mismatch(Some(&clean), Ok(&clean)), None);
    }

    #[test]
    fn tree_names_the_files_that_differ() {
        let recorded = tree("f1", &["?? a.rs", " M b.rs"]);
        let now = tree("f2", &["?? a.rs", " M b.rs", "?? c.rs"]);
        let found = tree_mismatch(Some(&recorded), Ok(&now)).expect("mismatch");
        assert!(found.contains("target tree changed"), "{found}");
        assert!(found.contains("?? c.rs"), "{found}");
        assert!(!found.contains("?? a.rs"), "{found}");
    }

    #[test]
    fn tree_names_at_most_ten_files_and_counts_the_rest() {
        let recorded = tree("f1", &[]);
        let names: Vec<String> = (0..12).map(|i| format!("?? f{i:02}.rs")).collect();
        let now = TreeState {
            fingerprint: "f2".into(),
            status: names,
        };
        let found = tree_mismatch(Some(&recorded), Ok(&now)).expect("mismatch");
        assert!(found.contains("?? f09.rs"), "{found}");
        assert!(!found.contains("?? f10.rs"), "{found}");
        assert!(found.contains("and 2 more"), "{found}");
    }

    #[test]
    fn tree_says_so_when_only_already_modified_contents_changed() {
        let recorded = tree("f1", &[" M b.rs"]);
        let now = tree("f2", &[" M b.rs"]);
        let found = tree_mismatch(Some(&recorded), Ok(&now)).expect("mismatch");
        assert!(found.contains("contents"), "{found}");
    }

    #[test]
    fn tree_refuses_a_checkpoint_that_recorded_no_tree() {
        let now = tree("f1", &[]);
        let found = tree_mismatch(None, Ok(&now)).expect("mismatch");
        assert!(found.contains("did not record the target tree"), "{found}");
    }

    #[test]
    fn tree_refuses_when_the_tree_cannot_be_read_now() {
        let recorded = tree("f1", &[]);
        let found =
            tree_mismatch(Some(&recorded), Err("git status failed".to_string())).expect("mismatch");
        assert!(found.contains("could not be read"), "{found}");
        assert!(found.contains("git status failed"), "{found}");
    }

    #[test]
    fn old_snapshot_has_no_tree_and_a_tree_round_trips() {
        let json = r#"{"current_role":null,"steps":1,"status":"running","block_reason":"none","repo_head":null}"#;
        let snap: StatusSnapshot = serde_json::from_str(json).expect("old snapshot parses");
        assert_eq!(snap.tree, None);

        let recorded = tree("f1", &["?? a.rs"]);
        let with_tree = StatusSnapshot {
            tree: Some(recorded.clone()),
            ..snap
        };
        let round: StatusSnapshot =
            serde_json::from_str(&serde_json::to_string(&with_tree).expect("serialize"))
                .expect("deserialize");
        assert_eq!(round.tree, Some(recorded));
    }
}

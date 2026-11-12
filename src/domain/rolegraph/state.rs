//! Run-state domain: the append-only action log and the status snapshot.
//!
//! Pure data with serde support so the state-store adapter can serialize to
//! JSONL (actions) and JSON (snapshot) without the domain depending on I/O.

use crate::domain::rolegraph::verdict::{BlockReason, Verdict};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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

/// Resumable snapshot of where a run stands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusSnapshot {
    /// The role currently executing, or `None` before start / after terminal.
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
    /// Per back-edge loop counts, keyed by `"role:target"`.
    #[serde(default)]
    pub loop_counters: BTreeMap<String, u32>,
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
        };
        let json = serde_json::to_string(&snap).expect("serialize");
        let round: StatusSnapshot = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(round, snap);
    }
}

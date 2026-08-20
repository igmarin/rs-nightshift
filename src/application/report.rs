//! Terminal report: render a run's outcome as a human-readable summary.

use crate::domain::rolegraph::state::{ActionEvent, EventKind, RunStatus, StatusSnapshot};
use crate::domain::rolegraph::verdict::BlockReason;

/// Human label for a run status (matches the legacy `nightshift status` words).
#[must_use]
pub fn status_label(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Done => "PASSED",
        RunStatus::Failed => "FAILED",
        RunStatus::Blocked => "REQUIRES_HUMAN_REVIEW",
        RunStatus::Running => "RUNNING",
    }
}

/// Human description of a block reason, for the morning report.
#[must_use]
pub fn block_reason_description(reason: BlockReason) -> &'static str {
    match reason {
        BlockReason::None => "none",
        BlockReason::IllDefinedTask => "the task was under-specified or needs clarification",
        BlockReason::ToolFailure => "a tool failed (tests, git apply, or context gathering)",
        BlockReason::VersionMismatch => {
            "a required model, tool, or version is missing or incompatible"
        }
        BlockReason::BudgetExhausted => {
            "a loop or step budget was exhausted before the run finished"
        }
    }
}

/// Render the morning report from the persisted snapshot and action log.
#[must_use]
pub fn render_report(snapshot: &StatusSnapshot, events: &[ActionEvent]) -> String {
    let mut lines = vec![
        format!("Status: {}", status_label(snapshot.status)),
        format!("Steps: {}", snapshot.steps),
    ];
    if snapshot.status != RunStatus::Done {
        lines.push(format!(
            "Block reason: {}",
            block_reason_description(snapshot.block_reason)
        ));
    }
    let roles: Vec<&str> = events
        .iter()
        .filter(|event| event.event == EventKind::RoleEnd)
        .map(|event| event.role.as_str())
        .collect();
    if !roles.is_empty() {
        lines.push(format!("Roles: {}", roles.join(" → ")));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::rolegraph::state::EventKind;
    use crate::domain::rolegraph::verdict::Verdict;

    fn snapshot(status: RunStatus, reason: BlockReason) -> StatusSnapshot {
        StatusSnapshot {
            current_role: Some("qa".into()),
            steps: 3,
            last_verdict: Some(Verdict::Issues),
            status,
            block_reason: reason,
            loop_counters: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn status_label_maps_each_status() {
        assert_eq!(status_label(RunStatus::Done), "PASSED");
        assert_eq!(status_label(RunStatus::Failed), "FAILED");
        assert_eq!(status_label(RunStatus::Blocked), "REQUIRES_HUMAN_REVIEW");
        assert_eq!(status_label(RunStatus::Running), "RUNNING");
    }

    #[test]
    fn done_report_omits_block_reason() {
        let report = render_report(&snapshot(RunStatus::Done, BlockReason::None), &[]);
        assert!(report.contains("PASSED"), "{report}");
        assert!(!report.contains("Block reason"), "{report}");
    }

    #[test]
    fn blocked_report_classifies_the_reason() {
        let report = render_report(
            &snapshot(RunStatus::Blocked, BlockReason::IllDefinedTask),
            &[],
        );
        assert!(report.contains("REQUIRES_HUMAN_REVIEW"), "{report}");
        assert!(report.contains("under-specified"), "{report}");
    }

    #[test]
    fn report_lists_the_role_trail() {
        let events = vec![
            ActionEvent {
                ts: "t".into(),
                event: EventKind::RoleEnd,
                role: "po".into(),
                provider: String::new(),
                model: String::new(),
                verdict: Some(Verdict::Continue),
                artifact: None,
                block_reason: BlockReason::None,
            },
            ActionEvent {
                ts: "t".into(),
                event: EventKind::RoleEnd,
                role: "dev".into(),
                provider: String::new(),
                model: String::new(),
                verdict: Some(Verdict::Done),
                artifact: None,
                block_reason: BlockReason::None,
            },
        ];
        let report = render_report(&snapshot(RunStatus::Done, BlockReason::None), &events);
        assert!(report.contains("Roles: po → dev"), "{report}");
    }
}

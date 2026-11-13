//! Terminal report: render a run's outcome as a human-readable summary.

use crate::domain::rolegraph::state::{ActionEvent, EventKind, RunStatus, StatusSnapshot};
use crate::domain::rolegraph::verdict::BlockReason;
use std::collections::HashMap;

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

/// Seconds since 1970-01-01 for a UTC timestamp in the form
/// `YYYY-MM-DDTHH:MM:SSZ`, or `None` when the text does not have that form.
fn parse_timestamp(ts: &str) -> Option<i64> {
    let bytes = ts.as_bytes();
    if bytes.len() != 20
        || [
            bytes[4], bytes[7], bytes[10], bytes[13], bytes[16], bytes[19],
        ] != *b"--T::Z"
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| {
        bytes[range].iter().try_fold(0_i64, |total, digit| {
            digit
                .is_ascii_digit()
                .then(|| total * 10 + i64::from(digit - b'0'))
        })
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    let days_in_month = match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1..=12 => 31,
        _ => return None,
    };
    if !(1..=days_in_month).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    // Days since 1970-01-01, leap years included (Howard Hinnant's
    // `days_from_civil`): count years from 1 March, in 400-year eras.
    let year = if month <= 2 { year - 1 } else { year };
    let (era, year_of_era) = (year.div_euclid(400), year.rem_euclid(400));
    let month_from_march = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Render the morning report from the persisted snapshot and action log.
#[must_use]
pub fn render_report(snapshot: &StatusSnapshot, events: &[ActionEvent]) -> String {
    let mut lines = vec![
        format!("Status: {}", status_label(snapshot.status)),
        format!("Steps: {}", snapshot.steps),
    ];
    let retry_count = events
        .iter()
        .filter(|event| event.event == EventKind::Retry)
        .count();
    if retry_count > 0 {
        lines.push(format!("Retries: {}", retry_count));
    }
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

    // Compute time per role
    let mut role_times: HashMap<String, i64> = HashMap::new();
    let mut role_order: Vec<String> = Vec::new();
    let mut pending_starts: HashMap<String, i64> = HashMap::new();

    for event in events {
        match event.event {
            EventKind::RoleStart => {
                // A new start replaces an earlier one that had no end, also
                // when the new one does not parse.
                pending_starts.remove(&event.role);
                if let Some(ts) = parse_timestamp(&event.ts) {
                    pending_starts.insert(event.role.clone(), ts);
                }
            }
            EventKind::RoleEnd => {
                if let Some(start_ts) = pending_starts.remove(&event.role) {
                    if let Some(end_ts) = parse_timestamp(&event.ts) {
                        let duration = (end_ts - start_ts).max(0);
                        let role = &event.role;
                        role_times
                            .entry(role.clone())
                            .and_modify(|t| *t += duration)
                            .or_insert(duration);
                        if !role_order.contains(role) {
                            role_order.push(role.clone());
                        }
                    }
                    // An end that does not parse drops the pair.
                }
            }
            _ => {}
        }
    }

    if !role_times.is_empty() {
        lines.push("Time per role".to_string());
        for role in &role_order {
            if let Some(&seconds) = role_times.get(role) {
                lines.push(format!("  {}: {}s", role, seconds));
            }
        }
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
                detail: None,
                call: None,
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
                detail: None,
                call: None,
            },
        ];
        let report = render_report(&snapshot(RunStatus::Done, BlockReason::None), &events);
        assert!(report.contains("Roles: po → dev"), "{report}");
    }

    #[test]
    fn test_parse_timestamp_valid() {
        assert_eq!(parse_timestamp("2024-01-15T10:30:45Z"), Some(1_705_314_645));
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
    }

    #[test]
    fn test_parse_timestamp_invalid() {
        assert!(parse_timestamp("invalid").is_none());
        assert!(parse_timestamp("2024-01-15").is_none());
        assert!(parse_timestamp("2024-13-15T10:30:45Z").is_none());
        assert!(parse_timestamp("2024-01-32T10:30:45Z").is_none());
        assert!(parse_timestamp("2024-01-15T25:30:45Z").is_none());
    }

    #[test]
    fn parse_timestamp_counts_leap_days() {
        // 2024 is a leap year: 29 February is one day before 1 March.
        let feb_29 = parse_timestamp("2024-02-29T23:59:30Z").expect("29 Feb");
        let mar_1 = parse_timestamp("2024-03-01T00:00:30Z").expect("1 Mar");
        assert_eq!(mar_1 - feb_29, 60);
        assert_eq!(parse_timestamp("2024-03-01T00:00:00Z"), Some(1_709_251_200));
        // 2100 is not a leap year.
        let feb_28 = parse_timestamp("2100-02-28T12:00:00Z").expect("28 Feb");
        let mar_1 = parse_timestamp("2100-03-01T12:00:00Z").expect("1 Mar");
        assert_eq!(mar_1 - feb_28, 86_400);
    }

    #[test]
    fn parse_timestamp_never_panics_on_odd_input() {
        // Before 1970, and 20 bytes that are not 20 characters.
        assert_eq!(parse_timestamp("1969-12-31T23:59:59Z"), Some(-1));
        assert_eq!(parse_timestamp("2024-01-15T10:30:4\u{e9}"), None);
        assert_eq!(parse_timestamp("\u{e9}024-01-15T10:30:45"), None);
        assert_eq!(parse_timestamp("+024-01-15T10:30:45Z"), None);
    }

    #[test]
    fn parse_timestamp_rejects_days_that_do_not_exist() {
        assert_eq!(parse_timestamp("2024-02-30T10:00:00Z"), None);
        assert_eq!(parse_timestamp("2023-02-29T10:00:00Z"), None);
        assert_eq!(parse_timestamp("2100-02-29T10:00:00Z"), None);
        assert_eq!(parse_timestamp("2024-04-31T10:00:00Z"), None);
        assert_eq!(parse_timestamp("2024-01-00T10:00:00Z"), None);
        assert!(parse_timestamp("2024-02-29T10:00:00Z").is_some());
        assert!(parse_timestamp("2000-02-29T10:00:00Z").is_some());
        assert!(parse_timestamp("2024-12-31T10:00:00Z").is_some());
    }

    fn event(kind: EventKind, role: &str, ts: &str) -> ActionEvent {
        ActionEvent {
            ts: ts.into(),
            event: kind,
            role: role.into(),
            provider: String::new(),
            model: String::new(),
            verdict: None,
            artifact: None,
            block_reason: BlockReason::None,
            detail: None,
            call: None,
        }
    }

    #[test]
    fn a_start_that_does_not_parse_drops_the_earlier_start() {
        // The first start has no end. The second start is unreadable, so the
        // end that follows has no start to pair with.
        let events = vec![
            event(EventKind::RoleStart, "dev", "2024-01-15T10:00:00Z"),
            event(EventKind::RoleStart, "dev", "invalid"),
            event(EventKind::RoleEnd, "dev", "2024-01-15T11:00:00Z"),
            event(EventKind::RoleStart, "qa", "2024-01-15T11:00:00Z"),
            event(EventKind::RoleEnd, "qa", "2024-01-15T11:00:07Z"),
        ];
        let report = render_report(&snapshot(RunStatus::Done, BlockReason::None), &events);
        assert!(report.contains("  qa: 7s"), "{report}");
        assert!(!report.contains("dev:"), "{report}");
    }

    #[test]
    fn single_role_90_seconds() {
        let events = vec![
            ActionEvent {
                ts: "2024-01-15T10:00:00Z".into(),
                event: EventKind::RoleStart,
                role: "po".into(),
                provider: String::new(),
                model: String::new(),
                verdict: None,
                artifact: None,
                block_reason: BlockReason::None,
                detail: None,
                call: None,
            },
            ActionEvent {
                ts: "2024-01-15T10:01:30Z".into(),
                event: EventKind::RoleEnd,
                role: "po".into(),
                provider: String::new(),
                model: String::new(),
                verdict: Some(Verdict::Done),
                artifact: None,
                block_reason: BlockReason::None,
                detail: None,
                call: None,
            },
        ];
        let report = render_report(&snapshot(RunStatus::Done, BlockReason::None), &events);
        assert!(report.contains("Time per role"), "{report}");
        assert!(report.contains("  po: 90s"), "{report}");
    }

    #[test]
    fn role_runs_twice_summed() {
        let events = vec![
            ActionEvent {
                ts: "2024-01-15T10:00:00Z".into(),
                event: EventKind::RoleStart,
                role: "dev".into(),
                provider: String::new(),
                model: String::new(),
                verdict: None,
                artifact: None,
                block_reason: BlockReason::None,
                detail: None,
                call: None,
            },
            ActionEvent {
                ts: "2024-01-15T10:00:30Z".into(),
                event: EventKind::RoleEnd,
                role: "dev".into(),
                provider: String::new(),
                model: String::new(),
                verdict: Some(Verdict::Continue),
                artifact: None,
                block_reason: BlockReason::None,
                detail: None,
                call: None,
            },
            ActionEvent {
                ts: "2024-01-15T10:01:00Z".into(),
                event: EventKind::RoleStart,
                role: "dev".into(),
                provider: String::new(),
                model: String::new(),
                verdict: None,
                artifact: None,
                block_reason: BlockReason::None,
                detail: None,
                call: None,
            },
            ActionEvent {
                ts: "2024-01-15T10:01:15Z".into(),
                event: EventKind::RoleEnd,
                role: "dev".into(),
                provider: String::new(),
                model: String::new(),
                verdict: Some(Verdict::Done),
                artifact: None,
                block_reason: BlockReason::None,
                detail: None,
                call: None,
            },
        ];
        let report = render_report(&snapshot(RunStatus::Done, BlockReason::None), &events);
        assert!(report.contains("Time per role"), "{report}");
        assert!(report.contains("  dev: 45s"), "{report}");
    }

    #[test]
    fn midnight_crossing_counted_correctly() {
        let events = vec![
            ActionEvent {
                ts: "2024-01-15T23:59:30Z".into(),
                event: EventKind::RoleStart,
                role: "qa".into(),
                provider: String::new(),
                model: String::new(),
                verdict: None,
                artifact: None,
                block_reason: BlockReason::None,
                detail: None,
                call: None,
            },
            ActionEvent {
                ts: "2024-01-16T00:00:30Z".into(),
                event: EventKind::RoleEnd,
                role: "qa".into(),
                provider: String::new(),
                model: String::new(),
                verdict: Some(Verdict::Done),
                artifact: None,
                block_reason: BlockReason::None,
                detail: None,
                call: None,
            },
        ];
        let report = render_report(&snapshot(RunStatus::Done, BlockReason::None), &events);
        assert!(report.contains("Time per role"), "{report}");
        assert!(report.contains("  qa: 60s"), "{report}");
    }

    #[test]
    fn unparseable_timestamps_no_section() {
        let events = vec![
            ActionEvent {
                ts: "invalid".into(),
                event: EventKind::RoleStart,
                role: "po".into(),
                provider: String::new(),
                model: String::new(),
                verdict: None,
                artifact: None,
                block_reason: BlockReason::None,
                detail: None,
                call: None,
            },
            ActionEvent {
                ts: "also_invalid".into(),
                event: EventKind::RoleEnd,
                role: "po".into(),
                provider: String::new(),
                model: String::new(),
                verdict: Some(Verdict::Done),
                artifact: None,
                block_reason: BlockReason::None,
                detail: None,
                call: None,
            },
        ];
        let report = render_report(&snapshot(RunStatus::Done, BlockReason::None), &events);
        assert!(!report.contains("Time per role"), "{report}");
    }

    #[test]
    fn report_shows_retry_count() {
        let events = vec![
            ActionEvent {
                ts: "t".into(),
                event: EventKind::Retry,
                role: "dev".into(),
                provider: String::new(),
                model: String::new(),
                verdict: None,
                artifact: None,
                block_reason: BlockReason::None,
                detail: None,
                call: None,
            },
            ActionEvent {
                ts: "t".into(),
                event: EventKind::Retry,
                role: "dev".into(),
                provider: String::new(),
                model: String::new(),
                verdict: None,
                artifact: None,
                block_reason: BlockReason::None,
                detail: None,
                call: None,
            },
        ];
        let report = render_report(&snapshot(RunStatus::Done, BlockReason::None), &events);
        assert!(report.contains("Retries: 2"), "{report}");
    }

    #[test]
    fn report_omits_retry_line_when_none() {
        let events = vec![ActionEvent {
            ts: "t".into(),
            event: EventKind::RoleEnd,
            role: "dev".into(),
            provider: String::new(),
            model: String::new(),
            verdict: Some(Verdict::Done),
            artifact: None,
            block_reason: BlockReason::None,
            detail: None,
            call: None,
        }];
        let report = render_report(&snapshot(RunStatus::Done, BlockReason::None), &events);
        assert!(!report.contains("Retries:"), "{report}");
    }
}

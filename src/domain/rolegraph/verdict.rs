//! Verdicts a role emits to drive deterministic routing.
//!
//! A role (one LLM invocation) returns a small structured envelope — the
//! [`RoleOutput`] — that the harness parses and routes on. The verdict
//! vocabulary is deliberately small and fixed so control flow stays
//! deterministic: no LLM decides the next step.

use serde::{Deserialize, Serialize};

/// Why a run halted or failed. Surfaced in the morning report so the operator
/// can tell "task was under-specified" from "a tool broke" at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    /// No block reason — used when the run succeeded or a verdict carries none.
    #[default]
    None,
    /// The task/goal was not specific enough to act on.
    IllDefinedTask,
    /// A code-side tool (test runner, git apply, context gather) failed.
    ToolFailure,
    /// A required model/tool/version was missing or incompatible.
    VersionMismatch,
    /// A loop or step cap was exhausted before the run could finish.
    BudgetExhausted,
}

/// A clarifying question a role can raise, with a blocking flag.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Question {
    /// The question text, in plain language.
    pub text: String,
    /// Whether this question blocks progress until answered.
    pub blocking: bool,
}

/// The verdict a role emits, plus any structured payload the harness routes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Work accepted; proceed to the `continue` target.
    Continue,
    /// Work incomplete; route to the `issues` target with `findings`.
    Issues,
    /// Needs clarification; route to the `questions` target or halt.
    Questions,
    /// Finished successfully; terminal.
    Done,
    /// Hard failure; terminal, with a `block_reason`.
    Fail,
}

/// The structured envelope a role is asked to emit and the harness parses.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RoleOutput {
    /// Routing decision for this role's result.
    pub verdict: Verdict,
    /// The role's deliverable text (the artifact body), written verbatim to
    /// the role's output file.
    #[serde(default)]
    pub content: String,
    /// One-line human summary of what happened.
    #[serde(default)]
    pub summary: String,
    /// Findings carried to the `issues` target when the verdict is
    /// [`Verdict::Issues`].
    #[serde(default)]
    pub findings: Vec<String>,
    /// Clarifying questions carried when the verdict is [`Verdict::Questions`].
    #[serde(default)]
    pub questions: Vec<Question>,
    /// Why the run halted, when the verdict is [`Verdict::Fail`] or a loop cap
    /// was exhausted.
    #[serde(default)]
    pub block_reason: BlockReason,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_deserializes_from_snake_case() {
        assert_eq!(
            serde_json::from_str::<Verdict>("\"continue\"").expect("continue"),
            Verdict::Continue
        );
        assert_eq!(
            serde_json::from_str::<Verdict>("\"issues\"").expect("issues"),
            Verdict::Issues
        );
        assert_eq!(
            serde_json::from_str::<Verdict>("\"questions\"").expect("questions"),
            Verdict::Questions
        );
        assert_eq!(
            serde_json::from_str::<Verdict>("\"done\"").expect("done"),
            Verdict::Done
        );
        assert_eq!(
            serde_json::from_str::<Verdict>("\"fail\"").expect("fail"),
            Verdict::Fail
        );
    }

    #[test]
    fn role_output_parses_with_defaults() {
        let output: RoleOutput = serde_json::from_str(r#"{"verdict":"done"}"#).expect("parse");
        assert_eq!(output.verdict, Verdict::Done);
        assert!(output.summary.is_empty());
        assert!(output.findings.is_empty());
        assert!(output.questions.is_empty());
        assert_eq!(output.block_reason, BlockReason::None);
    }

    #[test]
    fn role_output_parses_issues_with_findings() {
        let output: RoleOutput = serde_json::from_str(
            r#"{"verdict":"issues","findings":["compile error in src/main.rs"]}"#,
        )
        .expect("parse");
        assert_eq!(output.verdict, Verdict::Issues);
        assert_eq!(output.findings, vec!["compile error in src/main.rs"]);
    }

    #[test]
    fn role_output_parses_questions_with_blocking_flags() {
        let output: RoleOutput = serde_json::from_str(
            r#"{"verdict":"questions","questions":[{"text":"which port?","blocking":true}]}"#,
        )
        .expect("parse");
        assert_eq!(output.verdict, Verdict::Questions);
        assert_eq!(output.questions.len(), 1);
        assert!(output.questions[0].blocking);
    }

    #[test]
    fn block_reason_deserializes_snake_case() {
        assert_eq!(
            serde_json::from_str::<BlockReason>("\"ill_defined_task\"").expect("parse"),
            BlockReason::IllDefinedTask
        );
        assert_eq!(
            serde_json::from_str::<BlockReason>("\"budget_exhausted\"").expect("parse"),
            BlockReason::BudgetExhausted
        );
    }
}

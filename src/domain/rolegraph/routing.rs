//! Routing targets and the per-role verdict → target map.

use serde::Deserialize;

/// Where a verdict routes the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Continue the run at the named role.
    Role(String),
    /// Terminate the run successfully.
    Done,
    /// Halt the run and write a report for the operator.
    Halt,
}

impl<'de> Deserialize<'de> for Target {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "@done" => Target::Done,
            "@halt" => Target::Halt,
            _ => Target::Role(value),
        })
    }
}

/// Per-role routing map: which target each verdict routes to.
///
/// `next` (the `continue` verdict) defaults to [`Target::Done`]; `issues` and
/// `questions` default to [`Target::Halt`] when omitted. `done` and `fail` are
/// always terminal and need no entry here.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Routing {
    /// Target for the `continue` verdict (TOML key `continue`).
    #[serde(default, rename = "continue")]
    pub next: Option<Target>,
    /// Target for the `issues` verdict.
    #[serde(default)]
    pub issues: Option<Target>,
    /// Target for the `questions` verdict.
    #[serde(default)]
    pub questions: Option<Target>,
}

impl Routing {
    /// Effective target for the `continue` verdict (defaults to [`Target::Done`]).
    #[must_use]
    pub fn continue_target(&self) -> Target {
        self.next.clone().unwrap_or(Target::Done)
    }

    /// Effective target for the `issues` verdict (defaults to [`Target::Halt`]).
    #[must_use]
    pub fn issues_target(&self) -> Target {
        self.issues.clone().unwrap_or(Target::Halt)
    }

    /// Effective target for the `questions` verdict (defaults to [`Target::Halt`]).
    #[must_use]
    pub fn questions_target(&self) -> Target {
        self.questions.clone().unwrap_or(Target::Halt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_deserializes_sentinels_and_role_ids() {
        assert_eq!(
            serde_json::from_str::<Target>("\"@done\"").expect("done"),
            Target::Done
        );
        assert_eq!(
            serde_json::from_str::<Target>("\"@halt\"").expect("halt"),
            Target::Halt
        );
        assert_eq!(
            serde_json::from_str::<Target>("\"qa\"").expect("role"),
            Target::Role("qa".into())
        );
    }

    #[test]
    fn routing_defaults_continue_to_done_and_loops_to_halt() {
        let routing = Routing::default();
        assert_eq!(routing.continue_target(), Target::Done);
        assert_eq!(routing.issues_target(), Target::Halt);
        assert_eq!(routing.questions_target(), Target::Halt);
    }

    #[test]
    fn routing_parses_continue_issues_and_questions() {
        let routing: Routing =
            serde_json::from_str(r#"{"continue":"qa","issues":"developer"}"#).expect("parse");
        assert_eq!(routing.continue_target(), Target::Role("qa".into()));
        assert_eq!(routing.issues_target(), Target::Role("developer".into()));
        // questions omitted → defaults to halt.
        assert_eq!(routing.questions_target(), Target::Halt);
    }
}

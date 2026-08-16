//! Doctor report data model and formatting.

/// One named check in a doctor report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// Stable check identifier (for example `ollama` or `model:llama3.1:8b`).
    pub name: String,
    /// Whether the check passed.
    pub passed: bool,
    /// When true, a failure makes the environment not ready.
    pub required: bool,
    /// Operator-facing detail.
    pub detail: String,
}

/// Full doctor report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    /// Ordered checks.
    pub checks: Vec<Check>,
}

impl DoctorReport {
    /// Required checks all passed.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.checks.iter().filter(|c| c.required).all(|c| c.passed)
    }

    /// Process exit code: `0` ready, `2` not ready.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        if self.is_ready() {
            0
        } else {
            2
        }
    }
}

/// Write a human-readable report.
pub fn write_report(report: &DoctorReport, mut out: impl std::io::Write) -> std::io::Result<()> {
    for check in &report.checks {
        let mark = if check.passed {
            "ok"
        } else if check.required {
            "FAIL"
        } else {
            "warn"
        };
        writeln!(out, "[{mark}] {} - {}", check.name, check.detail)?;
    }
    if report.is_ready() {
        writeln!(out, "environment is ready")?;
    } else {
        writeln!(out, "environment is not ready")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_report_marks_failures_and_warnings() {
        let report = DoctorReport {
            checks: vec![
                Check {
                    name: "rustc".into(),
                    passed: true,
                    required: true,
                    detail: "ok rustc".into(),
                },
                Check {
                    name: "mise".into(),
                    passed: false,
                    required: false,
                    detail: "no mise".into(),
                },
                Check {
                    name: "ollama".into(),
                    passed: false,
                    required: true,
                    detail: "down".into(),
                },
            ],
        };
        let mut buf = Vec::new();
        write_report(&report, &mut buf).expect("write");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("[ok] rustc"));
        assert!(text.contains("[warn] mise"));
        assert!(text.contains("[FAIL] ollama"));
        assert!(text.contains("environment is not ready"));
    }

    #[test]
    fn write_report_ready_message() {
        let report = DoctorReport {
            checks: vec![Check {
                name: "rustc".into(),
                passed: true,
                required: true,
                detail: "ok".into(),
            }],
        };
        let mut buf = Vec::new();
        write_report(&report, &mut buf).expect("write");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("environment is ready"));
    }
}

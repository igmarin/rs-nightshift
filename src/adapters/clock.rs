//! System clock adapter.

use crate::ports::Clock;

/// The real system clock, read via the Unix `date` command.
///
/// This adapter is allowed to shell out (ADR-007); the domain and application
/// only ever see the [`Clock`] port.
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_iso(&self) -> String {
        date(&["-u", "+%Y-%m-%dT%H:%M:%SZ"])
    }

    fn today(&self) -> String {
        date(&["+%Y-%m-%d"])
    }
}

fn date(args: &[&str]) -> String {
    std::process::Command::new("date")
        .args(args)
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn today_matches_yyyy_mm_dd_shape() {
        // Best-effort: the real `date` command is available on Unix CI and dev.
        let today = SystemClock.today();
        assert_eq!(today.len(), 10, "{today}");
        assert_eq!(&today[4..5], "-", "{today}");
        assert_eq!(&today[7..8], "-", "{today}");
    }

    #[test]
    fn now_iso_ends_in_utc_z() {
        let now = SystemClock.now_iso();
        assert!(now.ends_with('Z'), "{now}");
    }
}

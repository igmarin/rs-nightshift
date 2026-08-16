//! Pipeline state persistence (`pipeline_state.json`).

use crate::error::Error;
use serde::Serialize;

/// Internal pipeline state written to `pipeline_state.json`.
#[derive(Serialize)]
struct PipelineState<'a> {
    stage: &'a str,
    iteration: u8,
    last_error: Option<&'a str>,
}

/// Validates that `date` matches the format `YYYY-MM-DD`.
/// Does NOT validate calendar correctness (e.g., 2026-02-31 passes format).
pub(super) fn validate_date_format(date: &str) -> Result<(), Error> {
    let bytes = date.as_bytes();
    let ok = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[8..10].iter().all(u8::is_ascii_digit);
    if ok {
        Ok(())
    } else {
        Err(Error::Artifact(format!(
            "run date must be YYYY-MM-DD, got {date:?}"
        )))
    }
}

/// Write `pipeline_state.json` into `run`.
pub(super) fn write_pipeline_state(
    run: &std::path::Path,
    stage: &str,
    iteration: u8,
    last_error: Option<&str>,
) -> Result<(), Error> {
    let state = PipelineState {
        stage,
        iteration,
        last_error,
    };
    let bytes = serde_json::to_vec_pretty(&state).map_err(|e| Error::Artifact(e.to_string()))?;
    std::fs::write(run.join("pipeline_state.json"), bytes)?;
    Ok(())
}

/// Point `latest` symlink at the newest run directory.
pub(super) fn update_latest(root: &std::path::Path, dir_name: &str) -> Result<(), Error> {
    let latest = root.join("latest");
    match std::fs::remove_file(&latest) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir_name, &latest)?;
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(dir_name, &latest)?;
    }
    Ok(())
}

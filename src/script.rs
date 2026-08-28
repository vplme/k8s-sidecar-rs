//! SCRIPT execution mirroring upstream helpers.execute(): run directly when
//! executable, else via `sh`; log stdout/stderr as Python bytes-reprs (the
//! upstream test suite counts occurrences inside those debug lines).

use std::os::unix::fs::PermissionsExt;

use crate::logger;

/// Render bytes the way Python's repr() does: b'...\n...'.
fn bytes_repr(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() + 3);
    out.push_str("b'");
    for &b in data {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'\'' => out.push_str("\\'"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(b as char),
            _ => out.push_str(&format!("\\x{:02x}", b)),
        }
    }
    out.push('\'');
    out
}

/// Spawn errors are returned so callers can route them through the same
/// generic exception paths as upstream.
pub async fn execute(script_path: &str) -> Result<(), String> {
    logger::info(&format!("Executing script from {}", script_path));

    let executable = std::fs::metadata(script_path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false);

    let (mut cmd, argv_repr) = if executable {
        (
            tokio::process::Command::new(script_path),
            format!("['{}']", script_path),
        )
    } else {
        let mut c = tokio::process::Command::new("sh");
        c.arg(script_path);
        (c, format!("['sh', '{}']", script_path))
    };

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("{}: '{}'", e, script_path))?;

    if output.status.success() {
        logger::debug(&format!("Script stdout: {}", bytes_repr(&output.stdout)));
        logger::debug(&format!("Script stderr: {}", bytes_repr(&output.stderr)));
        logger::debug("Script exit code: 0");
    } else {
        // Python: CalledProcessError caught and logged, not raised further.
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "-1".into());
        logger::error(&format!(
            "Script failed with error: Command '{}' returned non-zero exit status {}.",
            argv_repr, code
        ));
    }
    Ok(())
}

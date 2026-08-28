//! File writing/removal mirroring upstream helpers.py: sha256 write
//! suppression (which gates SCRIPT and REQ_URL via files_changed),
//! DEFAULT_FILE_MODE chmod, and the exact log lines.

use sha2::{Digest, Sha256};
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::logger;

/// Content-type tags as upstream logs them: CONTENT_TYPE_TEXT is "ascii".
pub const CONTENT_TYPE_TEXT: &str = "ascii";
pub const CONTENT_TYPE_BASE64_BINARY: &str = "binary";

/// Returns Ok(true) if the file was (re)written; errors bubble up to the
/// caller's catch-all like Python exceptions do.
pub fn write_data_to_file(
    folder: &str,
    filename: &str,
    data: &[u8],
    data_type: &str,
) -> std::io::Result<bool> {
    if !Path::new(folder).exists()
        && let Err(e) = std::fs::create_dir_all(folder)
    {
        match e.kind() {
            ErrorKind::PermissionDenied => {
                logger::error(&format!(
                    "Error: insufficient privileges to create {}. Skipping {}.",
                    folder, filename
                ));
                return Ok(false);
            }
            ErrorKind::AlreadyExists => {}
            _ => return Err(e),
        }
    }

    let absolute_path = Path::new(folder).join(filename);
    if absolute_path.exists() {
        let current = std::fs::read(&absolute_path)?;
        if Sha256::digest(data) == Sha256::digest(&current) {
            logger::debug(&format!(
                "Contents of {} haven't changed. Not overwriting existing file",
                filename
            ));
            return Ok(false);
        }
    }

    logger::info(&format!(
        "Writing {} ({})",
        absolute_path.display(),
        data_type
    ));
    std::fs::write(&absolute_path, data)?;

    if let Ok(mode_str) = std::env::var("DEFAULT_FILE_MODE")
        && !mode_str.is_empty()
    {
        // Invalid octal bubbles up like Python's ValueError.
        let mode = u32::from_str_radix(&mode_str, 8).map_err(|e| {
            std::io::Error::new(
                ErrorKind::InvalidData,
                format!("invalid DEFAULT_FILE_MODE: {}", e),
            )
        })?;
        std::fs::set_permissions(&absolute_path, std::fs::Permissions::from_mode(mode))?;
    }
    Ok(true)
}

pub fn remove_file(folder: &str, filename: &str) -> bool {
    let complete_file = Path::new(folder).join(filename);
    if complete_file.is_file() {
        logger::info(&format!("Removing {}", complete_file.display()));
        std::fs::remove_file(&complete_file).is_ok()
    } else {
        logger::error(&format!(
            "Unable to remove {}, file not found",
            complete_file.display()
        ));
        false
    }
}

pub fn unique_filename(
    filename: &str,
    namespace: &str,
    resource: &str,
    resource_name: &str,
) -> String {
    format!(
        "namespace_{}.{}_{}.{}",
        namespace, resource, resource_name, filename
    )
}

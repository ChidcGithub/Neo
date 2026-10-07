use super::BoardKind;
use std::path::{Component, Path, PathBuf};

pub(super) fn resolve(kind: BoardKind) -> Result<PathBuf, String> {
    let variable = match kind {
        BoardKind::Drawing => "NEO_DRAWING_DIR",
        BoardKind::Blackboard => "NEO_BLACKBOARD_DIR",
    };
    let directory = match std::env::var_os(variable) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        Some(_) => return Err(format!("{variable} must be a nonempty absolute directory")),
        None => std::env::current_exe()
            .map_err(|e| e.to_string())?
            .parent()
            .ok_or("host executable has no parent")?
            .join("apps")
            .join(kind.code()),
    };
    validate_path(&directory.join(kind.executable()), kind)
}

/// Production path validation; test builds construct trusted paths directly.
#[allow(dead_code)]
pub(super) fn validate_for_start(kind: BoardKind, path: &Path) -> Result<PathBuf, String> {
    let checked = validate_path(path, kind)?;
    if checked != resolve(kind)? {
        return Err(
            "runtime path must match the installed executable or explicit development override"
                .into(),
        );
    }
    Ok(checked)
}

pub(super) fn validate_path(path: &Path, kind: BoardKind) -> Result<PathBuf, String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err("runtime path must be absolute without traversal".into());
    }
    if path.file_name() != Some(std::ffi::OsStr::new(kind.executable())) {
        return Err("unexpected runtime executable filename".into());
    }
    #[cfg(windows)]
    {
        use std::path::Prefix;
        if !matches!(path.components().next(), Some(Component::Prefix(p))
            if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        {
            return Err(
                "runtime must be on a local absolute drive, not a network/device path".into(),
            );
        }
        // Forbid ADS, ambiguous Win32 trailing-dot/space names and device aliases.
        for part in path.components() {
            if let Component::Normal(name) = part {
                let name = name.to_str().ok_or("runtime path is not Unicode")?;
                let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
                if name.contains(':')
                    || name.ends_with(['.', ' '])
                    || name.chars().any(char::is_control)
                    || matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                    || (base.len() == 4
                        && (base.starts_with("COM") || base.starts_with("LPT"))
                        && matches!(base.as_bytes()[3], b'1'..=b'9'))
                {
                    return Err("ambiguous or device runtime path".into());
                }
            }
        }
    }
    for ancestor in path.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor)
            .map_err(|e| format!("runtime path unavailable: {e}"))?;
        if metadata.file_type().is_symlink() {
            return Err("runtime symlinks are not allowed".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("runtime reparse points are not allowed".into());
            }
        }
        if ancestor != path && !metadata.is_dir() {
            return Err("runtime parent is not a directory".into());
        }
        if ancestor == path && !metadata.is_file() {
            return Err("runtime executable is not a regular file".into());
        }
    }
    // This is not a signature/ACL check. A trusted directory must not be writable by
    // an adversary; path-only validation cannot eliminate replacement TOCTOU races.
    std::fs::canonicalize(path).map_err(|e| e.to_string())
}

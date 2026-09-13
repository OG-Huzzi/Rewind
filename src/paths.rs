use std::env;
use std::fs::{self, OpenOptions};
use std::path::{Component, Path, PathBuf};

use uuid::Uuid;

use crate::error::{Result, RewindError};
use crate::model::WorkspacePointer;

pub fn canonical_root(path: &Path) -> Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    if !canonical.is_dir() {
        return Err(RewindError::WorkspaceIdentity(format!(
            "{} is not a directory",
            canonical.display()
        )));
    }
    Ok(canonical)
}

pub fn default_store_root() -> Result<PathBuf> {
    if let Some(value) = env::var_os("REWIND_HOME") {
        return Ok(PathBuf::from(value));
    }
    let home = if cfg!(windows) {
        env::var_os("USERPROFILE")
    } else {
        env::var_os("HOME")
    }
    .ok_or_else(|| RewindError::Storage("user home directory is unavailable".to_owned()))?;
    Ok(PathBuf::from(home).join(".rewind"))
}

pub fn marker_path(root: &Path) -> PathBuf {
    root.join(".rewind").join("workspace.json")
}

pub fn discover_marker(start: &Path) -> Result<PathBuf> {
    let mut current = fs::canonicalize(start)?;
    loop {
        let candidate = marker_path(&current);
        if fs::symlink_metadata(&candidate)
            .map(|metadata| metadata.is_file())
            .unwrap_or(false)
        {
            return Ok(candidate);
        }
        if !current.pop() {
            break;
        }
    }
    Err(RewindError::WorkspaceNotInitialized)
}

pub fn write_pointer(root: &Path, pointer: &WorkspacePointer) -> Result<()> {
    let path = marker_path(root);
    if let Some(parent) = path.parent() {
        if let Ok(metadata) = fs::symlink_metadata(parent) {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(RewindError::PathEscape(format!(
                    "workspace metadata directory is not local: {}",
                    parent.display()
                )));
            }
        }
    }
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| RewindError::Storage("workspace marker has no parent".to_owned()))?,
    )?;
    let bytes = serde_json::to_vec_pretty(pointer)?;
    atomic_write(&path, &bytes)
}

pub fn read_pointer(path: &Path) -> Result<WorkspacePointer> {
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| RewindError::Storage("target has no parent".to_owned()))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        Uuid::new_v4()
    ));
    // Durability protocol (Phase 0.7 §11.3 step 2): the payload is flushed
    // before publication so a crash after the rename cannot leave the target
    // pointing at an empty or truncated file. Directory-entry durability is
    // requested where the platform accepts it (unix); on Windows the flush is
    // best-effort per the platform capability matrix.
    let write_result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp);
        return Err(RewindError::Storage(format!(
            "write temporary file {}: {error}",
            temp.display()
        )));
    }
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            let _ = fs::remove_file(&temp);
            return Err(RewindError::PathEscape(format!(
                "refusing to replace symlink {}",
                path.display()
            )));
        }
    }
    let publish = fs::rename(&temp, path);
    if publish.is_err() && path.exists() {
        fs::remove_file(path).map_err(|error| {
            RewindError::Storage(format!("replace existing file {}: {error}", path.display()))
        })?;
        fs::rename(&temp, path).map_err(|error| {
            RewindError::Storage(format!("publish replacement {}: {error}", path.display()))
        })?;
    } else {
        publish.map_err(|error| {
            RewindError::Storage(format!("publish file {}: {error}", path.display()))
        })?;
    }
    sync_directory(parent)?;
    Ok(())
}

pub fn ensure_directory(path: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(RewindError::PathEscape(format!(
                "expected a local directory: {}",
                path.display()
            )));
        }
        return Ok(());
    }
    fs::create_dir_all(path)?;
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(RewindError::PathEscape(format!(
            "directory was replaced during creation: {}",
            path.display()
        )));
    }
    Ok(())
}

pub fn validate_relative(relative: &str) -> Result<()> {
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err(RewindError::PathEscape(relative.to_owned()));
    }
    for component in path.components() {
        match component {
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(RewindError::PathEscape(relative.to_owned()))
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(())
}

pub fn workspace_path(root: &Path, relative: &str) -> Result<PathBuf> {
    validate_relative(relative)?;
    let path = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
    ensure_parent_confinement(root, &path)?;
    Ok(path)
}

pub fn normalize_relative(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| RewindError::PathEscape(path.display().to_string()))?;
    let mut components = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => components.push(value.to_string_lossy().into_owned()),
            Component::CurDir => {}
            _ => return Err(RewindError::PathEscape(path.display().to_string())),
        }
    }
    Ok(components.join("/"))
}

pub fn staging_root(root: &Path, transaction_id: Uuid) -> Result<PathBuf> {
    let parent = root.parent().ok_or_else(|| {
        RewindError::Storage("workspace has no same-filesystem parent".to_owned())
    })?;
    let staging_parent = parent.join(".rewind-txn");
    ensure_directory(&staging_parent)?;
    let staging = staging_parent.join(transaction_id.to_string());
    ensure_directory(&staging)?;
    Ok(staging)
}

pub fn ensure_parent_confinement(root: &Path, target: &Path) -> Result<()> {
    let relative = target
        .strip_prefix(root)
        .map_err(|_| RewindError::PathEscape(target.display().to_string()))?;
    let mut current = root.to_path_buf();
    let components: Vec<_> = relative.components().collect();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        let Component::Normal(name) = component else {
            return Err(RewindError::PathEscape(target.display().to_string()));
        };
        current.push(name);
        if current.exists() {
            let metadata = fs::symlink_metadata(&current)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(RewindError::PathEscape(format!(
                    "parent component is not a confined directory: {}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

pub fn remove_workspace_pointer(root: &Path) -> Result<()> {
    let marker = marker_path(root);
    if marker.exists() {
        fs::remove_file(marker)?;
    }
    Ok(())
}

/// Post-mutation confinement verification (Phase 1.2 Fix #3). Pre-mutation
/// checks necessarily run through a path string, so a racing external
/// writer can swap a parent directory for a symlink between the check and
/// the mutation. The mutation itself then resolved through whatever the
/// components pointed at *at mutation time*; this re-inspection confirms the
/// endpoint the operating system actually touched still sits inside the
/// workspace under real (non-symlink) parent components. The residual race
/// window between the mutation and this verification is documented by the
/// Phase 0.7 contract as an accepted conditional-guarantee boundary;
/// nothing here claims openat2-strength protection on platforms that do
/// not provide descriptor-relative operations.
///
/// `mutated_leaf_exists` distinguishes "the leaf should now exist" (create,
/// replace, quarantine-into) from "the leaf should now be absent"
/// (removal); for the absent case the parent chain is still fully verified.
pub fn verify_mutation_confined(
    root: &Path,
    mutated_path: &Path,
    mutated_leaf_exists: bool,
) -> Result<()> {
    // The endpoint must remain addressable beneath the root with real
    // directory parents (no symlink/reparse parent anywhere on the chain).
    ensure_parent_confinement(root, mutated_path)?;
    if mutated_leaf_exists {
        let metadata = fs::symlink_metadata(mutated_path).map_err(|error| {
            RewindError::PathEscape(format!(
                "mutated path is missing after mutation {}: {error}",
                mutated_path.display()
            ))
        })?;
        if metadata.file_type().is_symlink() && !leaf_was_symlink_expected() {
            // A leaf that is a symlink can only be legitimate when the
            // mutation installed a recorded SYMLINK; the rollback layer
            // passes `false` for those cases and re-verifies the target
            // itself, so a symlink here means the endpoint was swapped.
            return Err(RewindError::PathEscape(format!(
                "mutated path became a symlink after mutation: {}",
                mutated_path.display()
            )));
        }
    }
    Ok(())
}

/// Reserved for the symlink-installation path; rollback calls
/// `verify_mutation_confined` with `false` for leaf-exists after
/// installing a symlink from a recorded fingerprint and separately
/// re-reads the link target, so a true value is never needed here.
fn leaf_was_symlink_expected() -> bool {
    false
}

/// Best-effort flush of a directory's entry metadata so a just-published
/// file is durable as a namespace entry. Windows does not expose a
/// POSIX-style directory fsync; per the platform capability matrix the
/// request is silently skipped there.
fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

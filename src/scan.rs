use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use crate::cas::Cas;
use crate::error::{Result, RewindError};
use crate::model::{
    Effect, EffectType, Fingerprint, Manifest, MetadataFingerprint, SymlinkTargetKind,
};
use crate::paths::normalize_relative;

#[derive(Clone, Debug)]
pub struct ScanResult {
    pub manifest: Manifest,
    pub state_id: String,
    pub unsupported_paths: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct ScanOptions {
    pub deadline: Option<Instant>,
    /// When false the scan hashes objects but never writes them to the CAS.
    /// Phase 2 planning is side-effect free and uses this; every Phase 1
    /// capture path keeps the default `true`.
    pub ingest: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            deadline: None,
            ingest: true,
        }
    }
}

impl ScanOptions {
    /// A scan that ingests new objects into the CAS (every Phase 1 capture path).
    pub fn ingest(deadline: Option<Instant>) -> Self {
        Self {
            deadline,
            ingest: true,
        }
    }

    /// A scan that only reads: objects are hashed but nothing is stored.
    pub fn observe(deadline: Option<Instant>) -> Self {
        Self {
            deadline,
            ingest: false,
        }
    }
}

pub fn scan_workspace(root: &Path, cas: &Cas, options: ScanOptions) -> Result<ScanResult> {
    let mut entries = BTreeMap::new();
    let mut unsupported = Vec::new();
    let mut seen_case = BTreeMap::<String, String>::new();
    visit_directory(
        root,
        root,
        &mut entries,
        &mut unsupported,
        &mut seen_case,
        cas,
        options,
        true,
    )?;
    let manifest = Manifest { entries };
    let state_id = manifest.state_id()?;
    Ok(ScanResult {
        manifest,
        state_id,
        unsupported_paths: unsupported,
    })
}

#[allow(clippy::too_many_arguments)]
fn visit_directory(
    root: &Path,
    directory: &Path,
    entries: &mut BTreeMap<String, Fingerprint>,
    unsupported: &mut Vec<String>,
    seen_case: &mut BTreeMap<String, String>,
    cas: &Cas,
    options: ScanOptions,
    is_root: bool,
) -> Result<Fingerprint> {
    check_deadline(options)?;
    let mut children = BTreeMap::<String, Fingerprint>::new();
    let mut paths = Vec::<PathBuf>::new();
    let read_dir = fs::read_dir(directory).map_err(|error| {
        RewindError::ScanIncomplete(format!("{}: {error}", directory.display()))
    })?;
    for item in read_dir {
        let entry = item.map_err(|error| {
            RewindError::ScanIncomplete(format!("{}: {error}", directory.display()))
        })?;
        if is_root && entry.file_name() == ".rewind" {
            continue;
        }
        paths.push(entry.path());
    }
    paths.sort_by(|left, right| {
        left.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .cmp(&right.file_name().unwrap_or_default().to_string_lossy())
    });

    for path in paths {
        check_deadline(options)?;
        let name = path
            .file_name()
            .ok_or_else(|| RewindError::ScanIncomplete(path.display().to_string()))?
            .to_str()
            .ok_or_else(|| {
                RewindError::ScanIncomplete(format!(
                    "non-Unicode filename is unsupported: {}",
                    path.display()
                ))
            })?
            .to_owned();
        let relative = normalize_relative(root, &path)?;
        register_case_collision(&relative, seen_case)?;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| RewindError::ScanIncomplete(format!("{}: {error}", path.display())))?;
        let file_type = metadata.file_type();
        let reparse_unsupported = reparse_unsupported_fingerprint(&metadata, &file_type, &path);
        let fingerprint = if let Some(unsupported_fingerprint) = reparse_unsupported {
            unsupported.push(relative.clone());
            unsupported_fingerprint
        } else if file_type.is_dir() {
            visit_directory(
                root,
                &path,
                entries,
                unsupported,
                seen_case,
                cas,
                options,
                false,
            )?
        } else if file_type.is_file() {
            let hash = if options.ingest {
                cas.put_file(&path)?
            } else {
                cas.hash_file(&path)?
            };
            let after_metadata = fs::symlink_metadata(&path).map_err(|error| {
                RewindError::ScanIncomplete(format!("{}: {error}", path.display()))
            })?;
            if !after_metadata.file_type().is_file() {
                return Err(RewindError::ScanIncomplete(format!(
                    "object changed type while scanning {}",
                    path.display()
                )));
            }
            Fingerprint::RegularFile {
                content_hash: hash,
                size: after_metadata.len(),
                metadata: metadata_fingerprint(&metadata),
            }
        } else if file_type.is_symlink() {
            let target = fs::read_link(&path).map_err(|error| {
                RewindError::ScanIncomplete(format!("{}: {error}", path.display()))
            })?;
            let target = target
                .to_str()
                .ok_or_else(|| {
                    RewindError::ScanIncomplete(format!(
                        "non-Unicode symlink target is unsupported: {}",
                        path.display()
                    ))
                })?
                .to_owned();
            let target_kind = classify_symlink_target_kind(root, &path, target.as_str());
            let target_hash = blake3::hash(target.as_bytes()).to_hex().to_string();
            Fingerprint::Symlink {
                target,
                target_kind,
                target_hash,
                metadata: metadata_fingerprint(&metadata),
            }
        } else {
            let descriptor = format!("{file_type:?}");
            unsupported.push(relative.clone());
            Fingerprint::Unsupported {
                object_kind: descriptor,
                descriptor: metadata.len().to_string(),
            }
        };
        children.insert(name, fingerprint.clone());
        entries.insert(relative, fingerprint);
    }

    let metadata = fs::symlink_metadata(directory)?;
    let manifest_hash = hash_serialized(&children)?;
    let directory_fingerprint = Fingerprint::Directory {
        manifest_hash,
        entry_count: children.len() as u64,
        metadata: metadata_fingerprint(&metadata),
    };
    if !is_root {
        let relative = normalize_relative(root, directory)?;
        entries.insert(relative, directory_fingerprint.clone());
    }
    Ok(directory_fingerprint)
}

/// Per Phase 0.7 §8.4, only a reparse object *positively identified* as a
/// symbolic link may be treated as SYMLINK. Junctions and any reparse point
/// whose tag cannot be read must be classified as UNSUPPORTED and must never
/// be traversed or replaced. Returns `Some(fingerprint)` when the object must
/// be recorded as unsupported.
/// Public wrapper for archive/copy code that needs the authoritative
/// file-vs-directory flavor of a positively identified Windows symlink.
#[cfg(windows)]
pub fn reparse_symlink_kind(path: &Path) -> Option<crate::model::SymlinkTargetKind> {
    reparse_tag::symlink_kind(path)
}

/// Non-Windows platforms never call this; symlinks there have no flavor.
#[cfg(not(windows))]
pub fn reparse_symlink_kind(_path: &Path) -> Option<crate::model::SymlinkTargetKind> {
    None
}

#[cfg(windows)]
fn reparse_unsupported_fingerprint(
    metadata: &fs::Metadata,
    file_type: &std::fs::FileType,
    path: &Path,
) -> Option<Fingerprint> {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return None;
    }
    if !file_type.is_symlink() {
        return Some(Fingerprint::Unsupported {
            object_kind: "WINDOWS_REPARSE_POINT".to_owned(),
            descriptor: "unclassified reparse point; traversal refused".to_owned(),
        });
    }
    match reparse_tag::read_tag(path) {
        Some(reparse_tag::IO_REPARSE_TAG_SYMLINK) => None,
        Some(reparse_tag::IO_REPARSE_TAG_MOUNT_POINT) => Some(Fingerprint::Unsupported {
            object_kind: "WINDOWS_JUNCTION".to_owned(),
            descriptor: "NTFS junction reparse point; traversal and replacement refused".to_owned(),
        }),
        _ => Some(Fingerprint::Unsupported {
            object_kind: "WINDOWS_REPARSE_POINT".to_owned(),
            descriptor: "reparse point tag unreadable or unrecognized; traversal refused"
                .to_owned(),
        }),
    }
}

#[cfg(not(windows))]
fn reparse_unsupported_fingerprint(
    _metadata: &fs::Metadata,
    _file_type: &std::fs::FileType,
    _path: &Path,
) -> Option<Fingerprint> {
    None
}

/// The reparse tag is not exposed by stable standard-library APIs, so this
/// module reads it through `FSCTL_GET_REPARSE_POINT` on a handle opened with
/// `FILE_FLAG_OPEN_REPARSE_POINT` (the object is never followed). The only
/// unsafe code in the crate lives here; the handle is closed on every return
/// path and the query is read-only.
#[cfg(windows)]
mod reparse_tag {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    pub const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
    pub const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

    // Reparse buffer header: 8 bytes (ReparseTag + ReparseDataLength +
    // Reserved), then the substitute name and print name strings. A file
    // symlink buffer carries 12 bytes of flags before the names; the
    // directory flag is bit 0 of that 32-bit flags word.
    const SYMLINK_FLAG_DIRECTORY: u32 = 0x0001;

    const FSCTL_GET_REPARSE_POINT: u32 = 0x0009_00A8;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const OPEN_EXISTING: u32 = 0x0000_0003;
    const INVALID_HANDLE_VALUE: *mut c_void = usize::MAX as *mut c_void;
    const REPARSE_BUFFER_SIZE: usize = 16 * 1024;

    extern "system" {
        fn CreateFileW(
            filename: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *mut c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template_file: *mut c_void,
        ) -> *mut c_void;
        fn DeviceIoControl(
            device: *mut c_void,
            control_code: u32,
            in_buffer: *mut c_void,
            in_size: u32,
            out_buffer: *mut c_void,
            out_size: u32,
            bytes_returned: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    /// Returns the reparse tag of the object at `path`, or `None` when the
    /// object carries no reparse point or the tag cannot be read.
    pub fn read_tag(path: &Path) -> Option<u32> {
        read_reparse_buffer(path).map(|(tag, _buffer)| tag)
    }

    /// For a positively identified file symlink, returns whether the reparse
    /// data marks it as a directory symlink. This is the authoritative
    /// file-vs-directory distinction for link recreation on Windows.
    pub fn symlink_kind(path: &Path) -> Option<crate::model::SymlinkTargetKind> {
        let (tag, buffer) = read_reparse_buffer(path)?;
        if tag != IO_REPARSE_TAG_SYMLINK || buffer.len() < 12 {
            return None;
        }
        let flags = u32::from_le_bytes([buffer[8], buffer[9], buffer[10], buffer[11]]);
        Some(if flags & SYMLINK_FLAG_DIRECTORY != 0 {
            crate::model::SymlinkTargetKind::Directory
        } else {
            crate::model::SymlinkTargetKind::File
        })
    }

    fn read_reparse_buffer(path: &Path) -> Option<(u32, Vec<u8>)> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut buffer = [0_u8; REPARSE_BUFFER_SIZE];
        let mut returned: u32 = 0;
        unsafe {
            let handle = CreateFileW(
                wide.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null_mut(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
                std::ptr::null_mut(),
            );
            if handle == INVALID_HANDLE_VALUE {
                return None;
            }
            let ok = DeviceIoControl(
                handle,
                FSCTL_GET_REPARSE_POINT,
                std::ptr::null_mut(),
                0,
                buffer.as_mut_ptr().cast::<c_void>(),
                buffer.len() as u32,
                &mut returned,
                std::ptr::null_mut(),
            );
            CloseHandle(handle);
            if ok == 0 || returned < 4 {
                return None;
            }
            Some((
                u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]),
                buffer[..returned as usize].to_vec(),
            ))
        }
    }
}

fn metadata_fingerprint(metadata: &fs::Metadata) -> MetadataFingerprint {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        MetadataFingerprint {
            mode: Some(metadata.permissions().mode() & 0o777),
            readonly: metadata.permissions().readonly(),
        }
    }
    #[cfg(not(unix))]
    {
        MetadataFingerprint {
            mode: None,
            readonly: metadata.permissions().readonly(),
        }
    }
}

/// Determines whether the recorded symlink points at a file-like or
/// directory-like object *without following the link through user-controlled
/// path components*. Two evidence sources are trusted:
///
/// 1. On Windows, the reparse data itself distinguishes file symlinks from
///    directory symlinks, which is authoritative for recreation
///    (`symlink_file` vs `symlink_dir`).
/// 2. On all platforms, when the target resolves inside the workspace root
///    using no-follow component checks, the resolved object's kind is
///    authoritative.
///
/// An external or dangling target is deliberately recorded as `Unknown`;
/// restoring such a link is refused rather than guessed (Fix #5: never
/// infer object type from a filename extension, never follow an escaping
/// target merely to make restoration convenient).
pub fn classify_symlink_target_kind(
    root: &Path,
    link_path: &Path,
    target: &str,
) -> SymlinkTargetKind {
    #[cfg(windows)]
    {
        if let Some(kind) = windows_symlink_target_kind(link_path) {
            return kind;
        }
    }
    if let Some(kind) = workspace_symlink_target_kind(root, link_path, target) {
        return kind;
    }
    SymlinkTargetKind::Unknown
}

/// Windows reparse data distinguishes file symlinks from directory symlinks
/// via SYMLINK_FLAG_DIRECTORY in the reparse buffer.
#[cfg(windows)]
fn windows_symlink_target_kind(link_path: &Path) -> Option<SymlinkTargetKind> {
    reparse_tag::symlink_kind(link_path)
}

/// Resolves the target relative to the link's directory using no-follow
/// semantics; only a target that lands inside the workspace root is
/// classified. Any symlink component in the resolution path, an escaping
/// target, or an unresolvable target yields `None` (recorded as Unknown).
fn workspace_symlink_target_kind(
    root: &Path,
    link_path: &Path,
    target: &str,
) -> Option<SymlinkTargetKind> {
    let link_dir = link_path.parent()?;
    let mut resolved = link_dir.to_path_buf();
    for component in Path::new(target).components() {
        match component {
            std::path::Component::Normal(name) => {
                let candidate = resolved.join(name);
                // No-follow inspection: a symlink component inside the
                // resolution path makes the destination untrusted.
                let metadata = fs::symlink_metadata(&candidate).ok()?;
                if metadata.file_type().is_symlink() {
                    return None;
                }
                resolved = candidate;
            }
            std::path::Component::ParentDir => {
                if !resolved.pop() {
                    return None;
                }
            }
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    if !resolved.starts_with(root) || resolved == root {
        return None;
    }
    let metadata = fs::symlink_metadata(&resolved).ok()?;
    if metadata.file_type().is_dir() {
        Some(SymlinkTargetKind::Directory)
    } else if metadata.is_file() {
        Some(SymlinkTargetKind::File)
    } else {
        None
    }
}

fn hash_serialized<T: Serialize>(value: &T) -> Result<String> {
    let bytes = serde_json::to_vec(value)?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

fn register_case_collision(relative: &str, seen: &mut BTreeMap<String, String>) -> Result<()> {
    let key = if cfg!(any(windows, target_os = "macos")) {
        relative.to_lowercase()
    } else {
        relative.to_owned()
    };
    if let Some(previous) = seen.insert(key, relative.to_owned()) {
        if previous != relative {
            return Err(RewindError::Storage(format!(
                "case-insensitive path collision between {previous} and {relative}"
            )));
        }
    }
    Ok(())
}

fn check_deadline(options: ScanOptions) -> Result<()> {
    if let Some(deadline) = options.deadline {
        if Instant::now() >= deadline {
            return Err(RewindError::ScanIncomplete(
                "scan deadline exceeded".to_owned(),
            ));
        }
    }
    Ok(())
}

pub fn diff_manifests(before: &Manifest, after: &Manifest) -> Vec<Effect> {
    let mut paths = BTreeSet::new();
    paths.extend(before.entries.keys().cloned());
    paths.extend(after.entries.keys().cloned());
    let mut effects = Vec::new();
    for path in paths {
        let pre = before.get(&path);
        let post = after.get(&path);
        if pre == post {
            continue;
        }
        let effect_type = match (&pre, &post) {
            (Fingerprint::Absent, Fingerprint::Absent) => continue,
            (Fingerprint::Absent, _) => EffectType::Create,
            (_, Fingerprint::Absent) => EffectType::Delete,
            (left, right) if left.kind_name() != right.kind_name() => EffectType::TypeChange,
            _ => EffectType::Modify,
        };
        effects.push(Effect {
            path,
            from_path: None,
            effect_type,
            pre,
            post,
        });
    }

    let mut deletions = Vec::new();
    let mut creations = Vec::new();
    for effect in &effects {
        match &effect.effect_type {
            EffectType::Delete => deletions.push(effect),
            EffectType::Create => creations.push(effect),
            _ => {}
        }
    }
    let rename_pairs: Vec<(String, String)> = deletions
        .iter()
        .filter_map(|deletion| {
            creations
                .iter()
                .find(|candidate| {
                    deletion.pre == candidate.post && deletion.pre.is_supported_for_restore()
                })
                .map(|creation| (deletion.path.clone(), creation.path.clone()))
        })
        .collect();
    for (deletion_path, creation_path) in rename_pairs {
        if let Some(effect) = effects
            .iter_mut()
            .find(|candidate| candidate.path == creation_path)
        {
            effect.effect_type = EffectType::Rename;
            effect.from_path = Some(deletion_path.clone());
        }
        if let Some(effect) = effects
            .iter_mut()
            .find(|candidate| candidate.path == deletion_path)
        {
            effect.effect_type = EffectType::Rename;
            effect.from_path = Some(creation_path.clone());
        }
    }
    effects
}

pub fn manifest_fingerprint(manifest: &Manifest, path: &str) -> Fingerprint {
    manifest.get(path)
}

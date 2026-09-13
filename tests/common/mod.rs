//! Shared helpers for the real-filesystem integration suites. The suites
//! drive supervised commands through `Workspace::run_command` and the CLI;
//! the command vectors must be native on every platform (Windows `cmd`
//! scripts, POSIX shell scripts) so GitHub Actions can execute the same
//! behavioral scenarios on Linux and macOS.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

/// Writes a platform-native script under `scratch` and returns the argv
/// that executes it. Windows bodies are `.cmd` files run through
/// `cmd /C`; POSIX bodies are executable `.sh` files run directly.
pub fn shell_script(
    scratch: &Path,
    name: &str,
    windows_body: &str,
    posix_body: &str,
) -> Vec<String> {
    if cfg!(windows) {
        let path = scratch.join(format!("{name}.cmd"));
        fs::write(&path, windows_body.replace('\n', "\r\n")).expect("write windows script");
        vec![
            "cmd".to_owned(),
            "/C".to_owned(),
            path.to_string_lossy().into_owned(),
        ]
    } else {
        let path: PathBuf = scratch.join(format!("{name}.sh"));
        fs::write(&path, posix_body).expect("write posix script");
        make_executable(&path);
        vec![path.to_string_lossy().into_owned()]
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("make script executable");
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

/// The bytes a platform shell's `echo <content>` redirection produces, so
/// content assertions hold on both `cmd` (`CRLF`) and POSIX (`LF`).
pub fn echoed(content: &str) -> Vec<u8> {
    if cfg!(windows) {
        format!("{content}\r\n").into_bytes()
    } else {
        format!("{content}\n").into_bytes()
    }
}

/// Creates a symlink when the platform and privileges permit. Returns
/// `false` (capability skip) only for the documented Windows privilege
/// case; other creation failures panic so they are not silently skipped.
pub fn create_symlink_if_capable(target: &str, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).expect("unix symlink creation");
        return true;
    }
    #[cfg(windows)]
    {
        let target_path = Path::new(target);
        let result = if target_path.extension().is_some() {
            std::os::windows::fs::symlink_file(target, link)
        } else {
            std::os::windows::fs::symlink_dir(target, link)
        };
        match result {
            Ok(()) => true,
            Err(error) => {
                if error.raw_os_error() == Some(1314) {
                    // SeCreateSymbolicLink privilege unavailable.
                    return false;
                }
                panic!("windows symlink creation: {error}");
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        false
    }
}

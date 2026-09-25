//! Windows detached spawn with no handle inheritance.
//!
//! The standard library always calls `CreateProcessW` with
//! `bInheritHandles = TRUE` (rust-lang/rust#73281 is open on this), so a
//! watcher spawned through `std::process::Command` inherits the caller's
//! inheritable handles — including the stdout/stderr pipe handles that a
//! shell's command substitution or `Command::output()` created. A
//! long-lived watcher holding those write ends keeps the caller blocked on
//! an EOF that never arrives. The documented, minimal fix is the platform's
//! own daemonization primitive: one `CreateProcessW` call with
//! `bInheritHandles = FALSE` and NUL std handles, so the watcher starts
//! with an empty handle table except what it opens itself.
//!
//! This is the only `unsafe` in `src/watch/`, following the precedent of
//! `reparse_tag` in `src/scan.rs`: the smallest possible surface, every
//! handle closed on every return path, no traversal, no user data
//! dereferenced beyond the command line.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use crate::error::{Result, RewindError};

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const FILE_SHARE_DELETE: u32 = 0x0000_0004;
const OPEN_EXISTING: u32 = 0x0000_0003;
const DETACHED_PROCESS: u32 = 0x0000_0008;
const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
const INVALID_HANDLE_VALUE: *mut core::ffi::c_void = usize::MAX as *mut core::ffi::c_void;

type Handle = *mut core::ffi::c_void;

#[repr(C)]
struct StartupInfoW {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_count_chars: u32,
    y_count_chars: u32,
    fill_attribute: u32,
    flags: u32,
    show_window: u16,
    reserved2: u16,
    reserved3: *mut u8,
    std_input: Handle,
    std_output: Handle,
    std_error: Handle,
}

#[repr(C)]
struct ProcessInformation {
    process: Handle,
    thread: Handle,
    process_id: u32,
    thread_id: u32,
}

extern "system" {
    fn CreateFileW(
        filename: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *mut core::ffi::c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: Handle,
    ) -> Handle;
    fn CreateProcessW(
        application_name: *const u16,
        command_line: *mut u16,
        process_attributes: *mut core::ffi::c_void,
        thread_attributes: *mut core::ffi::c_void,
        inherit_handles: i32,
        creation_flags: u32,
        environment: *const core::ffi::c_void,
        current_directory: *const u16,
        startup_info: *const StartupInfoW,
        process_information: *mut ProcessInformation,
    ) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
}

/// Quotes one argument the way Windows command-line parsing expects. The
/// arguments built by this crate are executable paths, workspace roots, and
/// numeric flags — none contain quotes — so plain surrounding quotes are
/// sufficient; embedded quotes are rejected rather than guessed at.
fn quote(argument: &str) -> Result<String> {
    if argument.contains('"') {
        return Err(RewindError::Storage(format!(
            "watcher command line argument contains a quote character: {argument}"
        )));
    }
    Ok(format!("\"{argument}\""))
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

/// Spawns `exe arguments` detached: no console, no inherited handles. The
/// child's std handles are NUL. Returns the child's process id.
pub fn spawn_detached_no_inherit(exe: &Path, arguments: &[String]) -> Result<u32> {
    let mut command_line = quote(&exe.to_string_lossy())?;
    for argument in arguments {
        command_line.push(' ');
        command_line.push_str(&quote(argument)?);
    }
    let mut command_line = wide(&command_line);
    let nul = wide("NUL");

    unsafe {
        // One NUL handle serves all three std slots: the watcher never
        // reads or writes its stdio, the handles only need to be valid.
        let std_handle = CreateFileW(
            nul.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if std_handle == INVALID_HANDLE_VALUE {
            return Err(RewindError::Storage(format!(
                "watcher spawn: opening NUL failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        let startup = StartupInfoW {
            cb: std::mem::size_of::<StartupInfoW>() as u32,
            reserved: std::ptr::null_mut(),
            desktop: std::ptr::null_mut(),
            title: std::ptr::null_mut(),
            x: 0,
            y: 0,
            x_size: 0,
            y_size: 0,
            x_count_chars: 0,
            y_count_chars: 0,
            fill_attribute: 0,
            flags: STARTF_USESTDHANDLES,
            show_window: 0,
            reserved2: 0,
            reserved3: std::ptr::null_mut(),
            std_input: std_handle,
            std_output: std_handle,
            std_error: std_handle,
        };
        let mut process = ProcessInformation {
            process: std::ptr::null_mut(),
            thread: std::ptr::null_mut(),
            process_id: 0,
            thread_id: 0,
        };

        // inherit_handles = FALSE: the child inherits nothing. The std
        // handles above are still delivered through STARTUPINFO.
        let ok = CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            DETACHED_PROCESS,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process,
        );
        let _ = CloseHandle(std_handle);
        if ok == 0 {
            let error = std::io::Error::last_os_error();
            return Err(RewindError::Storage(format!(
                "watcher spawn: CreateProcessW failed: {error}"
            )));
        }
        let pid = process.process_id;
        let _ = CloseHandle(process.thread);
        let _ = CloseHandle(process.process);
        Ok(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_rejects_embedded_quotes_and_wraps_paths() {
        assert_eq!(
            quote("D:\\Program Files\\rewind.exe").expect("quote"),
            "\"D:\\Program Files\\rewind.exe\""
        );
        assert!(quote("has\"quote").is_err());
    }

    #[test]
    fn spawn_detached_runs_a_real_process_that_exits_on_its_own() {
        // Spawn this test binary itself with a bogus flag: the SPAWN must
        // succeed and report a pid; the child exits immediately (unknown
        // flag) and we hold no handles that could keep it alive — both
        // process and thread handles are closed on every return path.
        let exe = std::env::current_exe().unwrap();
        let pid = spawn_detached_no_inherit(&exe, &["--nonexistent-flag".to_owned()])
            .expect("spawn detached");
        assert!(pid > 0);
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

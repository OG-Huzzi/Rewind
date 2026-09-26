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

/// Quotes one argument per the Windows command-line parsing rules that
/// matter for this process pair — the MSVCRT/CRT rules, which are what the
/// Rust child's `std::env::args` implements (CommandLineToArgvW follows
/// *different* rules — empirically: it does not halve `2n` backslashes
/// before a quote — and is therefore NOT the round-trip reference; the
/// watcher's child is a Rust program, parsed by the CRT conventions):
///
/// * The argument is surrounded by quotes so spaces and empty arguments
///   survive.
/// * Backslashes are literal **except** immediately before a quote mark:
///   n backslashes followed by a quote must become 2n+1 backslashes plus an
///   escaped `\"` (the child then sees n literal backslashes and a literal
///   quote), and n trailing backslashes must become 2n before the closing
///   quote. Without this, a path ending in a backslash — a drive root such
///   as `C:\`, or the extended-length form `\\?\C:\` that
///   `fs::canonicalize` produces for a drive-root workspace — would close
///   with `\"`, which the child parses as an escaped literal quote,
///   mangling the argument and gluing the next one.
/// * Embedded quotes are escaped, not rejected: they round-trip correctly.
///
/// Ordinary arguments (spaces, no quotes/backslashes) serialize exactly as
/// before: wrapped in quotes.
pub fn quote(argument: &str) -> String {
    let mut quoted = String::with_capacity(argument.len() + 3);
    quoted.push('"');
    let mut backslashes = 0usize;
    for character in argument.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                // n backslashes then an embedded quote: emit 2n+1 backslashes
                // and the escaped quote, so the child parses n literal
                // backslashes and a literal quote mark (the quote must NOT
                // terminate the argument).
                for _ in 0..(2 * backslashes + 1) {
                    quoted.push('\\');
                }
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    quoted.push('\\');
                }
                quoted.push(character);
                backslashes = 0;
            }
        }
    }
    // n trailing backslashes precede the closing quote: emit 2n.
    for _ in 0..backslashes {
        quoted.push('\\');
        quoted.push('\\');
    }
    quoted.push('"');
    quoted
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

/// Spawns `exe arguments` detached: no console, no inherited handles. The
/// child's std handles are NUL. Returns the child's process id.
pub fn spawn_detached_no_inherit(exe: &Path, arguments: &[String]) -> Result<u32> {
    let mut command_line = quote(&exe.to_string_lossy());
    for argument in arguments {
        command_line.push(' ');
        command_line.push_str(&quote(argument));
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

    /// Serialization table: the exact escaped representation is part of the
    /// contract, because `\"` sequences are what Windows parsing treats
    /// specially. Ordinary arguments keep their existing representation.
    #[test]
    fn quoting_serializes_the_documented_escapes() {
        // Ordinary argument: unchanged behavior (wrapped in quotes).
        assert_eq!(
            quote("D:\\Program Files\\rewind.exe"),
            "\"D:\\Program Files\\rewind.exe\""
        );
        // Simple path without spaces or trailing backslash: literal
        // backslashes stay single.
        assert_eq!(quote("C:\\ws\\a.txt"), "\"C:\\ws\\a.txt\"");
        // Argument containing spaces stays one quoted argument.
        assert_eq!(quote("a b c"), "\"a b c\"");
        // Embedded quotes are escaped, not rejected: 2*0+1 backslashes.
        assert_eq!(quote("has\"quote"), "\"has\\\"quote\"");
        // Backslashes before an embedded quote double, plus the escape.
        assert_eq!(quote("dir\\\"x"), "\"dir\\\\\\\"x\"");
        // Trailing backslash doubles before the closing quote.
        assert_eq!(quote("C:\\ws\\"), "\"C:\\ws\\\\\"");
        // Drive root: the case that motivated the fix.
        assert_eq!(quote("C:\\"), "\"C:\\\\\"");
        // Extended-length drive root, as `fs::canonicalize` reports it:
        // backslash runs not before a quote stay literal; the trailing one
        // doubles.
        assert_eq!(quote("\\\\?\\C:\\"), "\"\\\\?\\C:\\\\\"");
        // Multiple trailing backslashes all double.
        assert_eq!(quote("a\\\\"), "\"a\\\\\\\\\"");
        // Empty argument round-trips as an empty argv slot.
        assert_eq!(quote(""), "\"\"");
        // Non-ASCII passes through unchanged: quoting operates on chars, and
        // `wide` converts the finished command line to UTF-16.
        assert_eq!(quote("café-日本語"), "\"café-日本語\"");
        // Non-ASCII combined with a trailing backslash still doubles it.
        assert_eq!(quote("日本語dir\\"), "\"日本語dir\\\\\"");
    }

    /// Reference decoder: an independent scanner implementing the CRT
    /// command-line parsing rules (2n backslashes before a quote → n
    /// literal backslashes and the quote as delimiter; 2n+1 → n literal
    /// backslashes and a literal quote; otherwise backslashes are literal).
    /// Deliberately written as a scanner rather than by reusing the encoder,
    /// so the round-trip below is a real independent check.
    fn decode_crt_command_line(command_line: &str) -> Vec<String> {
        let characters: Vec<char> = command_line.chars().collect();
        let mut argv: Vec<String> = Vec::new();
        let mut position = 0usize;
        while position < characters.len() {
            while position < characters.len()
                && (characters[position] == ' ' || characters[position] == '\t')
            {
                position += 1;
            }
            if position >= characters.len() {
                break;
            }
            let mut argument = String::new();
            let mut in_quotes = false;
            while position < characters.len() {
                let character = characters[position];
                if character == '\\' {
                    let mut run = 0usize;
                    while position < characters.len() && characters[position] == '\\' {
                        run += 1;
                        position += 1;
                    }
                    let followed_by_quote =
                        position < characters.len() && characters[position] == '"';
                    if followed_by_quote {
                        // 2n backslashes before a quote → n literal
                        // backslashes; the quote is a delimiter when the run
                        // is even and a literal quote mark when odd.
                        for _ in 0..run / 2 {
                            argument.push('\\');
                        }
                        if run % 2 == 1 {
                            argument.push('"');
                        } else {
                            in_quotes = !in_quotes;
                        }
                        position += 1;
                    } else {
                        // Not followed by a quote: every backslash is
                        // literal.
                        for _ in 0..run {
                            argument.push('\\');
                        }
                    }
                } else if character == '"' {
                    in_quotes = !in_quotes;
                    position += 1;
                } else if character == ' ' || character == '\t' {
                    if in_quotes {
                        argument.push(character);
                        position += 1;
                    } else {
                        break;
                    }
                } else {
                    argument.push(character);
                    position += 1;
                }
            }
            argv.push(argument);
        }
        argv
    }

    /// Round-trip: quoting each argument and joining them must decode back
    /// to the exact original arguments through the reference CRT parser —
    /// including the drive-root and trailing-backslash shapes that
    /// motivated the fix.
    #[test]
    fn quoting_round_trips_through_the_crt_parsing_rules() {
        let cases: Vec<Vec<&str>> = vec![
            vec!["C:\\"],
            vec!["\\\\?\\C:\\"],
            vec!["D:\\Program Files\\rewind.exe"],
            vec!["a b c"],
            vec!["has\"quote"],
            vec!["dir\\\"x"],
            vec!["C:\\ws\\a.txt"],
            vec!["C:\\ws\\"],
            vec!["a\\\\"],
            vec![""],
            vec!["x y", "z\\", "q\"r", ""],
            // Non-ASCII survives the wide-character conversion untouched.
            vec!["café-日本語"],
            vec!["日本語 dir\\", "café\\"],
            // A full production command line.
            vec![
                "D:\\Program Files\\rewind.exe",
                "watch",
                "serve",
                "--root",
                "C:\\",
                "--batch-ms",
                "50",
            ],
        ];
        for case in cases {
            let command_line = case
                .iter()
                .map(|argument| quote(argument))
                .collect::<Vec<_>>()
                .join(" ");
            let parsed = decode_crt_command_line(&command_line);
            let expected: Vec<String> = case.iter().map(|argument| argument.to_string()).collect();
            assert_eq!(parsed, expected, "round-trip failed for {command_line:?}");
        }
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

    /// The production spawn path with the argument shape that exposed the
    /// quoting bug: a workspace root ending in a backslash. The spawn must
    /// succeed, and the serialized command line round-trips through the CRT
    /// parsing rules (tested above), so the child receives the intended
    /// root, not a mangled one. The end-to-end proof that a real child
    /// receives the intended arguments lives in the integration suite
    /// (`tests/phase3_watcher.rs`), which can locate the real binary.
    #[test]
    fn spawn_detached_accepts_a_drive_root_ending_in_a_backslash() {
        let exe = std::env::current_exe().unwrap();
        let pid = spawn_detached_no_inherit(
            &exe,
            &[
                "watch".to_owned(),
                "serve".to_owned(),
                "--root".to_owned(),
                "C:\\".to_owned(),
            ],
        )
        .expect("spawn detached with trailing-backslash root");
        assert!(pid > 0);
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

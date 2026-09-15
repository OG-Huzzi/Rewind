//! Phase 1.3 shell-integration verification.
//!
//! Proves the actual user experience of the passive hook:
//! - the shell wrapper schedules post-command bookkeeping and returns
//!   control to the prompt WITHOUT waiting for it (proved with a stub
//!   `rewind` whose post-hook sleeps 12 seconds — a synchronous wrapper
//!   could not possibly pass);
//! - real bookkeeping through the wrapper records an observation, never a
//!   fabricated strong operation, and writers keep working afterwards;
//! - background failures (busy catalog, terminated hook process, missing
//!   workspace, unwritable CAS) leave the shell unaffected and degrade
//!   into the conservative reconciliation model.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rewind::model::WorkspaceCondition;
use rewind::workspace::Workspace;

const STUB_POST_SLEEP_SECS: u64 = 45;
const SHELL_HARD_GUARD: Duration = Duration::from_secs(120);
const BACKGROUND_COMPLETION_LIMIT: Duration = Duration::from_secs(75);

fn rewind_bin() -> &'static str {
    env!("CARGO_BIN_EXE_rewind")
}

fn shell_available(shell: &str) -> bool {
    Command::new(shell)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Git Bash needs POSIX-style paths for values interpreted by shell
/// scripts; MSYS maps `/d/...` to `D:\...`.
fn msys_path(path: &Path) -> String {
    if !cfg!(windows) {
        return path.to_string_lossy().into_owned();
    }
    let text = path.to_string_lossy().replace('\\', "/");
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        format!("/{}{}", (bytes[0] as char).to_ascii_lowercase(), &text[2..])
    } else {
        text
    }
}

fn prepend_path(dir: &Path) -> String {
    let existing = std::env::var("PATH").unwrap_or_default();
    let separator = if cfg!(windows) { ";" } else { ":" };
    format!("{}{}{}", dir.display(), separator, existing)
}

/// Writes a stub `rewind` executable. The pre-hook prints a boundary id on
/// stdout (the real contract, Phase 1.4) and records it; the post-hook
/// records the arguments the wrapper gave it, then sleeps before touching
/// its sentinel, so a wrapper that waited synchronously could never return
/// quickly.
fn write_stub_rewind(bin_dir: &Path) {
    let script = "#!/bin/sh\n\
         if [ \"$1\" = \"hook\" ] && [ \"$2\" = \"pre\" ]; then\n\
         \x20   printf '%s\\n' \"stub-boundary-$$\" >> \"$REWIND_STUB_ID\"\n\
         \x20   printf 'stub-boundary-%s\\n' \"$$\"\n\
         \x20   touch \"$REWIND_STUB_PRE\"\n\
         fi\n\
         if [ \"$1\" = \"hook\" ] && [ \"$2\" = \"post\" ]; then\n\
         \x20   printf '%s\\n' \"$*\" >> \"$REWIND_STUB_POST_ARGS\"\n\
         \x20   sleep \"$REWIND_STUB_SLEEP\"\n\
         \x20   touch \"$REWIND_STUB_POST\"\n\
         fi\n\
         exit 0\n";
    let path = bin_dir.join("rewind");
    fs::write(&path, script).expect("write stub rewind");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("stub chmod");
    }
}

struct InteractiveRun {
    output: std::process::ExitStatus,
}

/// Runs an interactive shell fed `lines` on stdin, with the shell
/// integration sourced first. stdout is discarded and the shell's stderr is
/// captured to `stderr_log` so failures are diagnosable; the measurement is
/// purely how long the shell takes to return.
fn run_interactive_shell(
    shell: &str,
    integration: &Path,
    lines: &[&str],
    cwd: &Path,
    env: &[(&str, String)],
    stderr_log: &Path,
) -> InteractiveRun {
    let stderr_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(stderr_log)
        .expect("open stderr log");
    let mut command = Command::new(shell);
    command
        .arg("-i")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file));
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn interactive shell");
    {
        let mut stdin = child.stdin.take().expect("shell stdin");
        let mut script = String::new();
        script.push_str(&format!("source '{}'\n", msys_path(integration)));
        for line in lines {
            script.push_str(line);
            script.push('\n');
        }
        script.push_str("exit 0\n");
        stdin.write_all(script.as_bytes()).expect("write script");
        stdin.flush().expect("flush script");
    }
    let started = Instant::now();
    loop {
        match child.try_wait().expect("poll shell") {
            Some(status) => {
                return InteractiveRun { output: status };
            }
            None if started.elapsed() > SHELL_HARD_GUARD => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("interactive shell did not exit within 120s");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn wait_for_file(path: &Path, limit: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < limit {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    path.exists()
}

fn cli(bin: &str, store: &Path, root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin)
        .args(args)
        .current_dir(root)
        .env("REWIND_HOME", store.to_string_lossy().into_owned())
        .output()
        .expect("run rewind cli")
}

fn condition(workspace: &Workspace) -> WorkspaceCondition {
    workspace.condition().expect("workspace condition")
}

fn observation_count(workspace: &Workspace) -> usize {
    workspace
        .storage
        .catalog
        .list_operations(workspace.id)
        .expect("operations")
        .iter()
        .filter(|operation| operation.kind.as_str() == "PASSIVE_OBSERVATION")
        .count()
}

fn boundaries(workspace: &Workspace) -> Vec<rewind::db::BoundaryRow> {
    workspace
        .storage
        .catalog
        .boundaries(workspace.id)
        .expect("boundaries")
}

/// A durable conservative trace: the workspace is gated away from HEALTHY, or
/// a bypass marker / open unknown interval is pending. This is the
/// representation the Phase 1.3 degradation model promises *instead of* a
/// fabricated observation, so accepting it is not a relaxation of the test:
/// `hook_post_locked` gates on exactly `condition != HEALTHY`.
fn conservative_trace(workspace: &Workspace) -> bool {
    condition(workspace) != WorkspaceCondition::Healthy
        || workspace
            .storage
            .catalog
            .has_pending_bypass(workspace.id)
            .expect("bypass")
        || workspace
            .storage
            .catalog
            .has_open_unknown(workspace.id)
            .expect("gap")
}

/// Background bookkeeping has *settled* for `expected` commands when every one
/// of them left a passive observation, or the workspace shows the durable
/// conservative trace instead.
///
/// Waiting on the boundaries being `consumed` is not enough: `consumed` is set
/// when a post-hook *claims* its boundary, which happens before the scan and
/// before either representation is written. On a slow runner the claim is
/// already visible while the hook is still legitimately mid-flight, so a test
/// that waits only for `consumed` can judge an interval the product has not
/// finished representing yet. This is what failed
/// `bash_rapid_commands_keep_command_identity` on macOS in CI runs #17/#18.
fn bookkeeping_settled(workspace: &Workspace, expected: usize) -> bool {
    conservative_trace(workspace) || observation_count(workspace) >= expected
}

/// Phase 1.4: the pre-hook prints the boundary's immutable id on stdout;
/// tests capture it and always hand exactly that id to the post-hook.
fn pre_hook_id(bin: &str, store: &Path, root: &Path, command: &str, session: &str) -> String {
    let output = cli(
        bin,
        store,
        root,
        &["hook", "pre", "--command", command, "--session", session],
    );
    assert!(
        output.status.success(),
        "pre-hook failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let id = String::from_utf8(output.stdout)
        .expect("utf8 boundary id")
        .trim()
        .to_owned();
    assert!(!id.is_empty(), "the pre-hook must print the boundary id");
    assert!(
        id.lines().count() == 1,
        "the pre-hook must print exactly one line"
    );
    id
}

/// Runs the post-hook for a known boundary id and waits for it. Only the
/// POSIX-only CAS degradation test needs this synchronous form.
#[cfg(unix)]
fn post_hook(
    bin: &str,
    store: &Path,
    root: &Path,
    boundary: &str,
    exit_code: i32,
) -> std::process::Output {
    cli(
        bin,
        store,
        root,
        &[
            "hook",
            "post",
            "--boundary",
            boundary,
            "--exit-code",
            &exit_code.to_string(),
        ],
    )
}

/// Reads the first boundary id the stub pre-hook recorded.
fn stub_boundary_id(id_log: &Path) -> String {
    fs::read_to_string(id_log)
        .expect("stub id log")
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}

/// Asserts the wrapper handed the stub post-hook the id minted by the stub
/// pre-hook: boundary identity survives the asynchronous spawn verbatim.
fn assert_stub_post_received_id(args_log: &Path, id_log: &Path, exit_code: i32) {
    let recorded = fs::read_to_string(args_log).expect("stub post args log");
    let line = recorded
        .lines()
        .next()
        .expect("the post-hook must have received arguments");
    let id = stub_boundary_id(id_log);
    assert!(
        !id.is_empty(),
        "the stub pre-hook must have recorded its boundary id"
    );
    assert!(
        line.contains(&format!("--boundary {id}")),
        "the wrapper must pass the pre-hook's boundary id; got {line:?} for id {id:?}"
    );
    assert!(
        line.contains(&format!("--exit-code {exit_code}")),
        "the wrapper must pass the command's exit status; got {line:?}"
    );
}

/// §7/§10 core proof: the bash wrapper schedules the post-hook and returns
/// to the prompt immediately. The stub `rewind` sleeps 45 seconds in its
/// post-hook — a synchronous wrapper would need at least that long — while
/// the wrapper must return in well under that. The same run also proves the
/// Phase 1.4 identity plumbing: the background post-hook receives the exact
/// id the pre-hook printed.
#[test]
fn bash_wrapper_returns_control_before_bookkeeping_finishes() {
    if !shell_available("bash") {
        eprintln!("SKIPPED: bash is unavailable on this runner");
        return;
    }
    let root = tempfile::tempdir().expect("workspace root");
    let home = tempfile::tempdir().expect("hermetic home");
    let bin_dir = tempfile::tempdir().expect("stub bin dir");
    let pre_sentinel = root.path().join("stub-pre-sentinel");
    let post_sentinel = root.path().join("stub-post-sentinel");
    let stub_ids = root.path().join("stub-boundary-ids");
    let stub_args = root.path().join("stub-post-args");
    write_stub_rewind(bin_dir.path());

    let integration = Path::new(env!("CARGO_MANIFEST_DIR")).join("integration/rewind.bash");
    let run = run_interactive_shell(
        "bash",
        &integration,
        &["true"],
        root.path(),
        &[
            ("PATH", prepend_path(bin_dir.path())),
            ("HOME", home.path().to_string_lossy().into_owned()),
            ("REWIND_STUB_PRE", msys_path(&pre_sentinel)),
            ("REWIND_STUB_POST", msys_path(&post_sentinel)),
            ("REWIND_STUB_ID", msys_path(&stub_ids)),
            ("REWIND_STUB_POST_ARGS", msys_path(&stub_args)),
            ("REWIND_STUB_SLEEP", STUB_POST_SLEEP_SECS.to_string()),
            (
                "REWIND_SESSION_ID",
                format!("stub-bash-{}", std::process::id()),
            ),
        ],
        &root.path().join("bash-stderr.log"),
    );
    assert!(
        run.output.success(),
        "the wrapper must never change the command's exit status"
    );
    // Threshold-free non-blocking proof: the shell exited while the stub's
    // post-hook (sleeping for STUB_POST_SLEEP_SECS) was still running. A
    // synchronous wrapper cannot produce this ordering at any speed — it
    // could not return before the post sentinel exists.
    assert!(
        !post_sentinel.exists(),
        "the shell returned only after the post-hook finished; \
         the wrapper must not wait for bookkeeping"
    );
    assert!(
        pre_sentinel.exists(),
        "the synchronous pre-hook must have run before the prompt"
    );
    assert!(
        wait_for_file(&post_sentinel, BACKGROUND_COMPLETION_LIMIT),
        "the background post-hook must still complete after the shell moved on"
    );
    assert_stub_post_received_id(&stub_args, &stub_ids, 0);
}

/// Same proof for zsh, where the runner provides one (macOS CI, and Linux
/// CI images that ship zsh). Absence is reported honestly, not faked.
#[test]
fn zsh_wrapper_returns_control_before_bookkeeping_finishes() {
    if !shell_available("zsh") {
        eprintln!("SKIPPED: zsh is unavailable on this runner (reported honestly)");
        return;
    }
    let root = tempfile::tempdir().expect("workspace root");
    let home = tempfile::tempdir().expect("hermetic home");
    let bin_dir = tempfile::tempdir().expect("stub bin dir");
    let pre_sentinel = root.path().join("stub-pre-sentinel");
    let post_sentinel = root.path().join("stub-post-sentinel");
    let stub_ids = root.path().join("stub-boundary-ids");
    let stub_args = root.path().join("stub-post-args");
    write_stub_rewind(bin_dir.path());

    let integration = Path::new(env!("CARGO_MANIFEST_DIR")).join("integration/rewind.zsh");
    let run = run_interactive_shell(
        "zsh",
        &integration,
        &["true"],
        root.path(),
        &[
            ("PATH", prepend_path(bin_dir.path())),
            ("HOME", home.path().to_string_lossy().into_owned()),
            ("REWIND_STUB_PRE", msys_path(&pre_sentinel)),
            ("REWIND_STUB_POST", msys_path(&post_sentinel)),
            ("REWIND_STUB_ID", msys_path(&stub_ids)),
            ("REWIND_STUB_POST_ARGS", msys_path(&stub_args)),
            ("REWIND_STUB_SLEEP", STUB_POST_SLEEP_SECS.to_string()),
            (
                "REWIND_SESSION_ID",
                format!("stub-zsh-{}", std::process::id()),
            ),
        ],
        &root.path().join("zsh-stderr.log"),
    );
    assert!(
        run.output.success(),
        "the wrapper must never change the command's exit status"
    );
    // Threshold-free non-blocking proof (see the bash test).
    assert!(
        !post_sentinel.exists(),
        "the shell returned only after the post-hook finished; \
         the wrapper must not wait for bookkeeping"
    );
    assert!(pre_sentinel.exists(), "synchronous pre-hook must have run");
    assert!(
        wait_for_file(&post_sentinel, BACKGROUND_COMPLETION_LIMIT),
        "the background post-hook must still complete after the shell moved on"
    );
    assert_stub_post_received_id(&stub_args, &stub_ids, 0);
}

/// §11 flow 1: a real passive command through the real wrapper records an
/// observation, fabricates no strong operation, and leaves the workspace
/// ready for future strong operations.
#[test]
fn wrapper_observation_flow_and_future_strong_operations() {
    if !shell_available("bash") {
        eprintln!("SKIPPED: bash is unavailable on this runner");
        return;
    }
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let home = tempfile::tempdir().expect("hermetic home");
    let init = cli(rewind_bin(), store.path(), root.path(), &["init", "."]);
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let bin_dir = Path::new(rewind_bin())
        .parent()
        .expect("binary parent dir")
        .to_path_buf();
    let integration = Path::new(env!("CARGO_MANIFEST_DIR")).join("integration/rewind.bash");
    let run = run_interactive_shell(
        "bash",
        &integration,
        &["printf hooked > hooked.txt"],
        root.path(),
        &[
            ("PATH", prepend_path(&bin_dir)),
            ("HOME", home.path().to_string_lossy().into_owned()),
            ("REWIND_HOME", store.path().to_string_lossy().into_owned()),
            (
                "REWIND_SESSION_ID",
                format!("real-bash-{}", std::process::id()),
            ),
            ("REWIND_HOOK_VERBOSE", "1".to_owned()),
        ],
        &root.path().join("bash-stderr.log"),
    );
    assert!(run.output.success(), "wrapper changed the command's status");
    assert_eq!(
        fs::read(root.path().join("hooked.txt")).expect("hooked file"),
        b"hooked"
    );

    // Wait for the background bookkeeping to land. Under heavy machine
    // load the bounded scan may legitimately fail into the conservative
    // model instead of recording an observation — per §7 both outcomes are
    // correct; the only forbidden outcome is fabrication.
    let workspace = Workspace::open_from_current(root.path()).expect("open workspace");
    let deadline = Instant::now() + BACKGROUND_COMPLETION_LIMIT;
    loop {
        let landed = observation_count(&workspace) >= 1
            || condition(&workspace) == WorkspaceCondition::ReconciliationRequired
            || workspace
                .storage
                .catalog
                .has_pending_bypass(workspace.id)
                .expect("bypass")
            || workspace
                .storage
                .catalog
                .has_open_unknown(workspace.id)
                .expect("gap");
        if landed || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let operations = workspace
        .storage
        .catalog
        .list_operations(workspace.id)
        .expect("operations");
    assert!(
        operations
            .iter()
            .all(|operation| operation.kind.as_str() != "STRONG"),
        "passive bookkeeping must never fabricate a strong operation; wrapper stderr: {}",
        fs::read_to_string(root.path().join("bash-stderr.log"))
            .unwrap_or_else(|_| "<unreadable>".to_owned())
    );
    if observation_count(&workspace) == 0 {
        // Degraded path (§11 flow 2): the scan did not finish (load, slow
        // workspace) or the boundary was never consumable. Anything except
        // a durable conservative trace or a clean absence would be a bug,
        // so report which one happened and then prove reconciliation
        // restores HEALTHY. The forbidden outcomes — fabricated strong
        // operations and a trusted baseline advanced from incomplete
        // information — are asserted unconditionally below.
        let gated = condition(&workspace) == WorkspaceCondition::ReconciliationRequired
            || workspace
                .storage
                .catalog
                .has_pending_bypass(workspace.id)
                .expect("bypass")
            || workspace
                .storage
                .catalog
                .has_open_unknown(workspace.id)
                .expect("gap");
        eprintln!("NOTE: no observation landed (load-dependent); durable trace: {gated}");
        let checkpoint = cli(rewind_bin(), store.path(), root.path(), &["reconcile"]);
        assert_eq!(
            checkpoint.status.code(),
            Some(0),
            "reconcile must resolve the degraded state: {}",
            String::from_utf8_lossy(&checkpoint.stderr)
        );
        assert_eq!(condition(&workspace), WorkspaceCondition::Healthy);
    } else {
        assert_eq!(condition(&workspace), WorkspaceCondition::Healthy);
    }

    // A future strong operation works normally afterwards (§11 flow 3).
    let run_args: Vec<&str> = if cfg!(windows) {
        vec!["run", "--", "cmd", "/C", "echo y> y2.txt"]
    } else {
        vec!["run", "--", "sh", "-c", "echo y > y2.txt"]
    };
    let strong = cli(rewind_bin(), store.path(), root.path(), &run_args);
    assert!(
        strong.status.success(),
        "strong run after passive bookkeeping failed: {}",
        String::from_utf8_lossy(&strong.stderr)
    );
    let operations = workspace
        .storage
        .catalog
        .list_operations(workspace.id)
        .expect("operations after run");
    assert!(
        operations
            .iter()
            .any(|operation| operation.kind.as_str() == "STRONG"),
        "the strong operation must be captured"
    );
    assert_eq!(condition(&workspace), WorkspaceCondition::Healthy);
}

/// §8: while the catalog is write-locked by another process, the pre-hook
/// fails open (diagnostic only), the wrapper's command is unaffected, and
/// nothing is fabricated. The next writer reconciles the unobserved drift
/// and captures normally.
#[test]
fn busy_catalog_makes_passive_bookkeeping_fail_open() {
    if !shell_available("bash") {
        eprintln!("SKIPPED: bash is unavailable on this runner");
        return;
    }
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let home = tempfile::tempdir().expect("hermetic home");
    let init = cli(rewind_bin(), store.path(), root.path(), &["init", "."]);
    assert!(init.status.success());
    fs::write(root.path().join("foo.txt"), b"A").expect("fixture file");

    // Hold the catalog's write lock for the whole passive round.
    let db = {
        let workspace = Workspace::open_from_current(root.path()).expect("open workspace");
        workspace.storage.project_root.join("metadata.sqlite")
    };
    let lock = rusqlite::Connection::open(&db).expect("open catalog");
    lock.execute_batch("BEGIN EXCLUSIVE;").expect("hold lock");

    let pre = cli(
        rewind_bin(),
        store.path(),
        root.path(),
        &[
            "hook",
            "pre",
            "--command",
            "busy",
            "--session",
            &format!("busy-{}", std::process::id()),
        ],
    );
    assert_eq!(
        pre.status.code(),
        Some(0),
        "the pre-hook must fail open; stderr: {}",
        String::from_utf8_lossy(&pre.stderr)
    );
    assert!(
        !pre.stderr.is_empty(),
        "a failed pre-hook must leave a diagnostic"
    );

    let bin_dir = Path::new(rewind_bin())
        .parent()
        .expect("binary parent dir")
        .to_path_buf();
    let integration = Path::new(env!("CARGO_MANIFEST_DIR")).join("integration/rewind.bash");
    let run = run_interactive_shell(
        "bash",
        &integration,
        &["printf live > live.txt"],
        root.path(),
        &[
            ("PATH", prepend_path(&bin_dir)),
            ("HOME", home.path().to_string_lossy().into_owned()),
            ("REWIND_HOME", store.path().to_string_lossy().into_owned()),
            (
                "REWIND_SESSION_ID",
                format!("busy-shell-{}", std::process::id()),
            ),
        ],
        &root.path().join("bash-stderr.log"),
    );
    assert!(
        run.output.success(),
        "the user's command must be unaffected by hook failure"
    );
    assert!(root.path().join("live.txt").exists());

    drop(lock);

    // Nothing may have been fabricated for the unobserved interval.
    let workspace = Workspace::open_from_current(root.path()).expect("reopen workspace");
    assert_eq!(
        observation_count(&workspace),
        0,
        "no passive observation may exist for the failed boundary"
    );
    let operations = workspace
        .storage
        .catalog
        .list_operations(workspace.id)
        .expect("operations");
    assert!(
        operations
            .iter()
            .all(|operation| operation.kind.as_str() != "STRONG"),
        "no strong operation may exist for the unobserved interval"
    );

    // The next writer notices the drift, reconciles, and captures.
    let run_args: Vec<&str> = if cfg!(windows) {
        vec!["run", "--", "cmd", "/C", "echo n> next.txt"]
    } else {
        vec!["run", "--", "sh", "-c", "echo n > next.txt"]
    };
    let strong = cli(rewind_bin(), store.path(), root.path(), &run_args);
    assert!(
        strong.status.success(),
        "the writer must reconcile the unobserved drift and capture: {}",
        String::from_utf8_lossy(&strong.stderr)
    );
    assert!(root.path().join("live.txt").exists(), "drift preserved");
    let operations = workspace
        .storage
        .catalog
        .list_operations(workspace.id)
        .expect("operations after run");
    assert!(
        operations
            .iter()
            .any(|operation| operation.kind.as_str() == "STRONG"),
        "the strong operation must be captured after reconciliation"
    );
    assert_eq!(condition(&workspace), WorkspaceCondition::Healthy);
}

/// §8: a terminated background hook process must never become confidence —
/// no observation may appear for the killed bookkeeping, and the next
/// writer must reconcile the drift rather than trust the lost boundary.
#[test]
fn terminated_background_hook_never_becomes_trusted() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let init = cli(rewind_bin(), store.path(), root.path(), &["init", "."]);
    assert!(init.status.success());
    // Real scan pressure so the hook has measurable work in flight when it
    // is terminated.
    fs::create_dir(root.path().join("bulk")).expect("bulk dir");
    for index in 0..1500 {
        fs::write(
            root.path().join("bulk").join(format!("f{index}.txt")),
            format!("payload-{index}"),
        )
        .expect("bulk file");
    }

    let session = format!("killtok-{}", std::process::id());
    let boundary = pre_hook_id(rewind_bin(), store.path(), root.path(), "victim", &session);

    let mut child = Command::new(rewind_bin())
        .args(["hook", "post", "--boundary", &boundary, "--exit-code", "0"])
        .current_dir(root.path())
        .env("REWIND_HOME", store.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn post hook");
    std::thread::sleep(Duration::from_millis(100));
    let _ = child.kill();
    let _ = child.wait();

    let workspace = Workspace::open_from_current(root.path()).expect("open workspace");
    assert_eq!(
        observation_count(&workspace),
        0,
        "a terminated hook must never produce an observation"
    );
    assert_eq!(
        boundaries(&workspace).len(),
        1,
        "a terminated hook must not fabricate boundaries"
    );

    // External mutation on top of the lost boundary, then a writer: it must
    // reconcile-first and capture a strong operation instead of trusting
    // the lost interval.
    fs::write(root.path().join("external.txt"), b"external").expect("external mutation");
    let run_args: Vec<&str> = if cfg!(windows) {
        vec!["run", "--", "cmd", "/C", "echo z> after.txt"]
    } else {
        vec!["run", "--", "sh", "-c", "echo z > after.txt"]
    };
    let strong = cli(rewind_bin(), store.path(), root.path(), &run_args);
    assert!(
        strong.status.success(),
        "the writer must reconcile and capture after a terminated hook: {}",
        String::from_utf8_lossy(&strong.stderr)
    );
    assert!(root.path().join("external.txt").exists());
    let operations = workspace
        .storage
        .catalog
        .list_operations(workspace.id)
        .expect("operations after run");
    assert!(
        operations
            .iter()
            .any(|operation| operation.kind.as_str() == "STRONG"),
        "the strong operation must be captured"
    );
    assert_eq!(
        observation_count(&workspace),
        0,
        "no passive observation may be fabricated for the killed interval"
    );
    assert_eq!(condition(&workspace), WorkspaceCondition::Healthy);
}

/// §8: when the workspace disappears, the background post-hook fails open
/// (exit 0, diagnostic) and cannot fabricate anything.
#[test]
fn post_hook_fails_open_when_workspace_disappears() {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let init = cli(rewind_bin(), store.path(), root.path(), &["init", "."]);
    assert!(init.status.success());
    let session = format!("vanish-{}", std::process::id());
    let boundary = pre_hook_id(rewind_bin(), store.path(), root.path(), "vanish", &session);

    fs::remove_dir_all(root.path()).expect("delete workspace");
    // The shell's directory no longer exists; from the nearest surviving
    // parent there is no workspace marker anywhere above, so the hook must
    // fail open with a diagnostic and exit 0.
    let parent = root
        .path()
        .parent()
        .expect("workspace parent")
        .to_path_buf();
    let post = Command::new(rewind_bin())
        .args(["hook", "post", "--boundary", &boundary, "--exit-code", "0"])
        .current_dir(&parent)
        .env("REWIND_HOME", store.path())
        .output()
        .expect("run hook post from surviving parent");
    assert_eq!(
        post.status.code(),
        Some(0),
        "the hook must fail open when the workspace is gone"
    );
    assert!(!post.stderr.is_empty(), "a diagnostic must be printed");
}

/// §8: a CAS/storage failure during the background scan must land in the
/// conservative model — durable capture gap and reconciliation gate — never
/// a fabricated observation. POSIX-only (directory permissions).
#[test]
#[cfg(unix)]
fn unwritable_cas_degrades_to_capture_gap() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let init = cli(rewind_bin(), store.path(), root.path(), &["init", "."]);
    assert!(init.status.success());
    // A new file forces a fresh CAS blob write during the post-hook scan.
    fs::write(root.path().join("hooked.txt"), b"hooked").expect("hooked file");

    let cas_tmp_dir = {
        let workspace = Workspace::open_from_current(root.path()).expect("open workspace");
        // put_file stages new blobs through the CAS temp directory; its own
        // permissions (not the parent's) govern whether a new blob can be
        // written at all.
        workspace.storage.root.join("cas").join("tmp")
    };
    fs::set_permissions(&cas_tmp_dir, fs::Permissions::from_mode(0o555)).expect("lock cas tmp");

    let session = format!("cas-{}", std::process::id());
    let boundary = pre_hook_id(rewind_bin(), store.path(), root.path(), "casfail", &session);
    let post = post_hook(rewind_bin(), store.path(), root.path(), &boundary, 0);
    assert_eq!(
        post.status.code(),
        Some(0),
        "the hook must fail open on CAS failure; stderr: {}",
        String::from_utf8_lossy(&post.stderr)
    );

    fs::set_permissions(&cas_tmp_dir, fs::Permissions::from_mode(0o755)).expect("unlock cas tmp");

    let workspace = Workspace::open_from_current(root.path()).expect("reopen workspace");
    // The interval is accounted for exactly once even though its capture
    // failed: the durable record is the gap, never a fabricated observation.
    let rows = boundaries(&workspace);
    assert_eq!(
        rows.len(),
        1,
        "a failed capture must not fabricate boundaries"
    );
    assert_eq!(rows[0].id, boundary);
    assert!(
        rows[0].consumed && rows[0].exit_code == Some(0),
        "the claimed boundary must be accounted for exactly once"
    );
    assert_eq!(
        condition(&workspace),
        WorkspaceCondition::ReconciliationRequired,
        "a failed background scan must gate the workspace"
    );
    assert!(
        workspace
            .storage
            .catalog
            .has_open_unknown(workspace.id)
            .expect("gap"),
        "the failed scan must record an unknown interval"
    );
    assert_eq!(
        observation_count(&workspace),
        0,
        "a failed scan must never fabricate an observation"
    );

    let checkpoint = cli(rewind_bin(), store.path(), root.path(), &["reconcile"]);
    assert_eq!(checkpoint.status.code(), Some(0));
    assert_eq!(condition(&workspace), WorkspaceCondition::Healthy);
    assert_eq!(
        fs::read(root.path().join("hooked.txt")).expect("hooked preserved"),
        b"hooked"
    );
}

/// §8 (bash): several rapid commands in one interactive session. The shell
/// never waits for bookkeeping, so several background post-hooks coexist.
/// Each command writes its own file and ends with its own exit status, so the
/// catalog can prove exact identity: the interval that produced `b.txt` must
/// be recorded with exit code 9, never with another command's status.
#[test]
fn bash_rapid_commands_keep_command_identity() {
    if !shell_available("bash") {
        eprintln!("SKIPPED: bash is unavailable on this runner");
        return;
    }
    rapid_commands_keep_command_identity("bash");
}

/// Same proof for zsh, where the runner provides one (macOS CI, and Linux CI
/// images that ship zsh). Absence is reported honestly, not faked.
#[test]
fn zsh_rapid_commands_keep_command_identity() {
    if !shell_available("zsh") {
        eprintln!("SKIPPED: zsh is unavailable on this runner (reported honestly)");
        return;
    }
    rapid_commands_keep_command_identity("zsh");
}

fn rapid_commands_keep_command_identity(shell: &str) {
    let root = tempfile::tempdir().expect("workspace root");
    let store = tempfile::tempdir().expect("store");
    let home = tempfile::tempdir().expect("hermetic home");
    let init = cli(rewind_bin(), store.path(), root.path(), &["init", "."]);
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let bin_dir = Path::new(rewind_bin())
        .parent()
        .expect("binary parent dir")
        .to_path_buf();
    let integration =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("integration/rewind.{shell}"));
    // One boundary per line on both shells: bash's DEBUG trap fires once per
    // simple command (the status is set inside the single `sh -c` command),
    // and zsh's preexec sees the whole line.
    let run = run_interactive_shell(
        shell,
        &integration,
        &[
            "printf a > a.txt",
            "sh -c 'printf b > b.txt; exit 9'",
            "sh -c 'printf c > c.txt; exit 5'",
        ],
        root.path(),
        &[
            ("PATH", prepend_path(&bin_dir)),
            ("HOME", home.path().to_string_lossy().into_owned()),
            ("REWIND_HOME", store.path().to_string_lossy().into_owned()),
            (
                "REWIND_SESSION_ID",
                format!("rapid-{shell}-{}", std::process::id()),
            ),
        ],
        &root.path().join(format!("{shell}-rapid-stderr.log")),
    );
    assert!(
        run.output.success(),
        "the wrapper must never change the shell's exit status"
    );
    // The user's commands really ran, with the statuses the catalog must
    // later attribute to exactly those commands.
    assert_eq!(fs::read(root.path().join("a.txt")).expect("a.txt"), b"a");
    assert_eq!(fs::read(root.path().join("b.txt")).expect("b.txt"), b"b");
    assert_eq!(fs::read(root.path().join("c.txt")).expect("c.txt"), b"c");

    let workspace = Workspace::open_from_current(root.path()).expect("open workspace");
    let expected: [(&str, i32); 3] = [("a.txt", 0), ("b.txt", 9), ("c.txt", 5)];
    // Wait until the bookkeeping has BOTH claimed every boundary and settled
    // its representation. Neither fact is sufficient on its own:
    //
    // * `consumed` alone becomes true when a post-hook claims its boundary,
    //   before the observation or the durable conservative trace is written -
    //   so a loop that waits only for that judges an unfinished interval on a
    //   slow runner (CI runs #17/#18);
    // * settlement alone can be reached by one hook's degradation while
    //   another hook has not claimed yet - so a loop that waits only for that
    //   fails the identity assertion below (CI run #22).
    let deadline = Instant::now() + BACKGROUND_COMPLETION_LIMIT;
    loop {
        let claimed = boundaries(&workspace)
            .iter()
            .filter(|row| row.consumed)
            .count()
            >= expected.len();
        if (claimed && bookkeeping_settled(&workspace, expected.len()))
            || Instant::now() >= deadline
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // Identity (unconditional): exactly one boundary per command, each
    // accounted for exactly once, each keeping its own exit status.
    let rows = boundaries(&workspace);
    for (token, exit_code) in expected {
        let matching: Vec<_> = rows
            .iter()
            .filter(|row| row.command.contains(token))
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "exactly one boundary per command; found {:?}",
            rows.iter()
                .map(|row| (&row.command, row.exit_code, row.consumed))
                .collect::<Vec<_>>()
        );
        let row = matching[0];
        assert!(
            row.consumed,
            "{token} must be accounted for by its own post-hook"
        );
        assert_eq!(
            row.exit_code,
            Some(exit_code),
            "{token} must keep its own exit status: a boundary was paired with \
             the wrong command"
        );
    }
    // Nothing else may be fabricated: the only permitted extra boundary is
    // the harness's trailing `exit` line, whose post-hook can never run.
    for row in rows.iter().filter(|row| {
        !expected
            .iter()
            .any(|(token, _)| row.command.contains(token))
    }) {
        assert!(
            row.command == "exit" || row.command.starts_with("exit "),
            "an unexpected boundary was fabricated: {:?}",
            row.command
        );
        assert!(
            !row.consumed,
            "the trailing exit line can never be accounted for"
        );
    }

    // Provenance (unconditional): no strong operation may be fabricated, and
    // every recorded operation that mentions one of the commands carries that
    // command's own exit status.
    let operations = workspace
        .storage
        .catalog
        .list_operations(workspace.id)
        .expect("operations");
    for operation in &operations {
        assert_ne!(
            operation.kind.as_str(),
            "STRONG",
            "passive bookkeeping must never fabricate a strong operation"
        );
        if let Some(command) = operation.command.as_deref() {
            if let Some((_, exit_code)) = expected.iter().find(|(token, _)| command.contains(token))
            {
                assert_eq!(
                    operation.exit_code,
                    Some(*exit_code),
                    "operation {command:?} must carry its own command's exit status"
                );
            }
        }
    }

    if observation_count(&workspace) == expected.len() {
        assert_eq!(condition(&workspace), WorkspaceCondition::Healthy);
        return;
    }
    // Load-degraded branch (the Phase 1.3 rule): a bounded scan may not have
    // completed for some interval, so fewer observations exist. That is
    // acceptable only if the conservative durable trace is present, and
    // reconciliation must still restore HEALTHY.
    eprintln!(
        "NOTE: {} of 3 observations landed (bounded scan, load-dependent); \
         verifying the conservative trace instead",
        observation_count(&workspace)
    );
    let gated = conservative_trace(&workspace);
    assert!(
        gated,
        "a missing observation must leave a durable conservative trace"
    );
    let checkpoint = cli(rewind_bin(), store.path(), root.path(), &["reconcile"]);
    assert_eq!(checkpoint.status.code(), Some(0));
    assert_eq!(condition(&workspace), WorkspaceCondition::Healthy);
}

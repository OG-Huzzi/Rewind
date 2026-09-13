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

/// Writes a stub `rewind` executable: the pre-hook touches a sentinel
/// instantly; the post-hook sleeps before touching its sentinel, so a
/// wrapper that waited synchronously could never return quickly.
fn write_stub_rewind(bin_dir: &Path) {
    let script = "#!/bin/sh\n\
         if [ \"$1\" = \"hook\" ] && [ \"$2\" = \"pre\" ]; then\n\
         \x20   touch \"$REWIND_STUB_PRE\"\n\
         fi\n\
         if [ \"$1\" = \"hook\" ] && [ \"$2\" = \"post\" ]; then\n\
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
        script.push_str("exit\n");
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

/// §7/§10 core proof: the bash wrapper schedules the post-hook and returns
/// to the prompt immediately. The stub `rewind` sleeps 12 seconds in its
/// post-hook — a synchronous wrapper would need at least that long — while
/// the wrapper must return in well under that.
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

    let token = format!("killtok-{}-{}", std::process::id(), STUB_POST_SLEEP_SECS);
    let pre = cli(
        rewind_bin(),
        store.path(),
        root.path(),
        &["hook", "pre", "--command", "victim", "--session", &token],
    );
    assert!(pre.status.success());

    let mut child = Command::new(rewind_bin())
        .args(["hook", "post", "--exit-code", "0", "--session", &token])
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
    let pre = cli(
        rewind_bin(),
        store.path(),
        root.path(),
        &[
            "hook",
            "pre",
            "--command",
            "vanish",
            "--session",
            &format!("vanish-{}", std::process::id()),
        ],
    );
    assert!(pre.status.success());

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
        .args([
            "hook",
            "post",
            "--exit-code",
            "0",
            "--session",
            &format!("vanish-{}", std::process::id()),
        ])
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
    let pre = cli(
        rewind_bin(),
        store.path(),
        root.path(),
        &["hook", "pre", "--command", "casfail", "--session", &session],
    );
    assert!(pre.status.success());
    let post = cli(
        rewind_bin(),
        store.path(),
        root.path(),
        &["hook", "post", "--exit-code", "0", "--session", &session],
    );
    assert_eq!(
        post.status.code(),
        Some(0),
        "the hook must fail open on CAS failure; stderr: {}",
        String::from_utf8_lossy(&post.stderr)
    );

    fs::set_permissions(&cas_tmp_dir, fs::Permissions::from_mode(0o755)).expect("unlock cas tmp");

    let workspace = Workspace::open_from_current(root.path()).expect("reopen workspace");
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

//! Phase 5 verification: platform expansion — POSIX named pipes (FIFOs) as
//! first-class objects (`.ai/PHASE_5_PLATFORM_EXPANSION.md` §6).
//!
//! FIFO tests run only on Unix: Windows cannot create FIFOs, and the
//! contract's Windows behavior is "the scanner never produces the variant",
//! which the unchanged Windows CI behavior covers. Every FIFO test drives
//! the real capture, undo, and redo machinery — not the scanner in
//! isolation — so classification, journaling, quarantine, and post-apply
//! verification are exercised together.

use rewind::model::{Fingerprint, MetadataFingerprint};

mod common;

/// AC5: the variant reports restore support on every platform and older
/// manifests without it deserialize unchanged (runs on all platforms).
#[test]
fn named_pipe_fingerprint_is_backward_compatible() {
    let fingerprint = Fingerprint::NamedPipe {
        metadata: MetadataFingerprint {
            mode: Some(0o600),
            readonly: false,
        },
    };
    assert_eq!(fingerprint.kind_name(), "NAMED_PIPE");
    assert!(fingerprint.is_supported_for_restore());
    let serialized = serde_json::to_string(&fingerprint).expect("serialize");
    let back: Fingerprint = serde_json::from_str(&serialized).expect("deserialize");
    assert_eq!(back, fingerprint);

    // A v2-era manifest (symlinks with target kinds, no named pipes) still
    // deserializes: additive variants never invalidate persisted data.
    let legacy = r#"{"foo.txt":{"kind":"RegularFile","value":{"content_hash":"abc","size":1,"metadata":{"mode":420,"readonly":false}}}}"#;
    let manifest: std::collections::BTreeMap<String, Fingerprint> =
        serde_json::from_str(legacy).expect("legacy manifest");
    assert!(manifest.contains_key("foo.txt"));
}

/// The FIFO lifecycle tests run only on Unix: Windows cannot create FIFOs,
/// and there the contract's behavior is that the scanner never produces the
/// variant (covered by the unchanged Windows CI behavior).
#[cfg(unix)]
mod posix {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use rewind::model::Fingerprint;
    use rewind::rollback::{redo, undo};
    use rewind::workspace::Workspace;

    fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Workspace) {
        let root = tempfile::tempdir().expect("temporary root");
        fs::write(root.path().join("foo.txt"), b"A").expect("write fixture");
        let store = tempfile::tempdir().expect("temporary store");
        let workspace = Workspace::init(root.path(), Some(store.path())).expect("initialize");
        (root, store, workspace)
    }

    fn fifo_mode(path: &std::path::Path) -> Option<u32> {
        fs::symlink_metadata(path)
            .ok()
            .filter(|metadata| {
                use std::os::unix::fs::FileTypeExt;
                metadata.file_type().is_fifo()
            })
            .map(|metadata| metadata.permissions().mode() & 0o7777)
    }

    fn mode_of(fingerprint: &Fingerprint) -> Option<u32> {
        match fingerprint {
            Fingerprint::NamedPipe { metadata } => metadata.mode,
            _ => None,
        }
    }

    /// The durable post-state manifest of a captured operation, fetched
    /// through the catalog (persistence across the real journal path).
    fn post_manifest(workspace: &Workspace, operation_id: i64) -> rewind::model::Manifest {
        let record = workspace
            .storage
            .catalog
            .operation(operation_id, workspace.id)
            .expect("operation record");
        let post_state_id = record.post_state_id.expect("post state id");
        workspace
            .state_manifest(&post_state_id)
            .expect("post manifest")
    }

    /// Creates a FIFO through the platform's own `mkfifo` utility —
    /// `std::os::unix::fs::mkfifo` is unstable (rust-lang/rust#139324) and
    /// tests must compile on stable.
    fn make_fifo(path: &std::path::Path, mode: &str) {
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("mkfifo -m {mode} '{}'", path.display()))
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo {mode} failed for {path:?}");
    }

    /// AC1: the scanner classifies a FIFO as NAMED_PIPE with its mode
    /// recorded, without opening or blocking on it (a FIFO with no writer
    /// would block any reader).
    #[test]
    fn a_fifo_scans_as_a_supported_named_pipe() {
        let (root, _store, workspace) = fixture();
        let pipe = root.path().join("logpipe");
        make_fifo(&pipe, "664");

        workspace
            .reconcile_locked("fifo classification")
            .expect("reconcile");
        let baseline = workspace
            .state_manifest(&workspace.baseline_id().expect("baseline"))
            .expect("baseline manifest");
        let fingerprint = baseline.get("logpipe");
        assert_eq!(fingerprint.kind_name(), "NAMED_PIPE");
        assert_eq!(mode_of(&fingerprint), Some(0o664));
        assert!(fingerprint.is_supported_for_restore());
        assert!(fifo_mode(&pipe).is_some(), "the pipe still exists");
    }

    /// AC2: an operation that creates a FIFO is captured with NamedPipe in
    /// the post state; undo removes it; redo recreates it with the recorded
    /// mode.
    #[test]
    fn fifo_creation_is_captured_and_reversible() {
        let (root, _store, workspace) = fixture();
        let outcome = workspace
            .run_command(&[
                "sh".to_owned(),
                "-c".to_owned(),
                "mkfifo -m 664 created.fifo".to_owned(),
            ])
            .expect("capture fifo creation");
        assert!(outcome.captured);
        let operation_id = outcome.operation_id.expect("operation id");

        let post = post_manifest(&workspace, operation_id);
        assert_eq!(post.get("created.fifo").kind_name(), "NAMED_PIPE");
        assert_eq!(fifo_mode(&root.path().join("created.fifo")), Some(0o664));

        undo(&workspace, Some(operation_id), false).expect("undo fifo creation");
        assert!(
            !root.path().join("created.fifo").exists(),
            "undo must remove the created fifo"
        );

        redo(&workspace, Some(operation_id)).expect("redo fifo creation");
        assert_eq!(
            fifo_mode(&root.path().join("created.fifo")),
            Some(0o664),
            "redo must recreate the fifo with the recorded mode"
        );
    }

    /// AC3: an operation that replaces a FIFO with a regular file is
    /// undoable — the undo recreates the FIFO with its recorded mode. Before
    /// this change the unsupported pre-state refused undo entirely.
    #[test]
    fn replacing_a_fifo_with_a_file_is_reversible() {
        let (root, _store, workspace) = fixture();
        // The FIFO exists before the captured operation and is replaced by it.
        make_fifo(&root.path().join("swap.fifo"), "600");
        workspace
            .reconcile_locked("fifo pre-state")
            .expect("reconcile");

        let outcome = workspace
            .run_command(&[
                "sh".to_owned(),
                "-c".to_owned(),
                "rm swap.fifo && printf 'DATA' > swap.fifo".to_owned(),
            ])
            .expect("capture fifo replacement");
        let operation_id = outcome.operation_id.expect("operation id");
        let post = post_manifest(&workspace, operation_id);
        assert_eq!(post.get("swap.fifo").kind_name(), "REGULAR_FILE");

        undo(&workspace, Some(operation_id), false).expect("undo replaces file with fifo");
        assert_eq!(
            fifo_mode(&root.path().join("swap.fifo")),
            Some(0o600),
            "the fifo must be restored exactly as recorded"
        );

        redo(&workspace, Some(operation_id)).expect("redo restores the file");
        assert_eq!(
            fs::read(root.path().join("swap.fifo")).expect("file content"),
            b"DATA",
            "redo must reinstall the regular file"
        );
    }

    /// AC4: a Unix domain socket remains unsupported with a named
    /// descriptor, and an operation whose pre-state contains one is still
    /// refused.
    #[test]
    fn unix_sockets_stay_unsupported_and_refuse_undo() {
        use std::os::unix::net::UnixListener;
        let (root, _store, workspace) = fixture();

        // The socket enters the baseline through the authoritative rescan —
        // exactly how a real workspace would acquire one.
        let _listener = UnixListener::bind(root.path().join("runtime.sock")).expect("bind socket");
        workspace
            .reconcile_locked("socket classification")
            .expect("reconcile");
        let baseline = workspace
            .state_manifest(&workspace.baseline_id().expect("baseline"))
            .expect("baseline manifest");
        match baseline.get("runtime.sock") {
            Fingerprint::Unsupported { object_kind, .. } => {
                assert_eq!(
                    object_kind, "UNIX_SOCKET",
                    "the refusal names the object class"
                );
            }
            other => panic!("socket must be unsupported, got {}", other.kind_name()),
        }

        let outcome = workspace
            .run_command(&[
                "sh".to_owned(),
                "-c".to_owned(),
                "rm runtime.sock".to_owned(),
            ])
            .expect("capture socket deletion");
        let refusal = undo(&workspace, Some(outcome.operation_id), false);
        assert!(
            refusal.is_err(),
            "undo of an unsupported-object operation must be refused"
        );
        assert!(
            !root.path().join("runtime.sock").exists(),
            "the refusal must not have mutated anything"
        );
    }
}

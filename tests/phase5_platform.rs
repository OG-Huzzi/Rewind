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
        let refusal = undo(&workspace, outcome.operation_id, false);
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

/// AC6: the junction variant reports its kind name and restore support on
/// every platform, round-trips through serde, and older manifests without
/// it deserialize unchanged (additive variant, no schema bump).
#[test]
fn junction_fingerprint_is_backward_compatible() {
    let fingerprint = Fingerprint::Junction {
        substitute: "\\??\\C:\\data".to_owned(),
        print_name: "C:\\data".to_owned(),
        target_hash: "abc123".to_owned(),
        metadata: MetadataFingerprint {
            mode: None,
            readonly: false,
        },
    };
    assert_eq!(fingerprint.kind_name(), "WINDOWS_JUNCTION");
    assert!(fingerprint.is_supported_for_restore());
    assert!(fingerprint.content_hash().is_none());
    let serialized = serde_json::to_string(&fingerprint).expect("serialize");
    let back: Fingerprint = serde_json::from_str(&serialized).expect("deserialize");
    assert_eq!(back, fingerprint);

    // A pre-junction manifest (Phase 5 slice 1 era) still deserializes:
    // additive variants never invalidate persisted data.
    let legacy = r#"{"foo.txt":{"kind":"RegularFile","value":{"content_hash":"abc","size":1,"metadata":{"mode":420,"readonly":false}}}}"#;
    let manifest: std::collections::BTreeMap<String, Fingerprint> =
        serde_json::from_str(legacy).expect("legacy manifest");
    assert!(manifest.contains_key("foo.txt"));
}

/// AC1 (all platforms): the stream set is an additive, skipped-when-empty
/// component of the regular-file fingerprint. A stream-free file serializes
/// byte-identically to the pre-slice form (no drift for existing
/// workspaces), a pre-slice manifest without the field still deserializes,
/// and a stream-carrying fingerprint round-trips.
#[test]
fn stream_fingerprints_are_backward_compatible() {
    use std::collections::BTreeMap;

    let stream_free = Fingerprint::RegularFile {
        content_hash: "abc".to_owned(),
        size: 1,
        metadata: MetadataFingerprint {
            mode: Some(0o644),
            readonly: false,
        },
        streams: BTreeMap::new(),
    };
    let serialized = serde_json::to_string(&stream_free).expect("serialize");
    assert_eq!(
        serialized,
        // Byte-identical to the pre-slice form: the empty stream set is
        // skipped, so existing state ids do not drift.
        r#"{"kind":"RegularFile","value":{"content_hash":"abc","size":1,"metadata":{"mode":420,"readonly":false}}}"#,
        "a stream-free file must serialize exactly as before the slice"
    );
    let back: Fingerprint = serde_json::from_str(&serialized).expect("deserialize");
    assert_eq!(back, stream_free);

    // A pre-slice manifest (no streams field) still deserializes, with an
    // empty stream set.
    let legacy = r#"{"kind":"RegularFile","value":{"content_hash":"abc","size":1,"metadata":{"mode":420,"readonly":false}}}"#;
    let legacy: Fingerprint = serde_json::from_str(legacy).expect("legacy deserialize");
    match &legacy {
        Fingerprint::RegularFile { streams, .. } => assert!(streams.is_empty()),
        other => panic!("expected a regular file, got {}", other.kind_name()),
    }

    // A stream-carrying fingerprint round-trips through serde.
    let mut streams = BTreeMap::new();
    streams.insert("Zone.Identifier".to_owned(), "deadbeef".to_owned());
    streams.insert("empty".to_owned(), "feedface".to_owned());
    let carrying = Fingerprint::RegularFile {
        content_hash: "abc".to_owned(),
        size: 1,
        metadata: MetadataFingerprint {
            mode: None,
            readonly: false,
        },
        streams,
    };
    let serialized = serde_json::to_string(&carrying).expect("serialize");
    let back: Fingerprint = serde_json::from_str(&serialized).expect("deserialize");
    assert_eq!(back, carrying);
    assert!(carrying.describe().contains("streams=2"));
}

/// The named-stream lifecycle tests run only on Windows: that is the only
/// platform whose filesystem produces named streams, and the mechanics were
/// probe-verified against a real NTFS volume before implementation
/// (`.ai/PHASE_5_ALTERNATE_DATA_STREAMS.md` §2). Every test drives the real
/// capture, undo, and redo machinery.
#[cfg(windows)]
mod streams {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    use rewind::model::Fingerprint;
    use rewind::rollback::{redo, undo};
    use rewind::scan::{named_streams, stream_spec};
    use rewind::workspace::Workspace;

    fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Workspace) {
        let root = tempfile::tempdir().expect("temporary root");
        fs::write(root.path().join("host.txt"), b"MAIN").expect("write fixture");
        let store = tempfile::tempdir().expect("temporary store");
        let workspace = Workspace::init(root.path(), Some(store.path())).expect("initialize");
        (root, store, workspace)
    }

    fn write_stream(root: &Path, file: &str, name: &str, content: &[u8]) {
        let spec = stream_spec(&root.join(file), name);
        fs::write(&spec, content).unwrap_or_else(|error| panic!("write stream {name}: {error}"));
    }

    fn read_stream(root: &Path, file: &str, name: &str) -> Vec<u8> {
        let spec = stream_spec(&root.join(file), name);
        fs::read(&spec).unwrap_or_else(|error| panic!("read stream {name}: {error}"))
    }

    fn streams_of(fingerprint: &Fingerprint) -> BTreeMap<String, String> {
        match fingerprint {
            Fingerprint::RegularFile { streams, .. } => streams.clone(),
            other => panic!("expected a regular file, got {}", other.kind_name()),
        }
    }

    fn baseline_manifest(workspace: &Workspace) -> rewind::model::Manifest {
        workspace
            .state_manifest(&workspace.baseline_id().expect("baseline"))
            .expect("baseline manifest")
    }

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

    /// AC2: named streams are captured with their exact bytes in the CAS;
    /// every stream name and hash is recorded; a stream-free file records an
    /// omitted (empty) stream set.
    #[test]
    fn named_streams_are_captured_into_the_fingerprint() {
        let (root, _store, workspace) = fixture();
        write_stream(
            root.path(),
            "host.txt",
            "Zone.Identifier",
            b"[ZoneTransfer]\r\nZoneId=3\r\n",
        );
        write_stream(root.path(), "host.txt", "empty", b"");
        write_stream(root.path(), "host.txt", "has space.dot", b"O");

        workspace
            .reconcile_locked("stream classification")
            .expect("reconcile");
        let baseline = baseline_manifest(&workspace);
        let recorded = streams_of(&baseline.get("host.txt"));
        assert_eq!(
            recorded.len(),
            3,
            "every named stream is recorded: {recorded:?}"
        );
        assert!(recorded.contains_key("Zone.Identifier"));
        assert!(recorded.contains_key("empty"));
        assert!(recorded.contains_key("has space.dot"));

        // Each recorded hash is the CAS id of the exact stream bytes; the
        // objects verify and read back byte-for-byte.
        for (name, hash) in &recorded {
            let bytes = workspace.storage.cas.read_bytes(hash).expect("CAS bytes");
            let expected = match name.as_str() {
                "Zone.Identifier" => b"[ZoneTransfer]\r\nZoneId=3\r\n".as_slice(),
                "empty" => b"".as_slice(),
                "has space.dot" => b"O".as_slice(),
                other => panic!("unexpected stream {other}"),
            };
            assert_eq!(bytes, expected, "CAS bytes of stream {name}");
        }

        // The default stream is the file's own content and stays untouched.
        match baseline.get("host.txt") {
            Fingerprint::RegularFile {
                content_hash, size, ..
            } => {
                assert_eq!(size, 4);
                assert_eq!(
                    workspace
                        .storage
                        .cas
                        .read_bytes(&content_hash)
                        .expect("main bytes"),
                    b"MAIN"
                );
            }
            other => panic!("expected a regular file, got {}", other.kind_name()),
        }

        // A stream-free file records no stream set at all: the fingerprint
        // serializes exactly as before the slice.
        fs::write(root.path().join("plain.txt"), b"P").expect("plain file");
        workspace
            .reconcile_locked("plain file")
            .expect("reconcile 2");
        let baseline = baseline_manifest(&workspace);
        let serialized =
            serde_json::to_string(&baseline.get("plain.txt")).expect("serialize plain");
        assert!(
            !serialized.contains("streams"),
            "a stream-free file must not drift: {serialized}"
        );
    }

    /// AC3: an operation that modifies, creates, and deletes streams is
    /// captured; undo restores the exact recorded stream set byte-for-byte;
    /// redo restores the new set; unrelated streams are untouched. Stream
    /// mutations go through PowerShell: cmd's redirection cannot target
    /// streams for deletion (`del` rejects the colon syntax) and rewriting
    /// the *main* file through cmd destroys the streams outright.
    #[test]
    fn stream_changes_are_captured_and_reversible() {
        let (root, _store, workspace) = fixture();
        write_stream(root.path(), "host.txt", "s1", b"V1\r\n");
        write_stream(root.path(), "host.txt", "s2", b"KEEP\r\n");
        write_stream(root.path(), "host.txt", "s3", b"STABLE\r\n");
        workspace
            .reconcile_locked("stream pre-state")
            .expect("reconcile");

        let outcome = workspace
            .run_command(&[
                "powershell".to_owned(),
                "-NoProfile".to_owned(),
                "-Command".to_owned(),
                "Set-Content -LiteralPath 'host.txt' -Stream 's1' -Value 'V2'; \
                 Set-Content -LiteralPath 'host.txt' -Stream 's4' -Value 'NEW'; \
                 Remove-Item -LiteralPath 'host.txt' -Stream 's2'"
                    .to_owned(),
            ])
            .expect("capture stream changes");
        let operation_id = outcome.operation_id.expect("operation id");
        let post = post_manifest(&workspace, operation_id);
        let streams = streams_of(&post.get("host.txt"));
        assert_eq!(streams.len(), 3, "s2 deleted, s4 created: {streams:?}");
        assert!(streams.contains_key("s4") && !streams.contains_key("s2"));
        // The captured bytes are whatever PowerShell wrote (encoding is
        // recorded, not assumed).
        let captured_s1 = read_stream(root.path(), "host.txt", "s1");
        let captured_s4 = read_stream(root.path(), "host.txt", "s4");

        undo(&workspace, Some(operation_id), false).expect("undo stream changes");
        assert_eq!(read_stream(root.path(), "host.txt", "s1"), b"V1\r\n");
        assert_eq!(read_stream(root.path(), "host.txt", "s2"), b"KEEP\r\n");
        assert_eq!(read_stream(root.path(), "host.txt", "s3"), b"STABLE\r\n");
        assert!(
            named_streams(&root.path().join("host.txt"))
                .expect("live streams")
                .iter()
                .all(|(name, _)| name != "s4"),
            "undo must remove the created stream"
        );
        assert_eq!(
            fs::read(root.path().join("host.txt")).expect("main content"),
            b"MAIN",
            "the default stream is untouched by stream undo"
        );

        redo(&workspace, Some(operation_id)).expect("redo stream changes");
        assert_eq!(read_stream(root.path(), "host.txt", "s1"), captured_s1);
        assert_eq!(read_stream(root.path(), "host.txt", "s3"), b"STABLE\r\n");
        assert_eq!(read_stream(root.path(), "host.txt", "s4"), captured_s4);
        assert!(
            named_streams(&root.path().join("host.txt"))
                .expect("live streams")
                .iter()
                .all(|(name, _)| name != "s2"),
            "redo must remove the deleted stream"
        );
    }

    /// AC4: an external stream modification after capture refuses undo
    /// before any mutation — stream content is conflict-checked exactly like
    /// main content.
    #[test]
    fn external_stream_divergence_refuses_undo() {
        let (root, _store, workspace) = fixture();
        write_stream(root.path(), "host.txt", "s1", b"V1\r\n");
        workspace
            .reconcile_locked("stream pre-state")
            .expect("reconcile");
        let outcome = workspace
            .run_command(&[
                "cmd".to_owned(),
                "/C".to_owned(),
                "echo CHANGED> host.txt:s1".to_owned(),
            ])
            .expect("capture stream change");
        let operation_id = outcome.operation_id.expect("operation id");

        // External writer changes the stream after the capture.
        write_stream(root.path(), "host.txt", "s1", b"EXTERNAL\r\n");
        let refusal = undo(&workspace, Some(operation_id), false);
        assert!(
            refusal.is_err(),
            "undo must refuse when a recorded stream diverged"
        );
        assert_eq!(
            read_stream(root.path(), "host.txt", "s1"),
            b"EXTERNAL\r\n",
            "nothing may be mutated by the refusal"
        );
        assert_eq!(
            fs::read(root.path().join("host.txt")).expect("main content"),
            b"MAIN",
            "the default stream is untouched by the refusal"
        );
    }

    /// AC5: quarantine carries streams (rename), the archive copy carries
    /// them (`fs::copy`/`CopyFileEx`), and archive verification compares
    /// them. The captured operation modifies a *stream*, so the quarantined
    /// post-state file still carries its stream set into the archive.
    /// (Rewriting the main file through a shell destroys streams outright —
    /// `CREATE_ALWAYS` semantics — which is precisely the silent loss this
    /// slice records and can undo.)
    #[test]
    fn stream_carrying_files_archive_faithfully() {
        let (root, store, workspace) = fixture();
        write_stream(root.path(), "host.txt", "s1", b"V1\r\n");
        write_stream(
            root.path(),
            "host.txt",
            "Zone.Identifier",
            b"[ZoneTransfer]\r\n",
        );
        workspace
            .reconcile_locked("stream pre-state")
            .expect("reconcile");

        let outcome = workspace
            .run_command(&[
                "cmd".to_owned(),
                "/C".to_owned(),
                "echo REPLACED> host.txt:s1".to_owned(),
            ])
            .expect("capture stream replacement");
        let operation_id = outcome.operation_id.expect("operation id");

        undo(&workspace, Some(operation_id), false).expect("undo stream replacement");
        assert_eq!(read_stream(root.path(), "host.txt", "s1"), b"V1\r\n");
        assert_eq!(
            fs::read(root.path().join("host.txt")).expect("main content"),
            b"MAIN"
        );

        // The committed transaction archived the quarantined post-state file
        // — which still carried both streams — and verify_archive_pair
        // compared stream sets and content, so the archive copy must carry
        // every stream.
        let archive_root = store.path().join("projects");
        let mut archived_stream_files = 0;
        fn walk(dir: &Path, sink: &mut usize) {
            for entry in fs::read_dir(dir).expect("read_dir") {
                let entry = entry.expect("entry");
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, sink);
                } else if named_streams(&path)
                    .map(|streams| !streams.is_empty())
                    .unwrap_or(false)
                {
                    *sink += 1;
                }
            }
        }
        walk(&archive_root, &mut archived_stream_files);
        assert!(
            archived_stream_files > 0,
            "the archive must contain the stream-carrying quarantined file"
        );
    }
}

/// The junction lifecycle tests run only on Windows: that is the only
/// platform whose scanner produces the variant, and its buffer layout and
/// creation mechanics were probe-verified against real `mklink /J`
/// junctions (`.ai/PHASE_5_WINDOWS_JUNCTIONS.md` §2). Every test drives the
/// real capture, undo, and redo machinery — journal, quarantine,
/// confinement, and post-apply verification included — not the scanner in
/// isolation.
#[cfg(windows)]
mod junctions {
    use std::fs;
    use std::path::Path;

    use rewind::model::Fingerprint;
    use rewind::rollback::{redo, undo};
    use rewind::scan::reparse_junction_names;
    use rewind::workspace::Workspace;

    fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Workspace) {
        let root = tempfile::tempdir().expect("temporary root");
        fs::write(root.path().join("foo.txt"), b"A").expect("write fixture");
        let store = tempfile::tempdir().expect("temporary store");
        let workspace = Workspace::init(root.path(), Some(store.path())).expect("initialize");
        (root, store, workspace)
    }

    /// Junctions enter through the platform's own mklink — the same way a
    /// real workspace acquires one.
    fn make_junction(root: &Path, name: &str, target: &str) {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J", name, target])
            .current_dir(root)
            .status()
            .expect("run mklink");
        assert!(status.success(), "mklink /J {name} -> {target} failed");
    }

    fn names_of(fingerprint: &Fingerprint) -> Option<(String, String)> {
        match fingerprint {
            Fingerprint::Junction {
                substitute,
                print_name,
                ..
            } => Some((substitute.clone(), print_name.clone())),
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

    fn baseline_manifest(workspace: &Workspace) -> rewind::model::Manifest {
        workspace
            .state_manifest(&workspace.baseline_id().expect("baseline"))
            .expect("baseline manifest")
    }

    /// AC1: the scanner classifies a junction as WINDOWS_JUNCTION with the
    /// recorded substitute and print names, never descends into it, and
    /// never touches its target.
    #[test]
    fn a_junction_scans_as_a_supported_junction() {
        let (root, _store, workspace) = fixture();
        fs::create_dir(root.path().join("sub")).expect("target dir");
        fs::write(root.path().join("sub").join("target.txt"), b"TARGET").expect("target file");
        make_junction(root.path(), "junc", "sub");

        workspace
            .reconcile_locked("junction classification")
            .expect("reconcile");
        let baseline = baseline_manifest(&workspace);
        let fingerprint = baseline.get("junc");
        assert_eq!(fingerprint.kind_name(), "WINDOWS_JUNCTION");
        assert!(fingerprint.is_supported_for_restore());
        let recorded = names_of(&fingerprint).expect("recorded junction names");
        let live = reparse_junction_names(&root.path().join("junc")).expect("live names");
        assert_eq!(recorded, live, "the record is the reparse data itself");
        assert!(
            baseline.entries.keys().all(|key| !key.starts_with("junc/")),
            "the scanner must never traverse a junction"
        );
        assert_eq!(
            fs::read(root.path().join("junc").join("target.txt"))
                .expect("read through the untouched junction"),
            b"TARGET",
            "the junction's target was never followed or modified"
        );
    }

    /// AC2: an operation that creates a junction is captured and reversible;
    /// undo removes it; redo recreates it with the recorded reparse data.
    #[test]
    fn junction_creation_is_captured_and_reversible() {
        let (root, _store, workspace) = fixture();
        fs::create_dir(root.path().join("sub")).expect("target dir");
        let outcome = workspace
            .run_command(&[
                "cmd".to_owned(),
                "/C".to_owned(),
                "mklink /J junc sub".to_owned(),
            ])
            .expect("capture junction creation");
        let operation_id = outcome.operation_id.expect("operation id");

        let post = post_manifest(&workspace, operation_id);
        assert_eq!(post.get("junc").kind_name(), "WINDOWS_JUNCTION");
        let recorded = names_of(&post.get("junc")).expect("recorded names");

        undo(&workspace, Some(operation_id), false).expect("undo junction creation");
        assert!(
            fs::symlink_metadata(root.path().join("junc")).is_err(),
            "undo must remove the created junction"
        );

        redo(&workspace, Some(operation_id)).expect("redo junction creation");
        assert_eq!(
            reparse_junction_names(&root.path().join("junc")).expect("redo reparse data"),
            recorded,
            "redo must recreate the junction with the recorded reparse data"
        );
    }

    /// AC3: replacing a junction with a real directory is reversible in both
    /// directions; the quarantine rename moves the junction, never its
    /// target subtree.
    #[test]
    fn replacing_a_junction_with_a_directory_is_reversible() {
        let (root, _store, workspace) = fixture();
        fs::create_dir(root.path().join("sub")).expect("target dir");
        fs::write(root.path().join("sub").join("target.txt"), b"TARGET").expect("target file");
        make_junction(root.path(), "junc", "sub");
        workspace
            .reconcile_locked("junction pre-state")
            .expect("reconcile");
        let recorded =
            names_of(&baseline_manifest(&workspace).get("junc")).expect("recorded names");

        let outcome = workspace
            .run_command(&[
                "cmd".to_owned(),
                "/C".to_owned(),
                "rd junc & md junc & echo DATA> junc\\file.txt".to_owned(),
            ])
            .expect("capture junction replacement");
        let operation_id = outcome.operation_id.expect("operation id");
        let post = post_manifest(&workspace, operation_id);
        assert_eq!(post.get("junc").kind_name(), "DIRECTORY");
        assert_eq!(post.get("junc/file.txt").kind_name(), "REGULAR_FILE");

        undo(&workspace, Some(operation_id), false).expect("undo restores the junction");
        assert_eq!(
            reparse_junction_names(&root.path().join("junc")).expect("restored reparse data"),
            recorded,
            "undo must restore the junction's recorded reparse data"
        );
        assert_eq!(
            fs::read(root.path().join("sub").join("target.txt")).expect("target content"),
            b"TARGET",
            "the quarantine rename must never touch the junction's target"
        );

        redo(&workspace, Some(operation_id)).expect("redo restores the directory");
        assert_eq!(
            fs::read_to_string(root.path().join("junc").join("file.txt"))
                .expect("replacement directory content"),
            "DATA\r\n",
            "redo must reinstall the replacement directory"
        );
    }

    /// AC4: junction targets outside the workspace root — including targets
    /// that do not exist — are recorded and restored literally; the external
    /// path is never followed, checked, or created.
    #[test]
    fn an_external_junction_target_is_restored_without_being_followed() {
        let (root, _store, workspace) = fixture();
        let outside = tempfile::tempdir().expect("external target outside the root");
        fs::write(outside.path().join("keep.txt"), b"EXTERNAL").expect("external content");
        make_junction(root.path(), "link-out", &outside.path().to_string_lossy());
        let missing = outside.path().join("missing");
        make_junction(root.path(), "link-dangling", &missing.to_string_lossy());
        workspace
            .reconcile_locked("external junction pre-state")
            .expect("reconcile");
        let baseline = baseline_manifest(&workspace);
        let recorded_out = names_of(&baseline.get("link-out")).expect("external names");
        let recorded_dangling = names_of(&baseline.get("link-dangling")).expect("dangling names");

        let outcome = workspace
            .run_command(&[
                "cmd".to_owned(),
                "/C".to_owned(),
                "rd link-out & md link-out & rd link-dangling & md link-dangling".to_owned(),
            ])
            .expect("capture junction replacement");
        let operation_id = outcome.operation_id.expect("operation id");

        undo(&workspace, Some(operation_id), false).expect("undo restores both junctions");
        assert_eq!(
            reparse_junction_names(&root.path().join("link-out")).expect("restored external"),
            recorded_out
        );
        assert_eq!(
            reparse_junction_names(&root.path().join("link-dangling")).expect("restored dangling"),
            recorded_dangling
        );
        assert!(
            !missing.exists(),
            "the nonexistent target must not have been created by restoration"
        );
        assert_eq!(
            fs::read(outside.path().join("keep.txt")).expect("external target content"),
            b"EXTERNAL",
            "the external target was never touched"
        );
        assert_eq!(
            fs::read(root.path().join("link-out").join("keep.txt"))
                .expect("read through the restored junction"),
            b"EXTERNAL",
            "the restored junction resolves to the untouched external target"
        );

        redo(&workspace, Some(operation_id)).expect("redo restores the directories");
        assert!(
            fs::symlink_metadata(root.path().join("link-out"))
                .expect("redo directory")
                .is_dir(),
            "redo must reinstall the replacement directories"
        );
    }

    /// AC5: external modification of the junction's reparse data after
    /// capture refuses undo before any mutation, exactly as for every other
    /// object class.
    #[test]
    fn external_divergence_of_a_junction_refuses_undo() {
        let (root, _store, workspace) = fixture();
        fs::create_dir(root.path().join("sub")).expect("target dir");
        fs::create_dir(root.path().join("other")).expect("other target");
        make_junction(root.path(), "junc", "sub");
        workspace
            .reconcile_locked("junction pre-state")
            .expect("reconcile");

        let outcome = workspace
            .run_command(&[
                "cmd".to_owned(),
                "/C".to_owned(),
                "rd junc & md junc".to_owned(),
            ])
            .expect("capture junction replacement");
        let operation_id = outcome.operation_id.expect("operation id");

        // External writer retargets the junction after the capture: the live
        // pre-state no longer matches the journal's expectation. (The
        // captured command left a plain directory at junc, so the external
        // writer removes it before recreating the link.)
        let status = std::process::Command::new("cmd")
            .args(["/C", "rd junc & mklink /J junc other"])
            .current_dir(root.path())
            .status()
            .expect("run external retarget");
        assert!(status.success(), "external retarget of junc failed");

        let refusal = undo(&workspace, Some(operation_id), false);
        assert!(
            refusal.is_err(),
            "undo must refuse when the live junction diverges from the journal"
        );
        assert!(
            reparse_junction_names(&root.path().join("junc")).is_some(),
            "the diverged junction is untouched by the refusal"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("foo.txt")).expect("content"),
            "A",
            "nothing else may be mutated by the refusal"
        );
    }
}

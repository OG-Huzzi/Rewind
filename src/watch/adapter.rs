//! Phase 3: the adapter boundary between the platform filesystem
//! notification mechanisms and the watcher loop.
//!
//! The production adapter uses the `notify` crate, which drives the
//! platform-native mechanism directly — inotify on Linux, FSEvents on macOS,
//! ReadDirectoryChangesW on Windows. What this layer keeps explicit rather
//! than delegating silently (contract §5):
//!
//! * **Overflow is a first-class outcome.** Every backend surfaces
//!   queue-loss as an event with `notify::event::Flag::Rescan` (inotify
//!   `IN_Q_OVERFLOW`, FSEvents `MustScanSubDirs`/`UserDropped`/`KernelDropped`,
//!   Windows rescan). All of it maps to [`AdapterOutput::Overflow`].
//! * **No semantic promise is invented**: the adapter maps a platform
//!   notification to a raw kind and nothing more. It does not pair renames,
//!   reconstruct deletes, or decide what "really" happened — the
//!   authoritative scan does that during reconciliation.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::event::{ModifyKind, RenameMode};
use notify::{EventKind, RecursiveMode, Watcher};

use crate::error::{Result, RewindError};

/// A platform notification reduced to the facts the watcher model can carry.
/// Paths are still absolute and unconfined at this stage; the loop confines
/// and normalizes them.
#[derive(Clone, Debug)]
pub struct RawEvent {
    pub paths: Vec<PathBuf>,
    pub kind: RawKind,
}

/// The adapter's best mapping of a platform notification. `RenameFrom`/
/// `RenameTo`/`RenameBoth` describe what the platform *said*, not what the
/// filesystem *did*.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RawKind {
    Create,
    Modify,
    Delete,
    RenameFrom,
    RenameTo,
    RenameBoth,
    Other,
}

/// One adapter poll outcome. `Idle` means "nothing arrived within the
/// timeout" and is the loop's opportunity for heartbeat and stop checks.
#[derive(Debug)]
pub enum AdapterOutput {
    Event(RawEvent),
    Idle,
    /// Platform queue overflow: events may have been lost; the covered
    /// interval becomes unknown (degradation `OVERFLOW`).
    Overflow,
    /// The adapter failed and can make no coverage claim.
    Failed(String),
}

/// The boundary the watcher loop is written against. The deterministic
/// [`FakeAdapter`] implements it for tests so the state machine is exercised
/// without platform scheduling (contract §15).
pub trait EventAdapter {
    /// Blocks up to `timeout` waiting for the next adapter outcome.
    fn poll(&mut self, timeout: Duration) -> AdapterOutput;

    /// Recreates the platform observation after overflow or failure so
    /// coverage can resume for everything after this point. The degradation
    /// record for *why* is written by the loop before this is called; the
    /// restart itself never restores trust for what came before.
    fn restart(&mut self) -> Result<()>;

    /// Releases platform resources. Called once, on loop exit.
    fn shutdown(&mut self);
}

/// Maps a `notify` event to the raw model. A single notify event may carry
/// several paths (FSEvents batches); each becomes its own raw event, except
/// a `RenameMode::Both` pair which stays together as `RenameBoth`.
pub fn map_notify_event(event: &notify::Event) -> Vec<RawEvent> {
    let kind = match &event.kind {
        EventKind::Create(_) => RawKind::Create,
        EventKind::Remove(_) => RawKind::Delete,
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => RawKind::RenameFrom,
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => RawKind::RenameTo,
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => RawKind::RenameBoth,
        EventKind::Modify(_) => RawKind::Modify,
        EventKind::Access(_) | EventKind::Other | EventKind::Any => RawKind::Other,
    };
    match (kind, event.paths.len()) {
        (RawKind::RenameBoth, 2) => vec![RawEvent {
            paths: event.paths.clone(),
            kind,
        }],
        (_, 0) => Vec::new(),
        (_, _) => event
            .paths
            .iter()
            .map(|path| RawEvent {
                paths: vec![path.clone()],
                kind,
            })
            .collect(),
    }
}

/// The production adapter over `notify::RecommendedWatcher`.
pub struct NotifyAdapter {
    watcher: Option<notify::RecommendedWatcher>,
    receiver: mpsc::Receiver<notify::Result<notify::Event>>,
    /// Raw events already mapped from received notifications but not yet
    /// returned by [`NotifyAdapter::poll`]. A single notification can carry
    /// several paths (FSEvents batches; the mapper fans them out), and the
    /// adapter's API returns one event per poll, so the remainder must be
    /// retained here — dropping them would silently discard changes the
    /// platform already delivered. The queue drains on every poll, so it
    /// holds at most one notification's worth of mapped events between
    /// polls; it is never a new unbounded buffer.
    pending: VecDeque<RawEvent>,
    root: PathBuf,
}

fn adapter_error(detail: impl std::fmt::Display) -> RewindError {
    RewindError::Storage(format!("watcher adapter: {detail}"))
}

impl NotifyAdapter {
    pub fn new(root: &Path) -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(sender).map_err(adapter_error)?;
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(adapter_error)?;
        Ok(Self {
            watcher: Some(watcher),
            receiver,
            pending: VecDeque::new(),
            root: root.to_path_buf(),
        })
    }

    /// Recreates the platform watcher after overflow or failure, so
    /// observation can resume for everything after this point. The
    /// degradation record for *why* the restart was needed has already been
    /// written by the loop before this is called. Raw events received from
    /// the old watcher stay queued and are delivered on subsequent polls:
    /// they were real notifications, and discarding them would recreate
    /// exactly the silent-loss behavior this queue exists to prevent. The
    /// degradation record, not the queue, is what keeps the covered
    /// interval unknown.
    pub fn restart_watcher(&mut self) -> Result<()> {
        self.watcher = None;
        let (sender, receiver) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(sender).map_err(adapter_error)?;
        watcher
            .watch(&self.root, RecursiveMode::Recursive)
            .map_err(adapter_error)?;
        self.watcher = Some(watcher);
        self.receiver = receiver;
        Ok(())
    }
}

impl EventAdapter for NotifyAdapter {
    fn poll(&mut self, timeout: Duration) -> AdapterOutput {
        // Queued events are delivered before polling for more
        // notifications, preserving the underlying notification order.
        if let Some(raw) = self.pending.pop_front() {
            return AdapterOutput::Event(raw);
        }
        match self.receiver.recv_timeout(timeout) {
            Ok(Ok(event)) => {
                if event.need_rescan() {
                    return AdapterOutput::Overflow;
                }
                let events = map_notify_event(&event);
                if events.is_empty() {
                    AdapterOutput::Idle
                } else {
                    // Every mapped event must reach the pipeline: return the
                    // first and retain the rest for the following polls.
                    let mut mapped: VecDeque<_> = events.into();
                    let first = mapped.pop_front().expect("non-empty");
                    self.pending.extend(mapped);
                    AdapterOutput::Event(first)
                }
            }
            Ok(Err(error)) => AdapterOutput::Failed(error.to_string()),
            Err(mpsc::RecvTimeoutError::Timeout) => AdapterOutput::Idle,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                AdapterOutput::Failed("watcher channel disconnected".to_owned())
            }
        }
    }

    fn restart(&mut self) -> Result<()> {
        self.restart_watcher()
    }

    fn shutdown(&mut self) {
        self.watcher = None;
    }
}

/// Deterministic adapter for tests: replays a scripted sequence of outputs.
/// After the script is exhausted, every poll is `Idle`, so a test can drive
/// the loop in a thread, assert on durable effects, and then request a stop
/// through the real stop-flag path. Restarts succeed unless scripted
/// otherwise via [`AdapterOutput::Failed`] consumed by the loop's
/// post-restart poll.
pub struct FakeAdapter {
    script: VecDeque<AdapterOutput>,
    restart_fails: bool,
}

impl FakeAdapter {
    pub fn new(script: impl IntoIterator<Item = AdapterOutput>) -> Self {
        Self {
            script: script.into_iter().collect(),
            restart_fails: false,
        }
    }

    /// Makes every restart fail, so tests can drive the loop's
    /// unrecoverable-failure path deterministically.
    pub fn with_failing_restarts(mut self) -> Self {
        self.restart_fails = true;
        self
    }
}

impl EventAdapter for FakeAdapter {
    fn poll(&mut self, _timeout: Duration) -> AdapterOutput {
        self.script.pop_front().unwrap_or(AdapterOutput::Idle)
    }

    fn restart(&mut self) -> Result<()> {
        if self.restart_fails {
            Err(RewindError::Storage("scripted restart failure".to_owned()))
        } else {
            Ok(())
        }
    }

    fn shutdown(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, ModifyKind as Mk};

    fn event(kind: EventKind, paths: &[&str]) -> notify::Event {
        let mut event = notify::Event::new(kind);
        event.paths = paths.iter().map(PathBuf::from).collect();
        event
    }

    #[test]
    fn maps_kinds_to_raw_model() {
        let mapped = map_notify_event(&event(EventKind::Create(CreateKind::File), &["/w/a.txt"]));
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].kind, RawKind::Create);

        let mapped = map_notify_event(&event(
            EventKind::Modify(Mk::Data(DataChange::Any)),
            &["/w/a.txt"],
        ));
        assert_eq!(mapped[0].kind, RawKind::Modify);

        let mapped = map_notify_event(&event(
            EventKind::Modify(Mk::Name(RenameMode::From)),
            &["/w/old.txt"],
        ));
        assert_eq!(mapped[0].kind, RawKind::RenameFrom);
    }

    #[test]
    fn rename_both_stays_one_event_with_two_paths() {
        let mapped = map_notify_event(&event(
            EventKind::Modify(Mk::Name(RenameMode::Both)),
            &["/w/from.txt", "/w/to.txt"],
        ));
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].kind, RawKind::RenameBoth);
        assert_eq!(mapped[0].paths.len(), 2);
    }

    #[test]
    fn batched_paths_fan_out_to_one_raw_event_each() {
        let mapped = map_notify_event(&event(
            EventKind::Create(CreateKind::Any),
            &["/w/a.txt", "/w/b.txt", "/w/c.txt"],
        ));
        assert_eq!(mapped.len(), 3);
        assert!(mapped.iter().all(|raw| raw.kind == RawKind::Create));
    }

    #[test]
    fn access_events_map_to_other() {
        use notify::event::AccessKind;
        let mapped = map_notify_event(&event(EventKind::Access(AccessKind::Any), &["/w/a.txt"]));
        assert_eq!(mapped[0].kind, RawKind::Other);
    }

    #[test]
    fn fake_adapter_replays_script_then_goes_idle() {
        let mut adapter = FakeAdapter::new([
            AdapterOutput::Overflow,
            AdapterOutput::Failed("x".to_owned()),
        ]);
        assert!(matches!(
            adapter.poll(Duration::ZERO),
            AdapterOutput::Overflow
        ));
        assert!(matches!(
            adapter.poll(Duration::ZERO),
            AdapterOutput::Failed(_)
        ));
        assert!(matches!(adapter.poll(Duration::ZERO), AdapterOutput::Idle));
        adapter.shutdown();
    }

    /// Builds the production adapter with a controllable notification
    /// channel (no platform watcher), so `poll` — the real production code
    /// path — can be driven deterministically.
    fn production_adapter() -> (NotifyAdapter, mpsc::Sender<notify::Result<notify::Event>>) {
        let (sender, receiver) = mpsc::channel();
        let adapter = NotifyAdapter {
            watcher: None,
            receiver,
            pending: VecDeque::new(),
            root: PathBuf::from("/w"),
        };
        (adapter, sender)
    }

    /// Regression (audit bug #1): a notification carrying several paths used
    /// to lose every mapped event after the first. Every mapped event must
    /// be emitted — exactly once, in notification order — and the queue must
    /// drain to Idle afterwards.
    #[test]
    fn poll_delivers_every_path_of_a_batched_notification() {
        let (mut adapter, sender) = production_adapter();
        sender
            .send(Ok(event(
                EventKind::Create(CreateKind::Any),
                &["/w/a.txt", "/w/b.txt", "/w/c.txt"],
            )))
            .expect("send");

        let expected = ["/w/a.txt", "/w/b.txt", "/w/c.txt"];
        for expected_path in expected {
            match adapter.poll(Duration::ZERO) {
                AdapterOutput::Event(raw) => {
                    assert_eq!(raw.kind, RawKind::Create);
                    assert_eq!(raw.paths, vec![PathBuf::from(expected_path)]);
                }
                other => panic!("expected an event for {expected_path}, got {other:?}"),
            }
        }
        // Queue drained, channel empty: Idle, nothing duplicated.
        assert!(matches!(adapter.poll(Duration::ZERO), AdapterOutput::Idle));
    }

    /// Queued events are delivered before polling for more notifications,
    /// preserving the underlying notification order.
    #[test]
    fn queued_events_are_delivered_before_new_notifications() {
        let (mut adapter, sender) = production_adapter();
        sender
            .send(Ok(event(
                EventKind::Create(CreateKind::Any),
                &["/w/a.txt", "/w/b.txt"],
            )))
            .expect("send first");
        sender
            .send(Ok(event(
                EventKind::Modify(Mk::Data(DataChange::Any)),
                &["/w/c.txt"],
            )))
            .expect("send second");

        let order: Vec<(RawKind, String)> = (0..3)
            .filter_map(|_| match adapter.poll(Duration::ZERO) {
                AdapterOutput::Event(raw) => {
                    Some((raw.kind, raw.paths[0].to_string_lossy().into()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            order,
            vec![
                (RawKind::Create, "/w/a.txt".to_owned()),
                (RawKind::Create, "/w/b.txt".to_owned()),
                (RawKind::Modify, "/w/c.txt".to_owned()),
            ],
            "queued leftovers precede newly received notifications"
        );
    }

    /// Overflow semantics are unchanged by the queue: a rescan notification
    /// still reports Overflow, and events received *before* the rescan stay
    /// queued and are delivered — they are real received evidence, and
    /// dropping them would recreate the silent-loss behavior the queue
    /// exists to prevent. The degradation record, not the queue, keeps the
    /// covered interval unknown.
    #[test]
    fn rescan_still_reports_overflow_with_queued_events_pending() {
        let (mut adapter, sender) = production_adapter();
        sender
            .send(Ok(event(
                EventKind::Create(CreateKind::Any),
                &["/w/a.txt", "/w/b.txt"],
            )))
            .expect("send event");
        sender
            .send(Ok(
                notify::Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan)
            ))
            .expect("send rescan");

        assert!(matches!(
            adapter.poll(Duration::ZERO),
            AdapterOutput::Event(_)
        ));
        assert!(matches!(
            adapter.poll(Duration::ZERO),
            AdapterOutput::Event(_)
        ));
        assert!(matches!(
            adapter.poll(Duration::ZERO),
            AdapterOutput::Overflow
        ));
        assert!(matches!(adapter.poll(Duration::ZERO), AdapterOutput::Idle));
    }

    /// Full production ingestion path: the real `NotifyAdapter::poll`
    /// (channel-fed), the real serve loop, confinement, coalescing, and
    /// both downstream artifacts — every path of a batched notification
    /// must reach the dirty index and the event log exactly once.
    #[test]
    fn serve_loop_ingests_every_path_of_a_batched_notification() {
        use crate::watch::run::{serve_loop, LoopExit, WatchConfig, WatchPaths};
        use uuid::Uuid;

        let root = tempfile::tempdir().expect("tempdir").keep();
        let paths = WatchPaths::new(&root);
        let (mut adapter, sender) = production_adapter();
        // Re-root the adapter's confinement root at the real temp directory
        // and send one batched notification carrying three paths.
        let absolute: Vec<String> = ["a.txt", "b.txt", "c.txt"]
            .iter()
            .map(|name| root.join(name).to_string_lossy().into_owned())
            .collect();
        let mut batched = notify::Event::new(EventKind::Create(CreateKind::Any));
        batched.paths = absolute.iter().map(PathBuf::from).collect();
        sender.send(Ok(batched)).expect("send batched notification");

        let loop_root = root.clone();
        let loop_paths = paths.clone();
        let workspace_id = Uuid::new_v4();
        let watch_config = WatchConfig {
            batch: Duration::from_millis(50),
            ..WatchConfig::default()
        };
        let handle = std::thread::spawn(move || {
            serve_loop(
                &loop_paths,
                &loop_root,
                workspace_id,
                &mut adapter,
                &watch_config,
            )
            .expect("serve loop")
        });

        let dirty_path = paths.dirty.clone();
        let events_path = paths.events.clone();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Ok(index) = std::fs::read_to_string(&dirty_path) {
                if let Ok(index) = serde_json::from_str::<crate::watch::model::DirtyIndex>(&index) {
                    let expected: Vec<String> = ["a.txt", "b.txt", "c.txt"]
                        .iter()
                        .map(|name| name.to_string())
                        .collect();
                    if index.paths.into_iter().collect::<Vec<_>>() == expected {
                        break;
                    }
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "all three paths must reach the dirty index"
            );
            std::thread::sleep(Duration::from_millis(25));
        }

        crate::paths::atomic_write(&paths.stop_flag, b"stop\n").expect("stop flag");
        assert_eq!(handle.join().expect("join"), LoopExit::Stopped);

        // The event log received every mapped event exactly once.
        let log = std::fs::read_to_string(&events_path).expect("event log");
        for name in ["a.txt", "b.txt", "c.txt"] {
            let count = log
                .lines()
                .filter(|line| line.contains(name) && line.contains("\"kind\":\"CREATE\""))
                .count();
            assert_eq!(count, 1, "{name} must appear exactly once in the log");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}

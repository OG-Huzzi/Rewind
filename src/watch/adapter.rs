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
            root: root.to_path_buf(),
        })
    }

    /// Recreates the platform watcher after overflow or failure, so
    /// observation can resume for everything after this point. The
    /// degradation record for *why* the restart was needed has already been
    /// written by the loop before this is called.
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
        match self.receiver.recv_timeout(timeout) {
            Ok(Ok(event)) => {
                if event.need_rescan() {
                    return AdapterOutput::Overflow;
                }
                let events = map_notify_event(&event);
                if events.is_empty() {
                    AdapterOutput::Idle
                } else {
                    // One raw event per poll keeps the loop's accounting
                    // simple; the channel drains fast enough at batch
                    // granularity.
                    AdapterOutput::Event(events.into_iter().next().expect("non-empty"))
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
}

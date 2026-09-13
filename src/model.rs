use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{Result, RewindError};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetadataFingerprint {
    pub mode: Option<u32>,
    pub readonly: bool,
}

/// State-identity schema version. The architecture defines a state's identity
/// as a digest of the canonical serialized manifest *and* the manifest
/// schema version, so any change to fingerprint semantics must produce a
/// different state id. Existing rows keep their stored ids and remain
/// readable; the first scan after an upgrade computes v2 ids, which the
/// drift rules resolve through an explicit reconciliation checkpoint rather
/// than silent invalidation.
pub const STATE_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SymlinkTargetKind {
    File,
    Directory,
    #[default]
    Unknown,
}

impl SymlinkTargetKind {
    /// True when the recorded kind is sufficient to choose the correct
    /// Windows creation API (`symlink_file` vs `symlink_dir`). A kind
    /// inferred from an external target is never trusted for restoration;
    /// unknown kinds refuse restoration instead of guessing.
    pub fn is_creatable(&self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value")]
pub enum Fingerprint {
    Absent,
    RegularFile {
        content_hash: String,
        size: u64,
        metadata: MetadataFingerprint,
    },
    Directory {
        manifest_hash: String,
        entry_count: u64,
        metadata: MetadataFingerprint,
    },
    Symlink {
        target: String,
        /// Phase 1.2 (schema v2) addition. Older persisted manifests predate
        /// target kinds; they deserialize with `Unknown`, which refuses
        /// Windows restoration rather than guessing — existing states stay
        /// readable instead of being silently invalidated.
        #[serde(default)]
        target_kind: SymlinkTargetKind,
        target_hash: String,
        metadata: MetadataFingerprint,
    },
    Unsupported {
        object_kind: String,
        descriptor: String,
    },
}

impl Fingerprint {
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Absent => "ABSENT",
            Self::RegularFile { .. } => "REGULAR_FILE",
            Self::Directory { .. } => "DIRECTORY",
            Self::Symlink { .. } => "SYMLINK",
            Self::Unsupported { .. } => "UNSUPPORTED",
        }
    }

    pub fn is_supported_for_restore(&self) -> bool {
        matches!(
            self,
            Self::Absent | Self::RegularFile { .. } | Self::Directory { .. } | Self::Symlink { .. }
        )
    }

    pub fn content_hash(&self) -> Option<&str> {
        match self {
            Self::RegularFile { content_hash, .. } => Some(content_hash),
            _ => None,
        }
    }

    /// Builds a directory fingerprint over an explicit child map. This is
    /// the same canonical form the scanner produces, so an expected
    /// intermediate directory state constructed here compares equal to the
    /// live directory the scanner will observe.
    pub fn directory_from_children(
        children: &BTreeMap<String, Fingerprint>,
        metadata: MetadataFingerprint,
    ) -> Result<Self> {
        let serialized = serde_json::to_vec(children)?;
        Ok(Self::Directory {
            manifest_hash: blake3::hash(&serialized).to_hex().to_string(),
            entry_count: children.len() as u64,
            metadata,
        })
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Absent => "ABSENT".to_owned(),
            Self::RegularFile {
                content_hash, size, ..
            } => format!("REGULAR_FILE(hash={content_hash},size={size})"),
            Self::Directory {
                manifest_hash,
                entry_count,
                ..
            } => format!("DIRECTORY(hash={manifest_hash},entries={entry_count})"),
            Self::Symlink {
                target,
                target_kind,
                target_hash,
                ..
            } => {
                let kind = match target_kind {
                    SymlinkTargetKind::File => "file",
                    SymlinkTargetKind::Directory => "directory",
                    SymlinkTargetKind::Unknown => "unknown",
                };
                format!("SYMLINK(target_hash={target_hash},target={target},kind={kind})")
            }
            Self::Unsupported {
                object_kind,
                descriptor,
            } => format!("UNSUPPORTED({object_kind}:{descriptor})"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Manifest {
    pub entries: BTreeMap<String, Fingerprint>,
}

/// Canonical manifest envelope: the schema version participates in the
/// serialized bytes, so `state_id()` cannot silently equate manifests
/// produced under different fingerprint semantics.
#[derive(Serialize)]
struct ManifestEnvelope<'a> {
    schema_version: u32,
    entries: &'a BTreeMap<String, Fingerprint>,
}

impl Manifest {
    pub fn empty() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        let envelope = ManifestEnvelope {
            schema_version: STATE_SCHEMA_VERSION,
            entries: &self.entries,
        };
        serde_json::to_vec(&envelope).map_err(RewindError::from)
    }

    pub fn state_id(&self) -> Result<String> {
        let digest = blake3::hash(&self.canonical_bytes()?);
        Ok(digest.to_hex().to_string())
    }

    pub fn get(&self, path: &str) -> Fingerprint {
        self.entries
            .get(path)
            .cloned()
            .unwrap_or(Fingerprint::Absent)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StateRecord {
    pub id: String,
    pub kind: StateKind,
    pub manifest: Manifest,
    pub parent_id: Option<String>,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum StateKind {
    Baseline,
    Observation,
    Reconciliation,
    Snapshot,
    Anchor,
}

impl StateKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Baseline => "BASELINE",
            Self::Observation => "OBSERVATION",
            Self::Reconciliation => "RECONCILIATION",
            Self::Snapshot => "SNAPSHOT",
            Self::Anchor => "ANCHOR",
        }
    }
}

impl std::str::FromStr for StateKind {
    type Err = RewindError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "BASELINE" => Ok(Self::Baseline),
            "OBSERVATION" => Ok(Self::Observation),
            "RECONCILIATION" => Ok(Self::Reconciliation),
            "SNAPSHOT" => Ok(Self::Snapshot),
            "ANCHOR" => Ok(Self::Anchor),
            other => Err(RewindError::Serialization(format!(
                "unknown state kind {other}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum WorkspaceCondition {
    Healthy,
    Degraded,
    ReconciliationRequired,
    RecoveryRequired,
}

impl WorkspaceCondition {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Healthy => "HEALTHY",
            Self::Degraded => "DEGRADED",
            Self::ReconciliationRequired => "RECONCILIATION_REQUIRED",
            Self::RecoveryRequired => "RECOVERY_REQUIRED",
        }
    }
}

impl fmt::Display for WorkspaceCondition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for WorkspaceCondition {
    type Err = RewindError;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "HEALTHY" => Ok(Self::Healthy),
            "DEGRADED" => Ok(Self::Degraded),
            "RECONCILIATION_REQUIRED" => Ok(Self::ReconciliationRequired),
            "RECOVERY_REQUIRED" => Ok(Self::RecoveryRequired),
            other => Err(RewindError::Serialization(format!(
                "unknown workspace condition {other}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum OperationKind {
    Strong,
    PassiveObservation,
    Reconciliation,
    Restore,
    CaptureFailed,
    BoundaryOnly,
}

impl OperationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Strong => "STRONG",
            Self::PassiveObservation => "PASSIVE_OBSERVATION",
            Self::Reconciliation => "RECONCILIATION",
            Self::Restore => "RESTORE",
            Self::CaptureFailed => "CAPTURE_FAILED",
            Self::BoundaryOnly => "BOUNDARY_ONLY",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum OperationStatus {
    Completed,
    Undone,
    Failed,
    Untrusted,
}

impl OperationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "COMPLETED",
            Self::Undone => "UNDONE",
            Self::Failed => "FAILED",
            Self::Untrusted => "UNTRUSTED",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TrackingConfidence {
    Tracked,
    LowConfidenceObservation,
    Degraded,
    Unsupported,
}

impl TrackingConfidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tracked => "TRACKED",
            Self::LowConfidenceObservation => "LOW_CONFIDENCE_OBSERVATION",
            Self::Degraded => "DEGRADED",
            Self::Unsupported => "UNSUPPORTED",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Reversibility {
    FullyReversible,
    PartiallyReversible,
    Unavailable,
    Unsupported,
}

impl Reversibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FullyReversible => "FULLY_REVERSIBLE",
            Self::PartiallyReversible => "PARTIALLY_REVERSIBLE",
            Self::Unavailable => "UNAVAILABLE",
            Self::Unsupported => "UNSUPPORTED",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum EffectType {
    Create,
    Modify,
    Delete,
    Rename,
    TypeChange,
}

impl EffectType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Create => "CREATE",
            Self::Modify => "MODIFY",
            Self::Delete => "DELETE",
            Self::Rename => "RENAME",
            Self::TypeChange => "TYPE_CHANGE",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Effect {
    pub path: String,
    pub from_path: Option<String>,
    pub effect_type: EffectType,
    pub pre: Fingerprint,
    pub post: Fingerprint,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OperationRecord {
    pub id: i64,
    pub workspace_id: Uuid,
    pub kind: OperationKind,
    pub status: OperationStatus,
    pub pre_state_id: Option<String>,
    pub post_state_id: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub exit_code: Option<i32>,
    pub confidence: TrackingConfidence,
    pub reversibility: Reversibility,
    pub error: Option<String>,
    pub created_at: i64,
    pub effects: Vec<Effect>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum JournalStatus {
    Planned,
    Prepared,
    Applying,
    Applied,
    Durable,
    Committed,
    RecoveryRequired,
    Abandoned,
}

impl JournalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planned => "PLANNED",
            Self::Prepared => "PREPARED",
            Self::Applying => "APPLYING",
            Self::Applied => "APPLIED",
            Self::Durable => "DURABLE",
            Self::Committed => "COMMITTED",
            Self::RecoveryRequired => "RECOVERY_REQUIRED",
            Self::Abandoned => "ABANDONED",
        }
    }

    /// ABANDONED is a terminal state: the interrupted transaction was
    /// explicitly given up by the user through `rewind recover --reconcile`
    /// and its effects were absorbed by a reconciliation checkpoint. It is
    /// never produced automatically.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Committed | Self::Abandoned)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ArchiveStatus {
    #[default]
    NotRequired,
    Pending,
    Archived,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JournalStep {
    pub id: usize,
    pub paths: Vec<String>,
    pub before: BTreeMap<String, Fingerprint>,
    pub after: BTreeMap<String, Fingerprint>,
    pub backup_path: Option<String>,
    pub staging_path: Option<String>,
    pub status: JournalStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Journal {
    pub transaction_id: Uuid,
    pub workspace_id: Uuid,
    pub operation_id: Option<i64>,
    pub anchor_state_id: String,
    pub target_state_id: String,
    pub direction: String,
    pub status: JournalStatus,
    pub steps: Vec<JournalStep>,
    #[serde(default)]
    pub archive_status: ArchiveStatus,
    #[serde(default)]
    pub archive_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkspacePointer {
    pub workspace_id: Uuid,
    pub root: String,
    pub store_root: String,
    pub schema_version: u32,
}

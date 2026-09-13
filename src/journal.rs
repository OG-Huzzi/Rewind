use std::fs;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::error::{Result, RewindError};
use crate::model::{ArchiveStatus, Journal, JournalStatus};
use crate::paths::{atomic_write, ensure_directory};

#[derive(Clone, Debug)]
pub struct JournalStore {
    root: PathBuf,
}

impl JournalStore {
    pub fn new(root: PathBuf) -> Result<Self> {
        ensure_directory(&root)?;
        Ok(Self { root })
    }

    pub fn path_for(&self, transaction_id: Uuid) -> PathBuf {
        self.root.join(format!("{transaction_id}.json"))
    }

    pub fn write(&self, journal: &Journal) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(journal)?;
        atomic_write(&self.path_for(journal.transaction_id), &bytes)
    }

    pub fn load(&self, path: &Path) -> Result<Journal> {
        let bytes = fs::read(path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn unfinished(&self) -> Result<Vec<(PathBuf, Journal)>> {
        let mut result = Vec::new();
        for item in fs::read_dir(&self.root)? {
            let item = item?;
            let path = item.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let journal = self.load(&path)?;
            if !journal.status.is_terminal() {
                result.push((path, journal));
            }
        }
        Ok(result)
    }

    pub fn pending_archives(&self) -> Result<Vec<(PathBuf, Journal)>> {
        let mut result = Vec::new();
        for item in fs::read_dir(&self.root)? {
            let item = item?;
            let path = item.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let journal = self.load(&path)?;
            if matches!(
                journal.archive_status,
                ArchiveStatus::Pending | ArchiveStatus::Failed
            ) && matches!(journal.status, JournalStatus::Committed)
            {
                result.push((path, journal));
            }
        }
        Ok(result)
    }

    pub fn mark_committed(&self, transaction_id: Uuid) -> Result<()> {
        let path = self.path_for(transaction_id);
        let mut journal = self.load(&path)?;
        journal.status = JournalStatus::Committed;
        for step in &mut journal.steps {
            step.status = JournalStatus::Durable;
        }
        self.write(&journal)
    }

    pub fn validate_path(&self, path: &Path) -> Result<()> {
        if path.parent() != Some(self.root.as_path()) {
            return Err(RewindError::Journal(format!(
                "journal path is outside journal store: {}",
                path.display()
            )));
        }
        Ok(())
    }
}

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::error::{Result, RewindError};
use crate::paths::{atomic_write, ensure_directory};

#[derive(Clone, Debug)]
pub struct Cas {
    root: PathBuf,
}

impl Cas {
    pub fn new(root: PathBuf) -> Result<Self> {
        let blobs = root.join("blobs");
        let temp = root.join("tmp");
        ensure_directory(&root)?;
        ensure_directory(&blobs)?;
        ensure_directory(&temp)?;
        for item in fs::read_dir(&temp)? {
            let item = item?;
            if item.path().extension().and_then(|value| value.to_str()) == Some("part") {
                fs::remove_file(item.path())?;
            }
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn blob_path(&self, hash: &str) -> Result<PathBuf> {
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(RewindError::Cas(format!("invalid BLAKE3 object id {hash}")));
        }
        Ok(self.root.join("blobs").join(hash))
    }

    pub fn put_file(&self, source: &Path) -> Result<String> {
        let mut input = File::open(source)?;
        let temporary = self
            .root
            .join("tmp")
            .join(format!("{}.part", Uuid::new_v4()));
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0_u8; 128 * 1024];
        let write_result = (|| -> Result<()> {
            loop {
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
                output.write_all(&buffer[..count])?;
            }
            output.sync_all()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        let hash = hasher.finalize().to_hex().to_string();
        let destination = self.blob_path(&hash)?;
        if destination.exists() {
            let _ = fs::remove_file(&temporary);
            self.verify(&hash)?;
        } else {
            set_readonly(&temporary)?;
            fs::rename(&temporary, &destination)?;
            if let Some(parent) = destination.parent() {
                sync_directory(parent)?;
            }
        }
        Ok(hash)
    }

    pub fn put_bytes(&self, bytes: &[u8]) -> Result<String> {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let destination = self.blob_path(&hash)?;
        if !destination.exists() {
            let temporary = self
                .root
                .join("tmp")
                .join(format!("{}.part", Uuid::new_v4()));
            fs::write(&temporary, bytes)?;
            let file = File::open(&temporary)?;
            file.sync_all()?;
            set_readonly(&temporary)?;
            fs::rename(&temporary, &destination)?;
            if let Some(parent) = destination.parent() {
                sync_directory(parent)?;
            }
        }
        self.verify(&hash)?;
        Ok(hash)
    }

    pub fn verify(&self, hash: &str) -> Result<()> {
        let path = self.blob_path(hash)?;
        let mut file = File::open(&path)
            .map_err(|error| RewindError::Cas(format!("missing CAS object {hash}: {error}")))?;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0_u8; 128 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        let actual = hasher.finalize().to_hex().to_string();
        if actual != hash {
            return Err(RewindError::Cas(format!(
                "CAS corruption for {hash}; computed {actual}"
            )));
        }
        Ok(())
    }

    pub fn read_bytes(&self, hash: &str) -> Result<Vec<u8>> {
        self.verify(hash)?;
        Ok(fs::read(self.blob_path(hash)?)?)
    }

    pub fn materialize(&self, hash: &str, destination: &Path) -> Result<()> {
        self.verify(hash)?;
        let parent = destination
            .parent()
            .ok_or_else(|| RewindError::Cas("destination has no parent".to_owned()))?;
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(
            ".{}.{}.stage",
            destination
                .file_name()
                .map(|name| name.to_string_lossy())
                .unwrap_or_default(),
            Uuid::new_v4()
        ));
        let mut input = File::open(self.blob_path(hash)?)?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        let copy_result = (|| -> Result<()> {
            std::io::copy(&mut input, &mut output)?;
            output.sync_all()?;
            fs::rename(&temporary, destination)?;
            Ok(())
        })();
        if let Err(error) = copy_result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        Ok(())
    }

    pub fn referenced_objects(&self, hashes: impl IntoIterator<Item = String>) -> Result<()> {
        for hash in hashes {
            self.verify(&hash)?;
        }
        Ok(())
    }

    pub fn publish_bytes(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let path = self.root.join(name);
        atomic_write(&path, bytes)
    }
}

fn set_readonly(path: &Path) -> Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

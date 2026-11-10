//! Separate raw binary asset messages. No attachment bytes enter the action log.
use crate::action_log;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const MAGIC: &[u8; 8] = b"BOMBLB01";
pub const MAX_BLOB_SIZE: usize = 64 * 1024 * 1024;
const MAX_HEADER: usize = 4096;
#[derive(Serialize, Deserialize)]
struct Header {
    asset: String,
    size: usize,
    crc32: u32,
}
#[derive(Debug)]
pub struct Blob {
    pub asset: String,
    pub bytes: Vec<u8>,
}
impl Blob {
    pub fn encode(&self) -> Result<Vec<u8>> {
        action_log::asset_relative_path(&self.asset)?;
        ensure!(
            self.bytes.len() <= MAX_BLOB_SIZE,
            "Attachment exceeds 64 MiB"
        );
        let header = serde_json::to_vec(&Header {
            asset: self.asset.clone(),
            size: self.bytes.len(),
            crc32: crc32fast::hash(&self.bytes),
        })?;
        ensure!(header.len() <= MAX_HEADER, "Attachment path is too long");
        let mut message = Vec::with_capacity(12 + header.len() + self.bytes.len());
        message.extend_from_slice(MAGIC);
        message.extend_from_slice(&(header.len() as u32).to_le_bytes());
        message.extend_from_slice(&header);
        message.extend_from_slice(&self.bytes);
        Ok(message)
    }
    pub fn decode(message: &[u8]) -> Result<Self> {
        ensure!(
            message.len() >= 12 && message.starts_with(MAGIC),
            "Invalid blob message"
        );
        let length = u32::from_le_bytes(message[8..12].try_into().unwrap()) as usize;
        ensure!(
            length <= MAX_HEADER && message.len() >= 12 + length,
            "Invalid blob header length"
        );
        let header: Header = serde_json::from_slice(&message[12..12 + length])?;
        action_log::asset_relative_path(&header.asset)?;
        let bytes = &message[12 + length..];
        ensure!(
            header.size <= MAX_BLOB_SIZE && bytes.len() == header.size,
            "Invalid blob size"
        );
        ensure!(
            crc32fast::hash(bytes) == header.crc32,
            "Blob checksum mismatch"
        );
        Ok(Self {
            asset: header.asset,
            bytes: bytes.to_vec(),
        })
    }
    pub fn read(root: &Path, asset: &str) -> Result<Self> {
        let path = checked_path(root, asset, false)?;
        let bytes = read_regular(&path)?;
        Ok(Self {
            asset: asset.to_owned(),
            bytes,
        })
    }
    /// Atomic, durable and immutable: retries may reuse identical files, never replace others.
    pub fn save(&self, root: &Path) -> Result<()> {
        ensure!(
            self.bytes.len() <= MAX_BLOB_SIZE,
            "Attachment exceeds 64 MiB"
        );
        let path = checked_path(root, &self.asset, true)?;
        if path.symlink_metadata().is_ok() {
            ensure!(
                read_regular(&path)? == self.bytes,
                "Attachment path already contains different bytes"
            );
            File::open(&path)?.sync_all()?;
            File::open(path.parent().context("Attachment has no parent")?)?.sync_all()?;
            return Ok(());
        }
        let parent = path.parent().context("Attachment has no parent")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&self.bytes)?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(&path) {
            Ok(_) => {
                File::open(parent)?.sync_all()?;
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                ensure!(
                    read_regular(&path)? == self.bytes,
                    "Attachment path already contains different bytes"
                );
                File::open(&path)?.sync_all()?;
                File::open(parent)?.sync_all()?;
            }
            Err(error) => return Err(error.error.into()),
        }
        Ok(())
    }
}
fn read_regular(path: &Path) -> Result<Vec<u8>> {
    let metadata = path
        .symlink_metadata()
        .with_context(|| format!("Missing attachment: {}", path.display()))?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "Attachment must be a regular file"
    );
    ensure!(
        metadata.len() <= MAX_BLOB_SIZE as u64,
        "Attachment exceeds 64 MiB"
    );
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_BLOB_SIZE as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_BLOB_SIZE, "Attachment exceeds 64 MiB");
    Ok(bytes)
}
/// Reject symlinks at every component so peers cannot read/write outside Attachments.
fn checked_path(root: &Path, asset: &str, create: bool) -> Result<PathBuf> {
    let relative = action_log::asset_relative_path(asset)?;
    let mut directory = root.to_owned();
    check_directory(&directory, create)?;
    for component in relative
        .parent()
        .context("Invalid attachment path")?
        .components()
    {
        directory.push(component);
        check_directory(&directory, create)?;
    }
    Ok(directory.join(
        relative
            .file_name()
            .context("Invalid attachment filename")?,
    ))
}
fn check_directory(path: &Path, create: bool) -> Result<()> {
    if create {
        match fs::create_dir(path) {
            Ok(()) => {
                File::open(
                    path.parent()
                        .context("Attachment directory has no parent")?,
                )?
                .sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = path
        .symlink_metadata()
        .with_context(|| format!("Missing attachment folder: {}", path.display()))?;
    ensure!(
        metadata.is_dir() && !metadata.file_type().is_symlink(),
        "Attachment directories must not be symlinks"
    );
    Ok(())
}
pub fn require_assets(root: &Path, record: &action_log::Record) -> Result<()> {
    for asset in action_log::referenced_assets(record)? {
        let path = checked_path(root, &asset, false)?;
        let metadata = path.symlink_metadata().with_context(|| {
            format!("Record references a missing attachment: {}", path.display())
        })?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "Attachment must be a regular file"
        );
        ensure!(
            metadata.len() <= MAX_BLOB_SIZE as u64,
            "Attachment exceeds 64 MiB"
        );
    }
    Ok(())
}

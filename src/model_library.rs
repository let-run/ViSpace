//! Machine-local default for KiCad's shared STEP/VRML model library.
use anyhow::{Context, Result, ensure};
use std::{
    path::{Path, PathBuf},
    sync::{OnceLock, RwLock},
};

fn path() -> Result<PathBuf> {
    Ok(crate::data_location::preference_path()?.with_file_name("model-library.json"))
}

fn setting() -> &'static RwLock<Option<String>> {
    static SETTING: OnceLock<RwLock<Option<String>>> = OnceLock::new();
    SETTING.get_or_init(|| {
        let saved = path()
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        RwLock::new(saved)
    })
}

pub fn configured_folder() -> Option<String> {
    setting().read().unwrap().clone()
}

pub fn save_folder(value: &str) -> Result<()> {
    use std::io::Write;
    let folder = if value.trim().is_empty() {
        None
    } else {
        let folder = Path::new(value.trim());
        ensure!(folder.is_absolute(), "Enter an absolute folder path");
        ensure!(folder.is_dir(), "Choose an existing model library folder");
        Some(folder.canonicalize()?.to_string_lossy().into_owned())
    };
    let path = path()?;
    let parent = path.parent().context("No configuration directory")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, &folder)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    *setting().write().unwrap() = folder;
    Ok(())
}

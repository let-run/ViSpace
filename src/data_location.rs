use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};

pub(crate) fn preference_path() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "BOMInventoryRust")
        .context("No application configuration directory")?;
    Ok(dirs.config_dir().join("data-location.json"))
}

pub fn configured_folder() -> Result<Option<PathBuf>> {
    match std::fs::read(preference_path()?) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn save_folder(folder: &Path) -> Result<()> {
    use std::io::Write;

    ensure!(folder.is_absolute(), "Enter an absolute folder path");
    std::fs::create_dir_all(folder)?;
    let folder = folder.canonicalize()?;
    // Verify that the selected folder can hold files before saving the choice.
    tempfile::NamedTempFile::new_in(&folder)?;
    let path = preference_path()?;
    let parent = path.parent().context("No configuration directory")?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, &folder)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

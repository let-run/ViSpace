use anyhow::Result;
use std::path::{Path, PathBuf};
pub mod fs {
    use super::*;
    pub fn read(path: impl AsRef<Path>) -> std::io::Result<Vec<u8>> {
        let key = format!("bom-inventory.file:{}", path.as_ref().display());
        let value = crate::web::storage()
            .map_err(std::io::Error::other)?
            .get_item(&key)
            .map_err(|e| std::io::Error::other(format!("{e:?}")))?
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "No saved preference")
            })?;
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(value)
            .map_err(std::io::Error::other)
    }
    pub fn write(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
        let path = path.as_ref();
        let bytes = bytes.as_ref();
        if path.starts_with("/download") {
            return crate::web::download(
                &path.file_name().unwrap_or_default().to_string_lossy(),
                bytes,
                "application/octet-stream",
            )
            .map_err(std::io::Error::other);
        }
        use base64::Engine;
        crate::web::storage()
            .map_err(std::io::Error::other)?
            .set_item(
                &format!("bom-inventory.file:{}", path.display()),
                &base64::engine::general_purpose::STANDARD.encode(bytes),
            )
            .map_err(|e| std::io::Error::other(format!("{e:?}")))
    }
    pub fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> std::io::Result<()> {
        write(to, read(&from)?)?;
        remove_file(from)
    }
    pub fn remove_file(path: impl AsRef<Path>) -> std::io::Result<()> {
        crate::web::storage()
            .map_err(std::io::Error::other)?
            .remove_item(&format!("bom-inventory.file:{}", path.as_ref().display()))
            .map_err(|e| std::io::Error::other(format!("{e:?}")))
    }
    pub fn create_dir_all(_path: impl AsRef<Path>) -> std::io::Result<()> {
        Ok(())
    }
    pub struct Metadata {
        len: u64,
    }
    impl Metadata {
        pub fn is_empty(&self) -> bool {
            self.len == 0
        }
        pub fn len(&self) -> u64 {
            self.len
        }
    }
    pub fn metadata(path: impl AsRef<Path>) -> std::io::Result<Metadata> {
        Ok(Metadata {
            len: read(path)?.len() as u64,
        })
    }
    pub struct File {
        bytes: std::io::Cursor<Vec<u8>>,
        target: Option<PathBuf>,
    }
    impl File {
        pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
            Ok(Self {
                bytes: std::io::Cursor::new(read(path)?),
                target: None,
            })
        }
        pub fn create(path: impl AsRef<Path>) -> std::io::Result<Self> {
            Ok(Self {
                bytes: std::io::Cursor::new(vec![]),
                target: Some(path.as_ref().into()),
            })
        }
    }
    impl std::io::Read for File {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            std::io::Read::read(&mut self.bytes, b)
        }
    }
    impl std::io::Write for File {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            std::io::Write::write(&mut self.bytes, b)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Drop for File {
        fn drop(&mut self) {
            if let Some(path) = &self.target {
                let _ = write(path, self.bytes.get_ref());
            }
        }
    }
}
pub mod temporary {
    use super::*;
    pub struct TempDir {
        path: PathBuf,
    }
    impl TempDir {
        pub fn path(&self) -> &Path {
            &self.path
        }
    }
    pub fn tempdir() -> std::io::Result<TempDir> {
        Ok(TempDir {
            path: PathBuf::from(format!("/draft/{}", crate::model::id())),
        })
    }
}
pub mod dialogs {
    use super::*;
    #[derive(Default)]
    pub struct FileDialog {
        name: String,
        extensions: Vec<String>,
    }
    impl FileDialog {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn add_filter(mut self, _name: &str, extensions: &[&str]) -> Self {
            self.extensions = extensions.iter().map(|s| (*s).into()).collect();
            self
        }
        pub fn set_file_name(mut self, name: impl Into<String>) -> Self {
            self.name = name.into();
            self
        }
        pub fn save_file(self) -> Option<PathBuf> {
            Some(PathBuf::from("/download").join(if self.name.is_empty() {
                "inventory.csv"
            } else {
                &self.name
            }))
        }
        pub fn pick_file(self) -> Option<PathBuf> {
            crate::web::choose_file(&self.extensions);
            None
        }
        pub fn pick_folder(self) -> Option<PathBuf> {
            crate::web::notice(
                "Folder backups are desktop-only. Use Export inventory in the browser.",
            );
            None
        }
    }
}
pub mod clipboard {
    use super::*;
    pub struct Clipboard;
    pub struct ImageData<'a> {
        pub width: usize,
        pub height: usize,
        pub bytes: std::borrow::Cow<'a, [u8]>,
    }
    impl Clipboard {
        pub fn new() -> Result<Self> {
            Ok(Self)
        }
        pub fn get_text(&mut self) -> Result<String> {
            anyhow::bail!("Paste text with Ctrl/⌘V in the browser")
        }
        pub fn get_image(&mut self) -> Result<ImageData<'static>> {
            anyhow::bail!("Use the image file button in the browser")
        }
        pub fn set_image(&mut self, image: ImageData<'_>) -> Result<()> {
            crate::web::copy_image(image)
        }
        pub fn get(&mut self) -> &mut Self {
            self
        }
        pub fn file_list(&mut self) -> Result<Vec<PathBuf>> {
            anyhow::bail!("Clipboard file lists are unavailable in browsers")
        }
    }
}
pub fn open_link(path: impl AsRef<std::ffi::OsStr>) -> std::io::Result<()> {
    let value = path.as_ref().to_string_lossy();
    if !value.starts_with("https://")
        && !value.starts_with("http://")
        && !value.starts_with("data:")
    {
        return Err(std::io::Error::other(
            "Desktop file links cannot be opened from a browser",
        ));
    }
    web_sys::window()
        .unwrap()
        .open_with_url_and_target(&value, "_blank")
        .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
    Ok(())
}

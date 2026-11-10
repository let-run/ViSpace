//! Temporary image/PDF drafts are promoted only when their editor is saved.
use anyhow::{Context, Result, ensure};
use image::{DynamicImage, ImageReader};
use serde_json::{Value, json};
use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

pub struct AttachmentDraft {
    directory: crate::platform::temporary::TempDir,
}
impl AttachmentDraft {
    pub fn new() -> Result<Self> {
        Ok(Self {
            directory: crate::platform::temporary::tempdir()?,
        })
    }
    pub fn root(&self) -> &Path {
        self.directory.path()
    }
    pub fn import_image(&self, path: &Path) -> Result<Value> {
        ensure!(
            crate::platform::fs::metadata(path)?.len() <= 30_000_000,
            "Image file exceeds 30 MB"
        );
        self.image_bytes(&crate::platform::fs::read(path)?)
    }
    pub fn image_bytes(&self, bytes: &[u8]) -> Result<Value> {
        let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(10_000);
        limits.max_image_height = Some(10_000);
        limits.max_alloc = Some(128_000_000);
        reader.limits(limits);
        self.stage_image(reader.decode().context("Choose a supported image file")?)
    }
    pub fn rgba(&self, width: usize, height: usize, bytes: Vec<u8>) -> Result<Value> {
        let pixels = width.checked_mul(height).context("Image is too large")?;
        ensure!(
            width > 0 && height > 0 && width <= 10_000 && height <= 10_000 && pixels <= 20_000_000,
            "Image dimensions exceed the limit"
        );
        ensure!(
            pixels.checked_mul(4) == Some(bytes.len()),
            "Invalid clipboard image data"
        );
        let image = image::RgbaImage::from_raw(width as u32, height as u32, bytes)
            .context("Invalid clipboard image")?;
        self.stage_image(DynamicImage::ImageRgba8(image))
    }
    pub fn clipboard_image(&self) -> Result<Value> {
        let mut clipboard =
            crate::platform::clipboard::Clipboard::new().context("Cannot access the clipboard")?;
        if let Ok(image) = clipboard.get_image() {
            return self.rgba(image.width, image.height, image.bytes.into_owned());
        }
        if let Ok(paths) = clipboard.get().file_list()
            && let Some(path) = paths.first()
        {
            return self.import_image(path);
        }
        anyhow::bail!("Copy an image or image file to the clipboard first.")
    }
    fn stage_image(&self, image: DynamicImage) -> Result<Value> {
        ensure!(
            u64::from(image.width()) * u64::from(image.height()) <= 20_000_000,
            "Image exceeds 20 megapixels"
        );
        #[cfg(target_arch = "wasm32")]
        {
            use base64::Engine;
            let uri = |image: DynamicImage| -> Result<String> {
                let mut bytes = Cursor::new(Vec::new());
                image.write_to(&mut bytes, image::ImageFormat::Png)?;
                Ok(format!(
                    "data:image/png;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
                ))
            };
            Ok(
                json!({"thumbnailPath":uri(image.thumbnail(160,160))?,"previewPath":uri(image.thumbnail(800,800))?,"originalPath":uri(image)?}),
            )
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let folder = self.root().join(crate::model::id());
            crate::platform::fs::create_dir_all(&folder)?;
            let original = folder.join("original.png");
            let thumbnail = folder.join("thumbnail.png");
            let preview = folder.join("preview.png");
            image.save(&original)?;
            image.thumbnail(160, 160).save(&thumbnail)?;
            image.thumbnail(800, 800).save(&preview)?;
            Ok(json!({"originalPath":original,"thumbnailPath":thumbnail,"previewPath":preview}))
        }
    }
    pub fn import_pdf(&self, path: &Path) -> Result<PathBuf> {
        ensure!(
            crate::platform::fs::metadata(path)?.len() <= 30_000_000,
            "PDF exceeds 30 MB"
        );
        let bytes = crate::platform::fs::read(path)?;
        ensure!(bytes.starts_with(b"%PDF-"), "File is not a PDF");
        let target = self.root().join(format!("{}.pdf", crate::model::id()));
        crate::platform::fs::write(&target, bytes)?;
        Ok(target)
    }
}

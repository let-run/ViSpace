//! Narrow host adapters; desktop behavior is unchanged.
#[cfg(not(target_arch = "wasm32"))]
pub mod clipboard {
    pub use arboard::*;
}
#[cfg(not(target_arch = "wasm32"))]
pub mod dialogs {
    pub use rfd::*;
}
#[cfg(not(target_arch = "wasm32"))]
pub mod temporary {
    pub use tempfile::*;
}
#[cfg(not(target_arch = "wasm32"))]
pub mod fs {
    pub use std::fs::*;
}
#[cfg(not(target_arch = "wasm32"))]
pub fn open_link(path: impl AsRef<std::ffi::OsStr>) -> std::io::Result<()> {
    open::that(path)
}
#[cfg(target_arch = "wasm32")]
pub use crate::web::platform::*;

pub fn image_uri(path: &str) -> String {
    if path.starts_with("data:") || path.starts_with("https://") || path.starts_with("blob:") {
        path.into()
    } else {
        format!("file://{path}")
    }
}

pub fn image_widget(path: &str) -> eframe::egui::Image<'static> {
    use base64::Engine;
    if path.starts_with("data:image/")
        && let Some((_, encoded)) = path.split_once(',')
        && let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(encoded)
    {
        eframe::egui::Image::from_bytes(path.to_owned(), bytes)
    } else {
        eframe::egui::Image::new(image_uri(path))
    }
}

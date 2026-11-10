//! Portable board scene building; no inventory, filesystem, or platform window dependencies.
pub mod cache;
mod scene;
pub use scene::*;
#[cfg(feature = "egui")]
mod gpu;
#[cfg(feature = "egui")]
pub use gpu::{Camera, PickRay, draw, draw_selected, draw_with_brightness, register, release};

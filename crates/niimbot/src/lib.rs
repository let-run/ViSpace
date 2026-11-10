//! B1 (model 4096) protocol ported from the Swift implementation.
//! Protocol attribution: repository THIRD_PARTY_NOTICES.md (iscarelli, MIT).
mod protocol;
pub use protocol::*;
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub use browser::*;

pub const SERVICE: &str = "e7810a71-73ae-499d-8c15-faa9aef0c3f2";
pub const CHARACTERISTIC: &str = "bef8d6c9-9c21-4c9e-b632-bd58c1009f9f";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
}

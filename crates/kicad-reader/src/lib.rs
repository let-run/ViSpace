//! Read-only, handwritten KiCad 6+ readers. No KiCad/parser/rendering dependencies.
//! Coordinates in the scene are millimetres, with schematic Y increasing downwards.
pub mod json;
pub mod nets;
mod pcb;
mod project;
pub use pcb::*;
mod schematic;
pub mod syntax;
pub use project::*;
pub use schematic::*;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    pub message: String,
    pub offset: usize,
    pub line: usize,
    pub column: usize,
}
impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            offset: 0,
            line: 1,
            column: 1,
        }
    }
    pub(crate) fn at(source: &str, offset: usize, message: impl Into<String>) -> Self {
        let prefix = &source[..offset.min(source.len())];
        Self {
            message: message.into(),
            offset,
            line: prefix.bytes().filter(|b| *b == b'\n').count() + 1,
            column: prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (line {}, column {})",
            self.message, self.line, self.column
        )
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
pub const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;

mod net_classes;
pub use net_classes::NetClasses;

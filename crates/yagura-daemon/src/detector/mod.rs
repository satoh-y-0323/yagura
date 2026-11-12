//! State detection engine: turns the copy of a window's screen its holder
//! sends into a confirmed `WindowStatus`. This file only declares the module
//! tree and re-exports the public surface.

pub mod definition;
pub mod engine;
pub mod layers;
pub mod reading;
pub mod source;

pub use engine::{ConfirmedStatus, DetectionEngine, DetectorHandle, InputEcho};
pub use reading::{CopyCounts, CopyRead, CopyRefused, HeldCopy, SharedCopy};

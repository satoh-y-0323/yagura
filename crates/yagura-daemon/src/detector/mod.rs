//! State detection engine: turns raw PTY output into a confirmed
//! `WindowStatus`. This file only declares the module tree and re-exports the
//! public surface.

pub mod definition;
pub mod engine;
pub mod layers;
pub mod screen;
mod screen_model;
pub mod screen_view;
pub mod source;

pub use engine::{ConfirmedStatus, DetectionEngine, DetectorHandle, InputEcho};
pub use screen::{ScreenIntegrity, SharedScreen, VtScreen};

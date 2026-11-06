//! A window's terminal: the model a child's output is fed into, the views a
//! client is shown of it, and the replies to the child's terminal queries.
//!
//! The terminal engine (`alacritty_terminal`) is held inside the bounds this
//! crate keeps, since the child's output is untrusted. Policy (what a screen
//! means) is not here: readers decide that.

mod projection;
pub mod screen;
mod screen_model;
pub mod screen_view;

pub use projection::Projector;
pub use screen::{PtyHost, ScreenIntegrity, SharedScreen, VtScreen};

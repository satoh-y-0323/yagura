//! yagura GUI: the Tauri shell around the terminal panes.
//!
//! The Rust core is an IPC client only. It owns no PTY and no child process
//! other than the detached daemon it may have to start.
//!
//! The binary target is a thin shell over this library: it only assembles the
//! Tauri application, so every item it needs is public here.

pub mod client_config;
pub mod commands;
pub mod config_paths;
pub mod connection_pool;
pub mod constants;
pub mod daemon_launch;
pub mod daemon_supervisor;
pub mod events;
pub mod frame_mailbox;
pub mod host;
pub mod instance;
pub mod ipc_client;
pub mod keymap;
pub mod message_dispatch;
pub mod pool_outcomes;
pub mod recovery_state;
pub mod themes;
pub mod window_list_poll;

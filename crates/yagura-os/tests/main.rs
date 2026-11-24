//! Every integration test of this crate, built as one test binary: each file in
//! this directory is a module listed below, and a file not listed is not built.

mod bundled_conpty;
mod config_replace;
mod config_seed;
mod debug_forms;
#[cfg(unix)]
mod descriptor_limit_unix;
mod endpoint;
mod enter_raw_console_input;
mod existing_file_replace;
#[cfg(unix)]
mod exit_watch_unix;
mod holder_endpoint;
mod hook_path;
mod host_os;
mod instance;
mod ipc_close;
mod login_shell;
mod mcp_bridge;
mod os_peer_executable;
mod os_security;
mod os_unix;
mod pipe_accept;
mod process_identity;
mod program_resolution;
mod pty_unix;
mod reap_gate;
mod spawn_fd_inheritance;
#[cfg(unix)]
mod tree_end_unix;
#[cfg(target_os = "linux")]
mod user_manager_linux;
mod window_lineage;

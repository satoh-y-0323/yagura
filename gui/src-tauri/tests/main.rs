//! Every integration test of this crate, built as one test binary: each file in
//! this directory is a module listed below, and a file not listed is not built.

mod attention_summary;
mod client_config_resolution;
mod frame_mailbox;
mod input_bounds;
mod instance_resolution;
mod keymap_file;
mod lib_constants;
mod recovery_disengage;
mod recovery_state_machine;
mod rejection_bridge;

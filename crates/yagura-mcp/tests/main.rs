//! Every integration test of this crate, built as one test binary: each file in
//! this directory is a module listed below, and a file not listed is not built.

mod cancellation;
mod common;
mod composer_fixture;
mod e2e;
mod eager_identify;
mod instance_resolution;
mod list_windows_surface;
mod plan_review_tools;
mod plan_tools;
mod protocol_revision_gate;
mod rejected_argument_bridge;
mod report_tools;
mod stdio_bridge;
mod ticket_e2e;
mod ticket_tools;
mod tool_listing;
mod tools;
mod tools_chat_boundary_probe;
mod transport_frames;
mod unsupported_mapping;
mod window_name_tools;

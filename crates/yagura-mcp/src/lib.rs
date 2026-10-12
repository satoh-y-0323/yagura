//! `yagura-mcp`: a thin MCP stdio bridge in front of the yagura daemon.
//!
//! Owns no PTYs or child processes: it forwards MCP tool calls to the daemon's
//! local IPC endpoint and maps the daemon's responses onto the MCP tool
//! contract.

pub mod plan_tools;
pub mod report_tools;
pub mod tools;
pub mod transport;

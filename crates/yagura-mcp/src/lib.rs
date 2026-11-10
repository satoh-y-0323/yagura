//! `yagura-mcp`: the relay a CLI starts as its MCP server.
//!
//! Owns no PTYs, child processes or MCP tools: it carries the CLI's JSON-RPC
//! lines to the daemon, which serves MCP, over the relay contract
//! ([`yagura_protocol::relay`]).

pub mod relay;

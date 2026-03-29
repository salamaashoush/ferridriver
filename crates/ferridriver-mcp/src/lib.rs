//! ferridriver-mcp -- Browser automation MCP server library.
//!
//! Provides a fully-functional MCP server for browser automation that
//! consumers can customize and extend:
//!
//! - Implement [`McpServerConfig`] to control chrome args (base + per-instance),
//!   server metadata, and pre-dispatch hooks.
//! - Use [`McpServer::with_extra_tools`] to compose additional tool routers.
//! - Use [`McpServer::with_extension`] to attach custom state accessible
//!   from tool handlers via [`McpServer::extension`].

pub mod mcp;
pub mod params;
pub mod server;
pub mod tools;

// Re-export key types at crate root for ergonomic imports.
pub use server::{DefaultConfig, McpServer, McpServerConfig, State};

//! MCP tool definitions -- split by category, each with its own `tool_router`.
//!
//! Each submodule defines tools in a separate `#[tool_router]` impl block.
//! Routers are combined via `+` in `McpServer::tool_router()`.
//!
//! # Surface by category
//!
//! - **navigation** — session bootstrap: `connect`, `navigate`, `page`
//! - **content** — observation + light JS: `snapshot`, `screenshot`,
//!   `evaluate`, `search_page`
//! - **network** — session diagnostics: `diagnostics`
//! - **script** — imperative scripting: `run_script` (the action path)
//!
//! Browser interaction flows through `run_script`, which exposes `page`,
//! `context`, and `request` globals over the ferridriver core.

pub mod content;
pub mod navigation;
pub mod network;
pub mod script;

use crate::server::McpServer;
use rmcp::handler::server::router::tool::ToolRouter;

impl McpServer {
  /// Build the base tool router by combining all category routers.
  ///
  /// Stored in a field on construction so consumers can merge extra tools
  /// via `with_extra_tools()`. The `#[tool_handler]` macro dispatches
  /// through the field (`self.tool_router`) rather than calling this method.
  #[must_use]
  pub fn tool_router() -> ToolRouter<Self> {
    Self::navigation_router() + Self::content_router() + Self::network_router() + Self::script_router()
  }
}

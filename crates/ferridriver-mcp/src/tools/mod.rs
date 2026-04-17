//! MCP tool definitions -- split by category, each with its own `tool_router`.
//!
//! Each submodule defines tools in a separate `#[tool_router]` impl block.
//! Routers are combined via `+` in `McpServer::tool_router()`.

pub mod bdd;
pub mod content;
pub mod cookies;
pub mod emulation;
pub mod input;
pub mod navigation;
pub mod network;
pub mod script;
pub mod storage;

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
    Self::navigation_router()
      + Self::input_router()
      + Self::content_router()
      + Self::cookies_router()
      + Self::storage_router()
      + Self::emulation_router()
      + Self::network_router()
      + Self::bdd_router()
      + Self::script_router()
  }
}

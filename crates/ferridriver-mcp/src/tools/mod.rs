//! MCP tool definitions -- split by category, each with its own `tool_router`.
//!
//! Each submodule defines tools in a separate `#[tool_router]` impl block.
//! Routers are combined via `+` in `McpServer::tool_router()`.
//!
//! # Surface by category
//!
//! - **navigation** — session bootstrap: `connect`, `navigate`, `page`
//! - **content** — observation + light JS: `snapshot`, `screenshot`, `evaluate`,
//!   `wait_for`, `search_page`, `find_elements`, `get_markdown`
//! - **network** — session diagnostics: `diagnostics`
//! - **script** — imperative scripting: `run_script` (the action path)
//!
//! Low-level action tools (click/fill/hover/type/press/etc.), state-setter
//! tools (cookies/storage/emulation), and BDD/Gherkin tools are intentionally
//! not exposed here. Scripts drive browser interaction via the `page`,
//! `context`, and `request` globals inside `run_script`; BDD/Gherkin lives in
//! the test-runner path (`bun test` through ferridriver-test), not in MCP.

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

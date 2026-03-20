//! MCP tool definitions — split by category, each with its own tool_router.
//!
//! Each submodule defines tools in a separate `#[tool_router]` impl block.
//! Routers are combined via `+` in `ChromeyMcp::combined_router()`.

pub mod navigation;
pub mod input;
pub mod content;
pub mod cookies;
pub mod storage;
pub mod emulation;
pub mod network;
pub mod bdd;
pub mod lifecycle;

use crate::server::ChromeyMcp;
use rmcp::handler::server::router::tool::ToolRouter;

impl ChromeyMcp {
    /// Combine all category routers into one.
    pub fn combined_router() -> ToolRouter<Self> {
        Self::navigation_router()
            + Self::input_router()
            + Self::content_router()
            + Self::cookies_router()
            + Self::storage_router()
            + Self::emulation_router()
            + Self::network_router()
            + Self::bdd_router()
            + Self::lifecycle_router()
    }
}

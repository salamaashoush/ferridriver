//! ferridriver -- High-performance browser automation library.
//!
//! Provides a Playwright-compatible API for browser automation across
//! multiple backends (CDP WebSocket, CDP Pipes, native `WebKit`).
//!
//! # Quick Start
//!
//! ```ignore
//! use ferridriver::{Browser, Page};
//! use ferridriver::options::RoleOptions;
//!
//! let browser = Browser::launch().await?;
//! let page = browser.new_page_with_url("https://example.com").await?;
//!
//! // Playwright-style locators
//! page.get_by_role("link", RoleOptions { name: Some("More".into()), ..Default::default() })
//!     .click().await?;
//!
//! // Content extraction
//! let title = page.title().await?;
//! let md = page.markdown().await?;
//! ```

// ── Public API (Playwright-compatible) ──
pub mod browser;
pub mod context;
pub mod events;
pub mod frame;
pub mod locator;
pub mod options;
pub mod page;

pub use browser::Browser;
pub use context::{BrowserContext, ContextRef};
pub use events::{EventEmitter, PageEvent};
pub use frame::Frame;
pub use locator::Locator;
pub use page::Page;

// ── Public lower-level modules (needed by MCP server and consumers) ──
pub mod backend;
pub mod route;
pub mod snapshot;
pub mod state;

// ── Implementation modules (used by MCP server, will be internalized) ──
pub mod actions;
pub mod selectors;

// ── BDD steps (use crate-internal APIs) ──
#[macro_use]
pub mod steps;

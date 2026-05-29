//! ferridriver -- Rust-based browser automation library.
//!
//! Provides a Playwright-shaped API for browser automation across
//! multiple backends (CDP WebSocket, CDP Pipes, native `WebKit`,
//! `WebDriver` `BiDi`).
//!
//! # Quick Start
//!
//! ```ignore
//! use ferridriver::{chromium, Page};
//! use ferridriver::options::{LaunchOptions, RoleOptions};
//!
//! let browser = chromium().launch(LaunchOptions::default()).await?;
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
pub mod browser_type;
pub mod console_message;
pub mod context;
pub mod dialog;
pub mod disposable;
pub mod download;
pub mod element_handle;
pub mod error;
pub mod events;
pub mod file_chooser;
pub mod frame;
pub mod har;
pub(crate) mod frame_cache;
pub mod js_handle;
pub mod locator;
pub mod network;
pub mod options;
pub mod page;
pub mod protocol;
pub mod url_matcher;
pub mod web_error;

pub use browser::Browser;
pub use browser_type::{BrowserType, chromium, firefox, webkit};
pub use context::{BrowserContext, ContextRef};
pub use disposable::Disposable;
pub use element_handle::{BoundingBox, ElementHandle, ElementState};
pub use error::{FerriError, Result};
pub use events::{
  BindingSource, ContextEvent, ContextEventEmitter, EventEmitter, ExposedBinding, ExposedFn, PageEvent,
};
pub use frame::Frame;
pub use js_handle::{HandleRemote, JSHandle};
pub use locator::{FrameLocator, Locator};
pub use page::Page;
pub use url_matcher::{UrlMatcher, UrlPredicate};
pub use video::Video;

// ── Public lower-level modules (needed by MCP server and consumers) ──
pub mod backend;
pub mod route;
pub mod snapshot;
pub mod state;

// ── Browser installation ──
pub mod install;

// ── Implementation modules (used by MCP server, will be internalized) ──
pub mod actions;
pub mod codegen;
pub mod ffmpeg;
pub mod http_client;
pub mod selectors;
pub mod video;

// ── BDD steps (use crate-internal APIs) ──
#[macro_use]
pub mod steps;

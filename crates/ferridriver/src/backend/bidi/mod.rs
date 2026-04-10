//! WebDriver BiDi backend for cross-browser automation.
//!
//! Implements the full ferridriver API over the W3C WebDriver BiDi protocol.
//! Supports Chrome, Firefox, and future Safari via the standardized protocol.
//!
//! Architecture:
//! - `BidiTransport`: WebSocket I/O with zero-alloc hot-path dispatch (json_scan)
//! - `BidiSession`: HTTP session creation + capability negotiation
//! - `BidiBrowser`: Browser lifecycle, context management
//! - `BidiPage`: Full page API (~50 methods) mapped to BiDi commands
//! - `BidiElement`: Element interactions via SharedReferences
//! - `input`: Action builders for mouse, keyboard, wheel input

pub mod browser;
pub mod element;
pub mod input;
pub mod page;
pub(crate) mod session;
pub(crate) mod transport;
pub mod types;

pub use browser::BidiBrowser;
pub use element::BidiElement;
pub use page::BidiPage;

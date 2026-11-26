//! Protocol wire transports for ferridriver.
//!
//! - [`bidi`]: `WebDriver` `BiDi` WebSocket transport with command correlation
//!   and event fan-out.
//! - [`webkit`]: Playwright `WebKit` Inspector NUL-delimited JSON pipe
//!   transport, envelope schema, and multi-level JSON-RPC connection router.

pub mod bidi;
pub mod webkit;

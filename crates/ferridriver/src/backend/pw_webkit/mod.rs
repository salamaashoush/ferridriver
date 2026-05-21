//! Playwright `WebKit` backend.
//!
//! Speaks Playwright's `WebKit` Inspector protocol over a NUL-byte-delimited
//! JSON pipe to a `pw_run.sh` (or `Playwright.app`) child process. Same
//! transport / message envelope on every platform.

pub mod browser;
pub mod connection;
pub mod launcher;
pub mod page;
pub mod protocol;
pub mod transport;

pub use browser::{Browser, BrowserError};
pub use connection::{BrowserSession, Connection, ConnectionError, PageProxySession, Session, TargetSession};
pub use launcher::{LaunchConfig, LaunchError, locate_binary, spawn};
pub use page::{Page, PageRef};
pub use transport::{Transport, TransportError};

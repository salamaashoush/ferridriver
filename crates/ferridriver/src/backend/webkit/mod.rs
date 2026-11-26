//! Playwright `WebKit` backend.
//!
//! Speaks Playwright's `WebKit` Inspector protocol over a NUL-byte-delimited
//! JSON pipe to a `pw_run.sh` child process. Same transport / message
//! envelope on every platform.

pub mod browser;
pub use ferridriver_transport::webkit::connection;
pub mod element;
pub mod events;
pub mod input;
pub mod launcher;
pub(crate) mod owner;
pub mod page;
pub use ferridriver_transport::webkit::protocol;
pub use ferridriver_transport::webkit::transport;
pub(crate) mod worker;

pub use browser::{BrowserError, WebKitBrowser};
pub use connection::{Connection, ConnectionError, Session};
pub use element::WebKitElement;
pub use launcher::{LaunchConfig, LaunchError, locate_binary};
pub use page::WebKitPage;
pub use transport::{Transport, TransportError};
pub use worker::WebKitWorker;

//! Playwright `WebKit` Inspector wire protocol, NUL-delimited pipe transport,
//! and multi-level JSON-RPC connection router.

pub mod connection;
pub mod protocol;
pub mod transport;

pub use connection::{Connection, ConnectionError, RouteObserver, Session};
pub use protocol::{Envelope, ErrorPayload};
pub use transport::{ReaderHandle, Transport, TransportError, WriterHandle};

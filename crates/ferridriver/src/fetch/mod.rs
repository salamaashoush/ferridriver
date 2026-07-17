//! A spec-faithful WHATWG Fetch model and the single send engine over
//! reqwest. Both the `fetch` global and the Playwright `request` API
//! marshal into the same [`Request`], go through the same [`engine::send`]
//! path, and read back the same [`Response`] — there is no second code
//! path.
//!
//! Layout:
//! - [`headers`] — the WHATWG header list.
//! - [`body`] — request/response body (`Empty` / `Bytes` / stream).
//! - [`model`] — [`Request`] / [`Response`] + the `RedirectMode` /
//!   `Credentials` / `ResponseType` enums and `RemoteAddr`.
//! - [`error`] — the typed [`FetchError`].
//! - [`engine`] — the client pool and the one manual-redirect send loop.
//! - [`net_guard`] — SSRF policy (allow-list, metadata/private blocking).
//! - [`cookie`] — RFC 6265 parsing/matching for the context-bound path.
//! - [`multipart`] — `multipart/form-data` serialization.
//! - [`bridge`] — the two-way browser-context cookie/defaults bridge.

pub mod body;
pub mod bridge;
pub mod cookie;
pub mod engine;
pub mod error;
pub mod headers;
pub mod model;
pub mod multipart;
pub mod net_guard;

pub use body::{Body, ByteStream};
pub use bridge::{BridgeFuture, ContextBridge, ContextDefaults};
pub use error::FetchError;
pub use headers::Headers;
pub use model::{Credentials, RedirectMode, RemoteAddr, Request, Response, ResponseType};
pub use multipart::{MultipartField, MultipartValue, serialize_multipart};
pub use net_guard::{NetGuard, host_allowed, host_of};

pub(crate) use engine::{ClientPool, send};
pub(crate) use multipart::multipart_boundary;

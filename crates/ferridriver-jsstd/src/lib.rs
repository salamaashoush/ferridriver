//! Vendored subset of [awslabs/llrt](https://github.com/awslabs/llrt)
//! (Apache-2.0): the WHATWG Streams implementation plus the pieces it
//! needs (`llrt_utils`, `llrt_events`, `llrt_exceptions`, `llrt_abort`).
//!
//! Upstream crate -> module here:
//!
//! | upstream            | module        |
//! |---------------------|---------------|
//! | `llrt_utils`        | [`utils`]     |
//! | `llrt_context`      | [`context`]   |
//! | `llrt_exceptions`   | [`exceptions`]|
//! | `llrt_events`       | [`events`]    |
//! | `llrt_abort`        | [`abort`]     |
//! | `llrt_os`           | [`os`]        |
//! | `llrt_encoding`     | [`encoding`]  |
//! | `llrt_buffer`       | [`buffer`]    |
//! | `llrt_json`         | [`json`]      |
//! | `llrt_crypto`       | [`crypto`]    |
//! | `llrt_stream_web`   | [`stream_web`]|
//! | `llrt_test`         | `test` (dev)  |
//!
//! Sources are kept byte-close to upstream — only `crate::` / `llrt_*`
//! path prefixes are rewritten — so a re-sync stays a mechanical diff.
//! Ferridriver-specific behaviour belongs in `ferridriver-script`, not
//! here.

pub mod abort;
pub mod context;
pub mod buffer;
pub mod crypto;
pub mod encoding;
pub mod events;
pub mod exceptions;
pub mod json;
pub mod modules;
/// Node modules ferridriver implements itself, because upstream llrt has
/// none or only a stub. Written to the repo's style, but compiled under
/// this crate's relaxed lints: pedantic's `needless_pass_by_value` is
/// unsatisfiable for rquickjs callback signatures, which must take owned
/// JS values.
pub mod node;
pub mod os;
pub mod stream_web;
pub mod utils;

#[cfg(test)]
mod test;

use rquickjs::{Ctx, Result};

/// Install every vendored global on `ctx`: `DOMException`, `Event` /
/// `EventTarget`, `AbortController` / `AbortSignal`, the full Streams
/// surface, and `Buffer` / `Blob` / `File`.
///
/// One entry point, so a host cannot install half the crate.
pub fn init(ctx: &Ctx<'_>) -> Result<()> {
  exceptions::init(ctx)?;
  events::init(ctx)?;
  abort::init(ctx)?;
  stream_web::init(ctx)?;
  buffer::init(ctx)?;
  Ok(())
}

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
//! | `llrt_stream_web`   | [`stream_web`]|
//! | `llrt_test`         | `test` (dev)  |
//!
//! Sources are kept byte-close to upstream — only `crate::` / `llrt_*`
//! path prefixes are rewritten — so a re-sync stays a mechanical diff.
//! Ferridriver-specific behaviour belongs in `ferridriver-script`, not
//! here.

pub mod abort;
pub mod context;
pub mod events;
pub mod exceptions;
pub mod stream_web;
pub mod utils;

#[cfg(test)]
mod test;

use rquickjs::{Ctx, Result};

/// Install every vendored global (`DOMException`, `Event`/`EventTarget`,
/// `AbortController`/`AbortSignal`, and the full Streams surface) on
/// `ctx`.
pub fn init(ctx: &Ctx<'_>) -> Result<()> {
  exceptions::init(ctx)?;
  events::init(ctx)?;
  abort::init(ctx)?;
  stream_web::init(ctx)?;
  Ok(())
}

//! `node:process` — the module form of the `process` global.
//!
//! Not a second implementation: the object this hands back IS
//! `globalThis.process`, so `import process from 'node:process'`,
//! `require('process')` and the bare global are one object, as in Node.

use rquickjs::{Ctx, Object, Result};

/// The names the module re-exports. `process` itself is installed by the
/// host; anything it does not set simply does not appear.
pub const PROCESS_MEMBERS: &[&str] = &[
  "argv",
  "argv0",
  "arch",
  "cwd",
  "env",
  "exit",
  "hrtime",
  "nextTick",
  "pid",
  "platform",
  "release",
  "stderr",
  "stdout",
  "version",
  "versions",
];

/// `globalThis.process`.
///
/// # Errors
///
/// When the host installed no `process` global.
pub fn process_object<'js>(ctx: &Ctx<'js>) -> Result<Object<'js>> {
  ctx.globals().get("process")
}

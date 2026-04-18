//! Wire-level protocol types shared with Playwright.
//!
//! The sub-modules mirror `/tmp/playwright/packages/playwright-core/src/protocol/`
//! and `/tmp/playwright/packages/protocol/src/channels.d.ts` on the Rust side so
//! that `page.evaluate(fn, arg)`, `JSHandle`, and `ElementHandle` can round-trip
//! rich JS values (`NaN` / ±`Infinity` / `Date` / `RegExp` / `URL` / `BigInt` /
//! `Map` / `Set` / `Error` / typed arrays) across the CDP / `BiDi` / `WebKit`
//! boundary the same way Playwright's own client does.

pub mod serializers;

pub use serializers::{
  ErrorValue, PropertyEntry, RegExpValue, SerializedArgument, SerializedValue, SpecialValue, TypedArrayKind,
  TypedArrayValue,
};

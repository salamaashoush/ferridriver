//! NAPI-RS native addon for ferridriver browser automation.
//!
//! Exposes the Playwright-compatible Browser/Page/Locator API to Node.js.

#![allow(unsafe_code)]
#![allow(clippy::needless_pass_by_value)]

mod browser;
mod locator;
mod page;
mod types;

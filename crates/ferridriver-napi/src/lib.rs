//! NAPI-RS native addon for ferridriver browser automation.
//!
//! Exposes the Playwright-compatible Browser/Page/Locator API to Node.js.

#![allow(unsafe_code)]
#![allow(clippy::needless_pass_by_value)]

mod browser;
mod context;
mod frame;
mod locator;
mod page;
mod test_runner;
mod types;

//! Backend abstraction layer for browser automation.
//!
//! Provides a unified API across multiple browser protocols:
//! - `cdp` (`CdpPipe` / `CdpWs`): Chrome `DevTools` Protocol over Unix pipes (fd 3/4) or WebSocket
//! - `bidi`: `WebDriver` `BiDi` over WebSocket (Firefox, Ladybird, Chromium, Safari)
//! - `webkit`: Playwright's `WebKit` build over `pw_run.sh` (NUL-delimited JSON inspector pipe)
//! - `webdriver`: W3C Classic `WebDriver` over HTTP
//!
//! Uses enum dispatch (not trait objects) for zero-cost abstraction and `Clone` support.

mod any_browser;
mod any_element;
mod any_page;
pub mod bidi;
mod binding;
pub mod cdp;
mod media;
mod popups;
mod shared_page;
mod types;
pub mod w3c_input;
pub mod webdriver;
pub mod webkit;

pub use ferridriver_driver::common;
pub(crate) use ferridriver_driver::json_scan;
pub(crate) use ferridriver_driver::keyboard;
pub(crate) use ferridriver_driver::screencast;
pub(crate) use ferridriver_driver::transport_tasks;
pub(crate) use ferridriver_process::async_tempdir;
pub use ferridriver_process::process;
#[cfg(unix)]
pub use ferridriver_process::reaper;
#[cfg(windows)]
pub use ferridriver_process::windows_spawn;

pub use common::js_escape;
pub(crate) use shared_page::ErasedWeakPage;
pub use shared_page::SharedPageState;

pub use any_browser::AnyBrowser;
pub use any_element::AnyElement;
pub use any_page::{AnyPage, element_from_remote, element_handle_remote};
pub(crate) use binding::{PageBinding, binding_settle_js};
pub(crate) use media::{DEFAULT_COLOR_SCHEME, SharedMediaEmulation};
pub use popups::PopupInfo;
pub(crate) use popups::{BarrierPasses, CreateLedger, PopupClaim, PopupTaps, PopupWatch, PopupsInFlight, push_popup};
#[cfg(test)]
pub(crate) use types::EMPTY_PARAMS;
pub use types::{
  AxNodeData, AxProperty, BackendClickArgs, BackendHoverArgs, BackendKind, BackgroundColor, ClearCookieOptions,
  CompiledCookieFilter, CookieData, FrameInfo, ImageFormat, MetricData, NavLifecycle, PageBackref, Protocol, SameSite,
  ScreenshotCapture, ScreenshotScale, SetCookieParams,
};
pub(crate) use types::{deadline_for, empty_params, encode_webp_from_png, ensure_user_data_dir};

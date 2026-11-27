//! NAPI-RS native addon for ferridriver browser automation.
//!
//! Exposes the Playwright-compatible Browser/Page/Locator API to Node.js.

// Deep async futures holding cyclic `Browser` / `Emitter<BrowserEvent>` types
// can exceed rustc's default 128-step `Send` trait-solver recursion limit.
#![recursion_limit = "256"]

// Use mimalloc as the global allocator for better NAPI workload performance.
// Reduces fragmentation and improves throughput for the frequent small allocations
// that occur at the Rust/JS boundary (strings, options structs, callbacks).
#[cfg(not(target_os = "windows"))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod abort;
mod android;
mod browser;
mod browser_type;
mod cdp_session;
mod clock;
mod codegen;
mod console_message;
mod context;
mod coverage;
mod credentials;
mod devices;
mod dialog;
mod disposable;
mod download;
mod element_handle;
mod emitter;
mod error;
mod evaluate_arg;
mod exposed;
mod file_chooser;
mod frame;
mod frame_locator;
mod heap;
mod http_client;
mod identity;
mod install;
mod js_handle;
mod locator;
mod macros;
mod network;
mod page;
mod route;
mod route_registry;
mod selectors;
mod serialize_out;
mod tracing;
mod types;
mod video;
mod web_error;
mod web_mcp;
mod web_socket_route;
mod web_storage;
mod worker;

// napi-derive registers free-function exports from a constructor it
// compiles out under `cfg(test)`; exporting them from the crate root keeps
// the unit-test build from reading them as dead.
pub use android::{android_connect, android_devices, android_launch_server, android_set_default_timeout};
pub use browser_type::{chromium, firefox, ladybird, lightpanda, safari, webkit};
pub use http_client::new_context;
pub use install::{
  dump_rtt_stats, find_installed_chromium, find_installed_firefox, find_installed_headless_shell,
  get_browser_cache_dir, install_chromium, install_chromium_headless_shell, install_chromium_with_deps,
  install_firefox, install_system_deps,
};
pub use selectors::{register_selector_engine, set_test_id_attribute};

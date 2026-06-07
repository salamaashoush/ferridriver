//! Shared helpers for the `backends` integration test binary.
//!
//! Split out of the sprawling `tests/backends.rs` so new test groups
//! can live in dedicated files without duplicating the MCP-client and
//! payload-extraction plumbing. When you add a new group of tests,
//! create a new file here named by the behaviour it exercises (not by
//! session-local labels like phase / task / rule numbers) and add its
//! `pub mod` line below — `tests/backends.rs` will pick up the test
//! functions via the module path.

pub mod action_options;
pub mod bdd;
pub mod binding_surface;
pub mod browser_context_options;
pub mod browser_type;
pub mod client;
pub mod console_message;
pub mod dialog;
pub mod download;
pub mod evaluate;
pub mod expect;
pub mod file_chooser;
pub mod getby_regex;
pub mod handle_surface;
pub mod locator_handler;
pub mod multi_page;
pub mod nav;
pub mod navigation_response;
pub mod network;
pub mod observation;
pub mod script_emul_storage;
pub mod script_handles_local;
pub mod script_input;
pub mod script_locators;
pub mod script_sessions;
pub mod video;
pub mod web_error;

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
pub mod client;
pub mod handle_surface;

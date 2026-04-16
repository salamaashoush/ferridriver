//! NAPI-RS native addon for ferridriver browser automation.
//!
//! Exposes the Playwright-compatible Browser/Page/Locator API to Node.js.

#![allow(
  unsafe_code,
  clippy::needless_pass_by_value,
  clippy::missing_errors_doc,
  clippy::missing_panics_doc,
  clippy::must_use_candidate,
  clippy::doc_markdown,
  clippy::module_name_repetitions,
  clippy::cast_possible_truncation,
  clippy::cast_possible_wrap,
  clippy::cast_sign_loss,
  clippy::cast_precision_loss,
  clippy::cast_lossless,
  clippy::redundant_closure_for_method_calls,
  clippy::implicit_clone,
  clippy::too_many_lines,
  clippy::uninlined_format_args,
  clippy::single_char_pattern,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::map_unwrap_or,
  clippy::struct_excessive_bools,
  clippy::unnecessary_wraps,
  clippy::default_trait_access,
  clippy::bool_to_int_with_if,
  clippy::format_push_string,
  clippy::unused_async,
  clippy::unused_self,
  clippy::match_same_arms,
  clippy::items_after_statements,
  clippy::vec_init_then_push,
  clippy::iter_over_hash_type,
  clippy::semicolon_if_nothing_returned,
  clippy::option_map_or_none,
  clippy::single_match_else,
  clippy::manual_let_else
)]

//! NAPI-RS native addon for ferridriver browser automation.
//!
//! Exposes the Playwright-compatible Browser/Page/Locator API to Node.js.

// Use mimalloc as the global allocator for better NAPI workload performance.
// Reduces fragmentation and improves throughput for the frequent small allocations
// that occur at the Rust/JS boundary (strings, options structs, callbacks).
#[cfg(not(target_os = "windows"))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod api_request;
mod bdd_registry;
mod browser;
mod codegen;
mod context;
mod frame;
#[allow(dead_code)]
mod install;
mod locator;
mod page;
mod route;
mod step_handle;
mod test_fixtures;
mod test_info;
mod test_runner;
mod types;

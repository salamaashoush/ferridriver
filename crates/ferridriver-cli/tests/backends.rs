#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::single_char_pattern,
  clippy::cast_precision_loss,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::needless_pass_by_value,
  clippy::redundant_closure_for_method_calls,
  clippy::format_push_string,
  clippy::semicolon_if_nothing_returned
)]
//! Integration tests for ferridriver across all backends.
//!
//! Architecture: ONE browser per backend, ALL tests run sequentially on it.
//! This avoids spawning many browser processes per backend; each test navigates
//! to a fresh page so state doesn't leak.
//!
//! The MCP surface is scripting-focused: observation via `navigate` / `snapshot`
//! / `screenshot` / `evaluate` / `search_page` / `diagnostics` / `page`, and
//! action via `run_script` with `page` / `context` / `request` globals.
//!
//! NOTE for next sessions: **do not extend this file further**. The
//! shared MCP client and payload-extraction helpers live in
//! `backends_support::client`. When you add a new group of tests,
//! create a new file under `tests/backends_support/` named by the
//! behaviour it exercises (not by session-local labels like phase /
//! task / rule numbers) and register its functions in
//! `run_all_tests` below.

mod backends_support;

use backends_support::client::McpClient;

// ─── Run all tests on one client ────────────────────────────────────────────

/// Run a closure-supplied test list against a fresh `McpClient` for
/// `backend`. Each per-(backend, category) `#[test]` reaches here via
/// the `gen_backend_tests!` macro at the bottom of this file. The
/// shared browser model (one launch per `#[test]`) preserves the
/// original architecture's per-backend cost while letting nextest
/// distribute categories in parallel.
fn run_category(backend: &str, register: fn(&mut TestSet<'_>)) {
  let mut c = McpClient::new(backend);
  let mut passed = 0u32;
  let mut failures: Vec<String> = Vec::new();

  // Optional substring filter for interactive debugging. When
  // `FERRIDRIVER_TEST_FILTER` is set, only tests whose fully-qualified
  // function path contains the given substring run; the rest are
  // silently skipped. Lets developers re-run a single group without
  // editing the test harness.
  let filter = std::env::var("FERRIDRIVER_TEST_FILTER").ok();
  let verbose = std::env::var("FERRIDRIVER_TEST_VERBOSE").is_ok();

  let mut set = TestSet {
    backend,
    client: &mut c,
    passed: &mut passed,
    failures: &mut failures,
    filter: filter.as_deref(),
    verbose,
  };
  register(&mut set);

  eprintln!("\n{backend}: {passed} passed, {} failed", failures.len());
  if !failures.is_empty() {
    eprintln!("Failures: {}", failures.join(", "));
  }
  assert_eq!(
    failures.len(),
    0,
    "{backend}: {} test failures: {}",
    failures.len(),
    failures.join(", ")
  );
}

struct TestSet<'a> {
  backend: &'a str,
  client: &'a mut McpClient,
  passed: &'a mut u32,
  failures: &'a mut Vec<String>,
  filter: Option<&'a str>,
  verbose: bool,
}

impl TestSet<'_> {
  fn run(&mut self, name: &'static str, body: fn(&mut McpClient)) {
    if let Some(f) = self.filter
      && !name.contains(f)
    {
      return;
    }
    if self.verbose {
      eprintln!("=== RUN {} {}", self.backend, name);
    }
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(self.client))).is_ok() {
      *self.passed += 1;
    } else {
      self.failures.push(name.to_string());
      eprintln!("  FAIL {name}");
    }
  }
}

// Macro shorthand: `run!(set, test_fn)` registers a single test by
// stringifying its path. Replaces the old free `run!` macro inside
// `run_all_tests`.
macro_rules! run {
  ($set:ident, $name:path) => {
    $set.run(stringify!($name), $name)
  };
}

fn register_nav(set: &mut TestSet<'_>) {
  backends_support::nav::register(set);
}

fn register_evaluate(set: &mut TestSet<'_>) {
  backends_support::evaluate::register(set);
}

fn register_observation(set: &mut TestSet<'_>) {
  backends_support::observation::register(set);
}

fn register_script_input(set: &mut TestSet<'_>) {
  backends_support::script_input::register(set);
}

fn register_script_handles(set: &mut TestSet<'_>) {
  run!(set, backends_support::script_handles_local::test_script_click_at);
  run!(
    set,
    backends_support::script_handles_local::test_script_mouse_click_coords
  );
  run!(set, backends_support::script_handles_local::test_script_drag_coords);
  run!(set, backends_support::script_handles_local::test_script_drag_and_drop);
  run!(
    set,
    backends_support::script_handles_local::test_script_drag_and_drop_options
  );
  run!(
    set,
    backends_support::script_handles_local::test_script_locator_drag_to_options
  );
  run!(
    set,
    backends_support::script_handles_local::test_script_drag_and_drop_trial
  );
  run!(
    set,
    backends_support::script_handles_local::test_script_locator_drop_payload
  );
  run!(
    set,
    backends_support::script_handles_local::test_script_locator_drop_rejected
  );
  run!(
    set,
    backends_support::script_handles_local::test_script_emulate_media_all_fields
  );
  run!(
    set,
    backends_support::script_handles_local::test_script_emulate_media_null_disables_single_field
  );
  run!(set, backends_support::script_handles_local::test_script_add_init_script);
  run!(
    set,
    backends_support::script_handles_local::test_script_utility_script_exposed
  );
  run!(
    set,
    backends_support::script_handles_local::test_script_handle_lifecycle
  );
  run!(
    set,
    backends_support::script_handles_local::test_script_evaluate_fn_and_handle
  );
  run!(
    set,
    backends_support::script_handles_local::test_script_evaluate_rich_types
  );
  run!(
    set,
    backends_support::script_handles_local::test_script_element_handle_methods
  );
  run!(
    set,
    backends_support::script_handles_local::test_script_handle_materialisation
  );
  run!(set, backends_support::handle_surface::test_handle_json_value);
  run!(set, backends_support::handle_surface::test_handle_properties);
  run!(set, backends_support::handle_surface::test_handle_multi_arg_evaluate);
  run!(set, backends_support::handle_surface::test_element_handle_eval);
  run!(set, backends_support::handle_surface::test_element_handle_query);
  run!(set, backends_support::handle_surface::test_element_handle_frames);
  run!(set, backends_support::handle_surface::test_element_handle_waits);
  run!(
    set,
    backends_support::handle_surface::test_element_handle_temp_tag_actions
  );
  run!(
    set,
    backends_support::handle_surface::test_element_handle_action_options
  );
  run!(set, backends_support::handle_surface::test_element_handle_select_text);
  run!(set, backends_support::script_handles_local::test_script_click_options);
  run!(set, backends_support::action_options::test_script_dblclick_options);
  run!(set, backends_support::action_options::test_script_press_options);
  run!(set, backends_support::action_options::test_script_type_options);
  run!(
    set,
    backends_support::action_options::test_script_set_input_files_polymorphism
  );
  run!(set, backends_support::script_handles_local::test_script_action_timeout);
  run!(set, backends_support::script_handles_local::test_script_tap_native);
  run!(set, backends_support::script_handles_local::test_script_fill_force);
  run!(set, backends_support::script_handles_local::test_script_check_behavior);
  run!(
    set,
    backends_support::script_handles_local::test_script_dispatch_event_timeout
  );
  run!(
    set,
    backends_support::script_handles_local::test_script_select_option_force
  );
  run!(set, backends_support::script_handles_local::test_script_mouse_wheel);
  run!(set, backends_support::script_handles_local::test_script_keyboard_press);
  run!(
    set,
    backends_support::script_handles_local::test_script_screenshot_mask_locator
  );
  run!(
    set,
    backends_support::script_handles_local::test_script_keyboard_type_named_keys
  );
}

fn register_script_locators(set: &mut TestSet<'_>) {
  backends_support::script_locators::register(set);
}

fn register_script_emulation_storage(set: &mut TestSet<'_>) {
  backends_support::script_emul_storage::register(set);
}

fn register_script_sessions(set: &mut TestSet<'_>) {
  backends_support::script_sessions::register(set);
}

fn register_events_network(set: &mut TestSet<'_>) {
  run!(set, backends_support::network::test_network_redirect_chain);
  run!(set, backends_support::network::test_network_request_failure);
  run!(set, backends_support::network::test_route_disposable);
  run!(set, backends_support::network::test_network_response_body);
  run!(set, backends_support::network::test_network_post_data);
  run!(set, backends_support::network::test_network_post_data_buffer);
  run!(set, backends_support::network::test_network_headers);
  run!(set, backends_support::network::test_network_http_version);
  run!(set, backends_support::network::test_network_websocket);
  run!(set, backends_support::network::test_route_fallback_applies_overrides);
  run!(set, backends_support::navigation_response::test_goto_returns_response);
  run!(set, backends_support::navigation_response::test_goto_follows_redirects);
  run!(set, backends_support::navigation_response::test_goto_network_failure);
  run!(set, backends_support::navigation_response::test_reload_returns_response);
  run!(
    set,
    backends_support::navigation_response::test_history_traversal_returns_response
  );
}

fn register_events_dialog_files(set: &mut TestSet<'_>) {
  run!(set, backends_support::dialog::test_dialog_accept_confirm);
  run!(set, backends_support::dialog::test_dialog_dismiss_confirm);
  run!(set, backends_support::dialog::test_dialog_prompt_with_text);
  run!(set, backends_support::dialog::test_dialog_double_accept_rejects);
  run!(set, backends_support::dialog::test_dialog_auto_dismiss_without_listener);
  run!(set, backends_support::dialog::test_dialog_page_accessor);
  run!(
    set,
    backends_support::file_chooser::test_file_chooser_single_string_path
  );
  run!(
    set,
    backends_support::file_chooser::test_file_chooser_multiple_string_array
  );
  run!(
    set,
    backends_support::file_chooser::test_file_chooser_file_payload_single
  );
  run!(
    set,
    backends_support::file_chooser::test_file_chooser_unclaimed_disposes
  );
  run!(set, backends_support::download::test_download_save_as_roundtrip);
  run!(set, backends_support::download::test_download_path_contents);
  run!(set, backends_support::download::test_download_cancel_surfaces_failure);
  run!(set, backends_support::download::test_download_cancel_bidi_unsupported);
}

fn register_events_metadata(set: &mut TestSet<'_>) {
  run!(set, backends_support::console_message::test_console_message_primitives);
  run!(
    set,
    backends_support::console_message::test_console_message_warn_maps_to_warning
  );
  run!(set, backends_support::console_message::test_console_message_error_type);
  run!(
    set,
    backends_support::console_message::test_console_message_location_shape
  );
  run!(set, backends_support::web_error::test_page_error_is_native_error);
  run!(
    set,
    backends_support::web_error::test_context_weberror_is_webbed_error_class
  );
  run!(set, backends_support::video::test_video_null_without_recording);
  run!(set, backends_support::video::test_video_recording_lifecycle);
}

fn register_context_options(set: &mut TestSet<'_>) {
  run!(
    set,
    backends_support::browser_context_options::test_context_options_user_agent
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_locale
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_timezone
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_color_scheme
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_reduced_motion
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_forced_colors
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_viewport
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_javascript_enabled
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_geolocation
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_extra_http_headers
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_offline
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_device_scale_factor
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_has_touch
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_set_http_credentials
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_set_default_timeout
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_is_closed_and_browser
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_route_and_unroute
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_service_workers_block
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_screen
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_bypass_csp
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_base_url
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_storage_state
  );
  run!(
    set,
    backends_support::browser_context_options::test_context_options_proxy
  );
}

fn register_browser_type(set: &mut TestSet<'_>) {
  run!(set, backends_support::browser_type::test_browser_type_name);
  run!(set, backends_support::browser_type::test_browser_type_executable_path);
  run!(set, backends_support::browser_type::test_browser_type_chromium_launch);
  run!(
    set,
    backends_support::browser_type::test_browser_type_chromium_transport_ws
  );
  run!(
    set,
    backends_support::browser_type::test_browser_type_connect_over_cdp_chromium_only
  );
  run!(
    set,
    backends_support::browser_type::test_browser_type_launch_persistent_context
  );
}

fn register_expect(set: &mut TestSet<'_>) {
  run!(set, backends_support::expect::test_expect_to_be_visible);
  run!(set, backends_support::expect::test_expect_to_have_text);
  run!(set, backends_support::expect::test_expect_to_contain_text);
  run!(set, backends_support::expect::test_expect_to_have_count);
  run!(set, backends_support::expect::test_expect_to_have_attribute);
  run!(set, backends_support::expect::test_expect_to_have_value);
  run!(set, backends_support::expect::test_expect_page_title_and_url);
  run!(set, backends_support::expect::test_expect_value_matchers_in_script);
  run!(set, backends_support::expect::test_expect_to_throw_in_script);
  run!(set, backends_support::expect::test_expect_failure_throws);
  run!(set, backends_support::expect::test_expect_poll_with_browser);
}

fn register_binding_surface(set: &mut TestSet<'_>) {
  run!(set, backends_support::binding_surface::test_frame_get_by_methods);
  run!(
    set,
    backends_support::binding_surface::test_frame_page_and_frame_locator
  );
  run!(set, backends_support::binding_surface::test_locator_get_by_methods);
  run!(
    set,
    backends_support::binding_surface::test_locator_page_and_frame_methods
  );
  run!(set, backends_support::binding_surface::test_frame_locator_class);
  run!(set, backends_support::binding_surface::test_page_frame_locator);
  run!(set, backends_support::binding_surface::test_page_touchscreen_tap);
  run!(set, backends_support::binding_surface::test_page_snapshot_for_ai);
  run!(set, backends_support::binding_surface::test_page_expose_function);
  run!(set, backends_support::binding_surface::test_context_expose_binding);
  run!(set, backends_support::binding_surface::test_context_expose_function);
  run!(
    set,
    backends_support::binding_surface::test_context_clear_cookies_filter
  );
}

fn register_getby_regex(set: &mut TestSet<'_>) {
  run!(set, backends_support::getby_regex::test_getby_text_regex);
  run!(set, backends_support::getby_regex::test_getby_role_name_regex);
  run!(set, backends_support::getby_regex::test_getby_placeholder_regex);
  run!(set, backends_support::getby_regex::test_getby_test_id_regex);
}

fn register_multi_page(set: &mut TestSet<'_>) {
  backends_support::multi_page::register(set);
}

// ─── Per-(backend, category) #[test] entry points ──────────────────────────
//
// 17 categories × 4 backends = 68 `#[test]`s grouped into one module
// per backend. nextest reports them as
// `backends::<backend>::<category>` and distributes them across cores.
// A single failing category fails its own test, not the entire backend.

macro_rules! backend_module {
  ($module:ident, $backend:literal) => {
    mod $module {
      use super::*;

      #[test]
      fn nav() {
        run_category($backend, register_nav);
      }
      #[test]
      fn evaluate() {
        run_category($backend, register_evaluate);
      }
      #[test]
      fn observation() {
        run_category($backend, register_observation);
      }
      #[test]
      fn script_input() {
        run_category($backend, register_script_input);
      }
      #[test]
      fn script_handles() {
        run_category($backend, register_script_handles);
      }
      #[test]
      fn script_locators() {
        run_category($backend, register_script_locators);
      }
      #[test]
      fn script_emulation_storage() {
        run_category($backend, register_script_emulation_storage);
      }
      #[test]
      fn script_sessions() {
        run_category($backend, register_script_sessions);
      }
      #[test]
      fn events_network() {
        run_category($backend, register_events_network);
      }
      #[test]
      fn events_dialog_files() {
        run_category($backend, register_events_dialog_files);
      }
      #[test]
      fn events_metadata() {
        run_category($backend, register_events_metadata);
      }
      #[test]
      fn context_options() {
        run_category($backend, register_context_options);
      }
      #[test]
      fn browser_type() {
        run_category($backend, register_browser_type);
      }
      #[test]
      fn expect() {
        run_category($backend, register_expect);
      }
      #[test]
      fn binding_surface() {
        run_category($backend, register_binding_surface);
      }
      #[test]
      fn getby_regex() {
        run_category($backend, register_getby_regex);
      }
      #[test]
      fn multi_page() {
        run_category($backend, register_multi_page);
      }
    }
  };
}

backend_module!(cdp_pipe, "cdp-pipe");
backend_module!(cdp_raw, "cdp-raw");
backend_module!(webkit, "webkit");
backend_module!(bidi, "bidi");

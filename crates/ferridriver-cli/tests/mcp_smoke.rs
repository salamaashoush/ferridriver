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
//! MCP smoke suite: the retained per-backend coverage of the MCP wire
//! itself, after the browser-behaviour e2e suite moved to
//! `tests/e2e/*.test.ts` (run by `ferridriver test`).
//!
//! What stays here and why:
//! - tool dispatch (`navigate` / `evaluate` / `snapshot` / `screenshot`
//!   / `search_page` / `diagnostics` / `page`) — MCP-session semantics
//!   with no runner analogue;
//! - `run_script` session state (`backends_support::script_sessions`) —
//!   vars/globalThis persistence + poison-timeout VM recovery;
//! - the zip validators (`backends_support::{tracing_har,trace}`) —
//!   their payload entries are DEFLATE-compressed and the QuickJS
//!   sandbox has no inflater, so content-level assertions live here;
//! - multi-page, session binding, the `run_bdd` tool, extension tools,
//!   and rmcp protocol features.
//!
//! Architecture: ONE browser per (backend, category), tests sequential
//! on it; each test navigates to a fresh page so state doesn't leak.
//! Browser-behaviour coverage belongs in `tests/e2e/` — add here only
//! when the behaviour is MCP-wire-specific.

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

fn register_nav(set: &mut TestSet<'_>) {
  backends_support::nav::register(set);
}

fn register_evaluate(set: &mut TestSet<'_>) {
  backends_support::evaluate::register(set);
}

fn register_observation(set: &mut TestSet<'_>) {
  backends_support::observation::register(set);
}

fn register_script_sessions(set: &mut TestSet<'_>) {
  backends_support::script_sessions::register(set);
}

fn register_events_metadata(set: &mut TestSet<'_>) {
  backends_support::tracing_har::register(set);
  backends_support::trace::register(set);
}

fn register_multi_page(set: &mut TestSet<'_>) {
  backends_support::multi_page::register(set);
}

fn register_session_bind(set: &mut TestSet<'_>) {
  backends_support::session_bind::register(set);
}

fn register_mcp_features(set: &mut TestSet<'_>) {
  backends_support::mcp_features::register(set);
  backends_support::error_convention::register(set);
}

// ─── Per-(backend, category) #[test] entry points ──────────────────────────
//
// 7 categories × 4 backends = 28 `#[test]`s grouped into one module
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
      fn script_sessions() {
        run_category($backend, register_script_sessions);
      }
      #[test]
      fn events_metadata() {
        run_category($backend, register_events_metadata);
      }
      #[test]
      fn multi_page() {
        run_category($backend, register_multi_page);
      }
      #[test]
      fn session_bind() {
        run_category($backend, register_session_bind);
      }
    }
  };
}

backend_module!(cdp_pipe, "cdp-pipe");
backend_module!(cdp_raw, "cdp-raw");
backend_module!(webkit, "webkit");
backend_module!(bidi, "bidi");

// `run_bdd` runs on the live MCP session (the same browser the client
// drives), reusing the BDD step engine. It is session-driven, not
// backend-specific, so it runs once rather than per-backend.
#[test]
fn run_bdd_tool() {
  let mut c = McpClient::new("cdp-pipe");
  backends_support::bdd::run(&mut c);
}

// Extension system at the MCP boundary: promoted-tool metadata /
// schema contracts on the wire, plus the page-callback half of the
// capability-follows-registrar invariant. VM-side behaviour, not
// backend protocol behaviour (and `startScreencast` is CDP-only), so
// it runs once on cdp-pipe with its own config-loaded server.
#[test]
fn extension_tools() {
  backends_support::extension_tools::run();
}

#[test]
fn extension_context() {
  backends_support::extension_context::run();
}

// Extension PACKAGES: multi-entry `ferridriver.entries` resolution and the
// `requires` / `settings` gate. Config + loader behaviour, so one backend.
#[test]
fn extension_package() {
  backends_support::extension_package::run();
}

#[test]
fn instances() {
  backends_support::instances::run();
}

// rmcp-2.x server features (tool annotations/titles, artifact:// resource
// links, progress notifications) are protocol-level, not backend-specific,
// so they run once on cdp-pipe.
#[test]
fn mcp_features() {
  run_category("cdp-pipe", register_mcp_features);
}

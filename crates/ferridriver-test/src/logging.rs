//! Centralized tracing/logging initialization.
//!
//! Call `ferridriver_test::logging::init()` once at startup. It's safe to call
//! multiple times — subsequent calls are no-ops.
//!
//! Respects (in priority order):
//! 1. `RUST_LOG` — standard tracing env filter
//! 2. `FERRIDRIVER_DEBUG` — category-based filter
//! 3. `verbose` parameter — 0=warn, 1=debug, 2+=trace
//!
//! ## `FERRIDRIVER_DEBUG` categories
//!
//! | Value | Tracing target |
//! |-------|---------------|
//! | `*` / `all` | `ferridriver=trace` |
//! | `cdp` | `ferridriver::cdp=trace` |
//! | `step` / `steps` | `ferridriver::bdd::step=trace` |
//! | `hook` / `hooks` | `ferridriver::bdd::hook=trace` |
//! | `worker` | `ferridriver::worker=trace` |
//! | `fixture` | `ferridriver::fixture=trace` |
//! | `reporter` | `ferridriver::reporter=trace` |
//! | `action` | `ferridriver::action=trace` |
//! | `runner` | `ferridriver::runner=trace` |

use std::sync::Once;
use tracing_subscriber::EnvFilter;

static INIT: Once = Once::new();

/// Initialize the tracing subscriber. Safe to call multiple times.
///
/// `verbose`: 0 = warn (default), 1 = debug, 2+ = trace.
/// Overridden by `RUST_LOG` or `FERRIDRIVER_DEBUG` env vars.
pub fn init(verbose: u8) {
  INIT.call_once(|| {
    let filter = build_filter(verbose);
    let use_ansi = std::io::IsTerminal::is_terminal(&std::io::stderr());
    tracing_subscriber::fmt()
      .with_env_filter(filter)
      .with_writer(std::io::stderr)
      .with_ansi(use_ansi)
      .init();
  });
}

/// Initialize with env-var-only detection (no verbose flag).
/// Used by standalone harnesses and NAPI where there's no CLI verbose flag.
pub fn init_from_env() {
  if std::env::var("FERRIDRIVER_DEBUG").is_ok() || std::env::var("RUST_LOG").is_ok() {
    init(0);
  }
}

/// Build a tracing `EnvFilter` from verbose level and env vars.
fn build_filter(verbose: u8) -> EnvFilter {
  // RUST_LOG takes top priority.
  if std::env::var("RUST_LOG").is_ok() {
    return EnvFilter::from_default_env();
  }

  // FERRIDRIVER_DEBUG category-based filter.
  if let Ok(debug_val) = std::env::var("FERRIDRIVER_DEBUG") {
    return parse_debug_categories(&debug_val);
  }

  // --verbose flag.
  match verbose {
    0 => EnvFilter::new("warn"),
    1 => EnvFilter::new("warn,ferridriver=debug"),
    _ => EnvFilter::new("warn,ferridriver=trace"),
  }
}

/// Parse `FERRIDRIVER_DEBUG` value into an `EnvFilter`.
fn parse_debug_categories(debug_val: &str) -> EnvFilter {
  let mut filter = EnvFilter::new("warn");
  for category in debug_val.split(',').map(str::trim) {
    let directive = match category {
      "*" | "all" => "ferridriver=trace",
      "cdp" => "ferridriver::cdp=trace",
      "step" | "steps" => "ferridriver::bdd::step=trace",
      "hook" | "hooks" => "ferridriver::bdd::hook=trace",
      "worker" => "ferridriver::worker=trace",
      "fixture" => "ferridriver::fixture=trace",
      "reporter" => "ferridriver::reporter=trace",
      "action" => "ferridriver::action=trace",
      "runner" => "ferridriver::runner=trace",
      other => {
        // Allow arbitrary target names.
        let owned = format!("{other}=trace");
        filter = filter.add_directive(owned.parse().unwrap_or_else(|_| "warn".parse().unwrap()));
        continue;
      }
    };
    filter = filter.add_directive(directive.parse().unwrap_or_else(|_| "warn".parse().unwrap()));
  }
  filter
}

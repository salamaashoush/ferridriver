//! Throttling presets.
//!
//! From devtools-frontend `lantern/simulation/Constants.ts`. The two
//! adjustment factors exist because `DevTools` applies throttling
//! differently from the simulator, and a value passed through one has to
//! be converted before the other can use it.

/// `Simulator`'s `DEFAULT_LAYOUT_TASK_MULTIPLIER`.
pub const DEFAULT_LAYOUT_TASK_MULTIPLIER: f64 = 0.5;

pub const DEVTOOLS_RTT_ADJUSTMENT_FACTOR: f64 = 3.75;
pub const DEVTOOLS_THROUGHPUT_ADJUSTMENT_FACTOR: f64 = 0.9;

/// One network profile.
#[derive(Debug, Clone, Copy)]
pub struct Throttling {
  /// Round-trip time in milliseconds.
  pub rtt_ms: f64,
  /// Bits per second.
  pub throughput_bps: f64,
  /// How much slower the CPU is assumed to be than the machine that
  /// recorded the trace.
  pub cpu_slowdown_multiplier: f64,
  /// Applied on top of the CPU slowdown for a task that laid out.
  /// Layout is less CPU-bound than script, so it does not slow down as
  /// far; the simulator multiplies the two rather than choosing between
  /// them.
  pub layout_task_multiplier: f64,
}

impl Throttling {
  /// The conditions the trace itself recorded.
  ///
  /// This is what `DevTools` simulates its insights under
  /// (`throttlingMethod: 'provided'`), so a saving comes out in the
  /// milliseconds the page's own visitor would have seen rather than a
  /// hypothetical mobile connection's. Neither multiplier applies: the
  /// CPU being modelled is the one that produced the trace.
  #[must_use]
  pub fn observed(rtt_ms: f64, throughput_bps: f64) -> Self {
    Self {
      rtt_ms,
      throughput_bps,
      cpu_slowdown_multiplier: 1.0,
      layout_task_multiplier: 1.0,
    }
  }
}

/// Lighthouse's default. Aligns with `WebPageTest`'s "Fast 3G", and sits
/// around the 75th percentile of real 4G connections.
pub const MOBILE_SLOW_4G: Throttling = Throttling {
  rtt_ms: 150.0,
  throughput_bps: 1.6 * 1024.0 * 1024.0,
  cpu_slowdown_multiplier: 4.0,
  layout_task_multiplier: DEFAULT_LAYOUT_TASK_MULTIPLIER,
};

/// Roughly Chrome UX Report's 3G definition.
pub const MOBILE_REGULAR_3G: Throttling = Throttling {
  rtt_ms: 300.0,
  throughput_bps: 700.0 * 1024.0,
  cpu_slowdown_multiplier: 4.0,
  layout_task_multiplier: DEFAULT_LAYOUT_TASK_MULTIPLIER,
};

/// A broadband desktop connection; no CPU slowdown.
pub const DESKTOP_DENSE_4G: Throttling = Throttling {
  rtt_ms: 40.0,
  throughput_bps: 10.0 * 1024.0 * 1024.0,
  cpu_slowdown_multiplier: 1.0,
  layout_task_multiplier: DEFAULT_LAYOUT_TASK_MULTIPLIER,
};

//! Throttling presets.
//!
//! From devtools-frontend `lantern/simulation/Constants.ts`. The two
//! adjustment factors exist because `DevTools` applies throttling
//! differently from the simulator, and a value passed through one has to
//! be converted before the other can use it.

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
}

/// Lighthouse's default. Aligns with `WebPageTest`'s "Fast 3G", and sits
/// around the 75th percentile of real 4G connections.
pub const MOBILE_SLOW_4G: Throttling = Throttling {
  rtt_ms: 150.0,
  throughput_bps: 1.6 * 1024.0 * 1024.0,
  cpu_slowdown_multiplier: 4.0,
};

/// Roughly Chrome UX Report's 3G definition.
pub const MOBILE_REGULAR_3G: Throttling = Throttling {
  rtt_ms: 300.0,
  throughput_bps: 700.0 * 1024.0,
  cpu_slowdown_multiplier: 4.0,
};

/// A broadband desktop connection; no CPU slowdown.
pub const DESKTOP_DENSE_4G: Throttling = Throttling {
  rtt_ms: 40.0,
  throughput_bps: 10.0 * 1024.0 * 1024.0,
  cpu_slowdown_multiplier: 1.0,
};

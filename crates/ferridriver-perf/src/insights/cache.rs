//! Static assets served with a cache lifetime too short to be worth it.
//!
//! Mirrors devtools-frontend `insights/Cache.ts`.

use crate::handlers::network::NetworkRequest;
use crate::insights::{Check, Insight, Item, Severity};
use crate::units::count_to_f64;

/// Above this hit probability the lifetime is already long enough that
/// lengthening it buys nothing.
const IGNORE_THRESHOLD_IN_PERCENT: f64 = 0.925;

/// Only these statuses describe a body worth storing.
const CACHEABLE_STATUS_CODES: [i64; 3] = [200, 203, 206];

/// Resource types whose bytes do not change between visits.
const STATIC_RESOURCE_TYPES: [&str; 5] = ["Font", "Image", "Media", "Script", "Stylesheet"];

/// Schemes that never touch the network, so caching is meaningless.
const NON_NETWORK_SCHEMES: [&str; 6] = ["blob", "data", "intent", "file", "filesystem", "chrome-extension"];

const SECONDS_PER_DAY: f64 = 86_400.0;

#[must_use]
pub fn run(requests: &[NetworkRequest], lantern: Option<&crate::lantern::Context>) -> Insight {
  let mut items = Vec::new();
  let mut wasted_total = 0.0;

  for request in requests {
    if request.response_headers.is_empty() || !is_cacheable(request) {
      continue;
    }
    let cache_control = request.header("cache-control").unwrap_or_default();
    let directives = parse_cache_control(cache_control);

    // A response that deliberately opts out is not a finding.
    if caching_disabled(request, &directives) {
      continue;
    }

    let ttl = match cache_lifetime_seconds(request, &directives) {
      // A present but non-positive lifetime is an explicit "do not
      // store", same as opting out.
      Some(t) if t <= 0.0 || !t.is_finite() => continue,
      Some(t) => t,
      None => 0.0,
    };
    // A month or more is already as good as permanent.
    if ttl / SECONDS_PER_DAY >= 30.0 {
      continue;
    }
    let hit_probability = cache_hit_probability(ttl);
    if hit_probability > IGNORE_THRESHOLD_IN_PERCENT {
      continue;
    }

    let wasted = (1.0 - hit_probability) * count_to_f64(request.encoded_data_length);
    wasted_total += wasted;
    items.push(Item {
      label: request.url.clone(),
      value: wasted,
      unit: "bytes",
    });
  }

  items.sort_by(|a, b| b.value.total_cmp(&a.value));
  let passed = items.is_empty();
  let wasted_by_url: rustc_hash::FxHashMap<&str, f64> =
    items.iter().map(|item| (item.label.as_str(), item.value)).collect();
  let mut metrics = vec![("wastedBytes".into(), wasted_total)];
  crate::insights::push_byte_savings(&mut metrics, lantern, &wasted_by_url);

  Insight {
    key: "Cache".into(),
    title: "Use efficient cache lifetimes".into(),
    description: "A long cache lifetime can speed up repeat visits to your page.".into(),
    severity: if passed { Severity::Pass } else { Severity::Fail },
    checks: vec![Check {
      name: "usesLongCacheLifetimes".into(),
      passed,
      detail: if passed {
        "All static assets have an efficient cache lifetime".into()
      } else {
        format!(
          "{} static assets could be cached longer (~{:.0} KB re-fetched)",
          items.len(),
          wasted_total / 1024.0
        )
      },
    }],
    metrics,
    items,
  }
}

fn is_cacheable(request: &NetworkRequest) -> bool {
  if NON_NETWORK_SCHEMES.contains(&request.protocol.as_str()) {
    return false;
  }
  CACHEABLE_STATUS_CODES.contains(&request.status_code)
    && STATIC_RESOURCE_TYPES.contains(&request.resource_type.as_str())
}

/// `no-store`, `no-cache` and `must-revalidate` all mean the response is
/// not meant to be reused, and a `pragma: no-cache` does the same on
/// HTTP/1.0 responses that set no `cache-control` at all.
fn caching_disabled(request: &NetworkRequest, directives: &CacheControl) -> bool {
  if request.header("cache-control").is_none()
    && request
      .header("pragma")
      .is_some_and(|p| p.to_ascii_lowercase().contains("no-cache"))
  {
    return true;
  }
  directives.forbids_reuse
}

/// `max-age` when present, otherwise whatever `expires` leaves.
///
/// An unparseable `expires` counts as already expired, which is how a
/// browser treats it.
fn cache_lifetime_seconds(request: &NetworkRequest, directives: &CacheControl) -> Option<f64> {
  if let Some(max_age) = directives.max_age {
    return Some(max_age);
  }
  request.header("expires").map(|_| 0.0)
}

#[derive(Default)]
struct CacheControl {
  max_age: Option<f64>,
  /// Any directive meaning "do not reuse this without asking again".
  /// `no-store`, `no-cache`, `must-revalidate` and `private` are four
  /// spellings of the same answer for this insight's purposes, so they
  /// collapse rather than sitting as four independent flags.
  forbids_reuse: bool,
}

fn parse_cache_control(value: &str) -> CacheControl {
  let mut out = CacheControl::default();
  for directive in value.split(',') {
    let directive = directive.trim().to_ascii_lowercase();
    let (name, argument) = match directive.split_once('=') {
      Some((n, a)) => (n.trim(), Some(a.trim())),
      None => (directive.as_str(), None),
    };
    match name {
      "max-age" => out.max_age = argument.and_then(|a| a.trim_matches('"').parse().ok()),
      "no-store" | "no-cache" | "must-revalidate" | "private" => out.forbids_reuse = true,
      _ => {},
    }
  }
  out
}

/// How likely a repeat visit still finds the asset in cache.
///
/// The deciles are the hand-drawn distribution `DevTools` carries, from
/// 2017 UMA data on `HttpCache.StaleEntry.Validated.Age`. They exist
/// because cache lifetime has sharply diminishing returns: six months
/// is not twice as good as three.
fn cache_hit_probability(max_age_seconds: f64) -> f64 {
  const AGE_IN_HOURS_DECILES: [f64; 12] = [
    0.0,
    0.2,
    1.0,
    3.0,
    8.0,
    12.0,
    24.0,
    48.0,
    72.0,
    168.0,
    8760.0,
    f64::INFINITY,
  ];

  let max_age_hours = max_age_seconds / 3600.0;
  let Some(upper) = AGE_IN_HOURS_DECILES.iter().position(|d| *d >= max_age_hours) else {
    return 1.0;
  };
  if upper == AGE_IN_HOURS_DECILES.len() - 1 {
    return 1.0;
  }
  if upper == 0 {
    return 0.0;
  }

  let upper_value = AGE_IN_HOURS_DECILES[upper];
  let lower_value = AGE_IN_HOURS_DECILES[upper - 1];
  #[expect(clippy::cast_precision_loss, reason = "index is at most 11")]
  let upper_decile = upper as f64 / 10.0;
  #[expect(clippy::cast_precision_loss, reason = "index is at most 11")]
  let lower_decile = (upper - 1) as f64 / 10.0;

  linear_interpolation(lower_value, lower_decile, upper_value, upper_decile, max_age_hours)
}

fn linear_interpolation(x0: f64, y0: f64, x1: f64, y1: f64, x: f64) -> f64 {
  if (x1 - x0).abs() < f64::EPSILON {
    return y0;
  }
  y0 + (x - x0) * (y1 - y0) / (x1 - x0)
}

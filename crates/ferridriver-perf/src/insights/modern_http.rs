//! Origins still served over HTTP/1.
//!
//! Mirrors `devtools-frontend` `insights/ModernHTTP.ts`.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::handlers::network::NetworkRequest;
use crate::insights::{Check, Insight, Item, Severity};

/// An origin serving fewer than this over HTTP/1 is not worth flagging:
/// multiplexing wins only show up once a host is carrying real traffic.
const MIN_REQUESTS_PER_ORIGIN: usize = 6;

#[must_use]
pub fn run(requests: &[NetworkRequest]) -> Insight {
  let mut by_origin: FxHashMap<String, Vec<&NetworkRequest>> = FxHashMap::default();
  let mut seen_urls: FxHashSet<&str> = FxHashSet::default();

  for request in requests {
    if !seen_urls.insert(request.url.as_str()) {
      continue;
    }
    // Service workers report http/1.1 regardless of the real transport,
    // which used to produce false positives (Lighthouse #7158).
    if request.flags.from_service_worker || !is_old_http(&request.protocol) {
      continue;
    }
    if let Some(origin) = origin_of(&request.url) {
      by_origin.entry(origin).or_default().push(request);
    }
  }

  let mut items: Vec<Item> = by_origin
    .into_iter()
    .filter(|(_, reqs)| reqs.len() >= MIN_REQUESTS_PER_ORIGIN)
    .map(|(origin, reqs)| Item {
      label: origin,
      value: crate::units::len_to_f64(reqs.len()),
      unit: "requests",
    })
    .collect();
  items.sort_by(|a, b| b.value.total_cmp(&a.value));

  let passed = items.is_empty();
  Insight {
    key: "ModernHTTP".into(),
    title: "Modern HTTP".into(),
    description: "HTTP/2 and HTTP/3 multiplex requests over one connection. Origins still on HTTP/1 pay a \
                  connection setup per request and are limited by per-host connection caps."
      .into(),
    severity: if passed { Severity::Pass } else { Severity::Fail },
    checks: vec![Check {
      name: "usesModernHTTP".into(),
      passed,
      detail: if passed {
        "All origins use HTTP/2 or better".into()
      } else {
        format!("{} origins served over HTTP/1", items.len())
      },
    }],
    metrics: vec![("legacyOrigins".into(), crate::units::len_to_f64(items.len()))],
    items,
  }
}

/// `http/0.9`, `http/1.0` and `http/1.1`, matching `DevTools`'
/// `/HTTP\/[01][.\d]?/i`. Note this is the negotiated protocol, not the
/// URL scheme.
fn is_old_http(protocol: &str) -> bool {
  let p = protocol.to_ascii_lowercase();
  p.starts_with("http/0") || p.starts_with("http/1")
}

/// `scheme://host[:port]`, without pulling in a URL parser for what is
/// a prefix scan.
fn origin_of(url: &str) -> Option<String> {
  let (scheme, rest) = url.split_once("://")?;
  let authority = rest.split(['/', '?', '#']).next()?;
  if authority.is_empty() {
    return None;
  }
  Some(format!("{scheme}://{authority}"))
}

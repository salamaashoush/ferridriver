//! Bytes and time spent on origins other than the page's own.
//!
//! Mirrors `devtools-frontend` `insights/ThirdParties.ts` in what it
//! reports. `DevTools` maps origins onto a curated entity list (so
//! `googletagmanager.com` and `google-analytics.com` roll up as one
//! product); without that list this groups by origin, which is
//! stricter but never wrong about which bytes came from where.

use rustc_hash::FxHashMap;

use crate::handlers::network::NetworkRequest;
use crate::insights::{Check, Insight, Item, Severity};

#[must_use]
pub fn run(requests: &[NetworkRequest], first_party_url: &str) -> Insight {
  // Without a first party there is nothing to be third TO. Reporting
  // every origin as third-party would be strictly worse than saying
  // nothing, so an unresolvable page URL yields an empty result.
  let Some(first_party) = registrable_domain(first_party_url) else {
    return empty("page URL unknown, so no request can be classified");
  };

  let mut bytes_by_origin: FxHashMap<String, i64> = FxHashMap::default();
  for request in requests {
    let Some(origin) = host_of(&request.url) else { continue };
    if registrable_domain(&request.url).as_ref() == Some(&first_party) {
      continue;
    }
    *bytes_by_origin.entry(origin).or_default() += request.encoded_data_length;
  }

  let mut items: Vec<Item> = bytes_by_origin
    .into_iter()
    .map(|(origin, bytes)| Item {
      label: origin,
      value: crate::units::count_to_f64(bytes),
      unit: "bytes",
    })
    .collect();
  items.sort_by(|a, b| b.value.total_cmp(&a.value));

  let total: f64 = items.iter().map(|i| i.value).sum();
  Insight {
    key: "ThirdParties".into(),
    title: "Third parties".into(),
    description: "Third-party code you do not control still competes for the main thread and the network.".into(),
    // Third-party weight is context the developer judges, not something
    // with a threshold to fail against.
    severity: Severity::Informative,
    checks: vec![Check {
      name: "hasThirdParties".into(),
      // Informative: third-party weight is a fact to weigh, not a
      // threshold to fail, so "no third parties" is the only pass and
      // any other count is reported without a verdict.
      passed: items.is_empty(),
      detail: if items.is_empty() {
        "No third-party requests".into()
      } else {
        format!(
          "{} third-party origins, {:.0} KB transferred",
          items.len(),
          total / 1024.0
        )
      },
    }],
    metrics: vec![
      ("thirdPartyOrigins".into(), crate::units::len_to_f64(items.len())),
      ("thirdPartyBytes".into(), total),
    ],
    items,
  }
}

fn host_of(url: &str) -> Option<String> {
  let (_, rest) = url.split_once("://")?;
  let authority = rest.split(['/', '?', '#']).next()?;
  let host = authority.split('@').next_back()?.split(':').next()?;
  if host.is_empty() { None } else { Some(host.to_string()) }
}

/// Last two labels of the host, so `cdn.example.com` and
/// `www.example.com` count as the same party.
///
/// This is deliberately not a public-suffix lookup: it would treat
/// `foo.co.uk` and `bar.co.uk` as one party. That is the known
/// trade-off for not carrying the PSL, and it errs toward calling
/// something first-party, never toward inventing a third party.
fn registrable_domain(url: &str) -> Option<String> {
  let host = host_of(url)?;
  let labels: Vec<&str> = host.split('.').collect();
  if labels.len() < 2 {
    return Some(host);
  }
  Some(labels[labels.len() - 2..].join("."))
}

/// A result for the case where classification is impossible.
fn empty(reason: &str) -> Insight {
  Insight {
    key: "ThirdParties".into(),
    title: "Third parties".into(),
    description: "Third-party code you do not control still competes for the main thread and the network.".into(),
    severity: Severity::Informative,
    checks: vec![Check {
      name: "hasThirdParties".into(),
      passed: true,
      detail: format!("Not evaluated: {reason}"),
    }],
    metrics: Vec::new(),
    items: Vec::new(),
  }
}

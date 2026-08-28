//! Chains of requests that each had to wait for the one before.
//!
//! Mirrors devtools-frontend `insights/NetworkDependencyTree.ts`.
//!
//! A chain forms when a request's initiator is itself a request: the
//! HTML finds a script, which fetches another script, which fetches the
//! data. Nothing in a chain can start until its parent finishes, so the
//! chain's total is a floor on how fast the page can possibly load, and
//! the fix is to shorten it rather than to speed up any one hop.

use rustc_hash::FxHashMap;

use crate::event::Micro;
use crate::handlers::network::NetworkRequest;
use crate::insights::{Check, Insight, Item, Severity};
use crate::units::{len_to_f64, micros_to_ms};

/// A chain this long is a finding. Upstream fails on `path.length >= 2`,
/// so the document plus one critical resource is already a chain: the
/// resource could not be discovered until the HTML arrived. It reads
/// aggressive, but matching it is the point of a port.
const MIN_REPORTABLE_CHAIN_LENGTH: usize = 2;

#[must_use]
pub fn run(requests: &[NetworkRequest], document_url: &str) -> Insight {
  // A request's parent is whatever request its initiator names.
  let by_url: FxHashMap<&str, usize> = requests
    .iter()
    .enumerate()
    .map(|(index, request)| (request.url.as_str(), index))
    .collect();

  let mut longest: Vec<usize> = Vec::new();
  let mut longest_latency: Micro = 0;

  for (index, request) in requests.iter().enumerate() {
    // Only critical resources block rendering; an async image at the end
    // of a chain does not hold the page up.
    if !is_critical(request) {
      continue;
    }
    let chain = chain_to_root(requests, &by_url, index, document_url);
    // The chain's cost is measured from where it starts to where it ends,
    // not by summing hops, because hops can overlap.
    let latency = chain.iter().map(|i| requests[*i].timing.finish_time).max().unwrap_or(0)
      - chain.iter().map(|i| requests[*i].start_time).min().unwrap_or(0);
    if chain.len() > longest.len() || (chain.len() == longest.len() && latency > longest_latency) {
      longest = chain;
      longest_latency = latency;
    }
  }

  let passed = longest.len() < MIN_REPORTABLE_CHAIN_LENGTH;
  let items: Vec<Item> = longest
    .iter()
    .enumerate()
    .map(|(depth, index)| Item {
      label: format!("{}{}", "  ".repeat(depth), requests[*index].url),
      value: micros_to_ms(requests[*index].end_time - requests[*index].start_time),
      unit: "ms",
    })
    .collect();

  Insight {
    key: "NetworkDependencyTree".into(),
    title: "Network dependency tree".into(),
    description: "Avoid chaining critical requests by reducing the length of chains, reducing the download size \
                  of resources, or deferring the download of unnecessary resources."
      .into(),
    severity: if passed { Severity::Pass } else { Severity::Fail },
    checks: vec![Check {
      name: "noLongCriticalChain".into(),
      passed,
      detail: if passed {
        "No long chains of critical network requests".into()
      } else {
        format!(
          "Longest critical chain is {} requests over {:.0} ms",
          longest.len(),
          micros_to_ms(longest_latency)
        )
      },
    }],
    metrics: vec![
      ("maxChainLength".into(), len_to_f64(longest.len())),
      ("maxCriticalPathLatencyMs".into(), micros_to_ms(longest_latency)),
    ],
    items,
  }
}

/// Whether a request holds up rendering.
///
/// Anything the browser marked render-blocking counts, as do the
/// high-priority resource types the parser must have before it can
/// continue.
fn is_critical(request: &NetworkRequest) -> bool {
  if matches!(request.render_blocking.as_str(), "blocking" | "in_body_parser_blocking") {
    return true;
  }
  matches!(request.resource_type.as_str(), "Document" | "Stylesheet" | "Script")
    && matches!(request.priority.as_str(), "VeryHigh" | "High")
}

/// The chain from the document down to this request, root first.
///
/// Walks initiator links upward. A cycle would otherwise loop forever,
/// so a request already on the path ends the walk.
fn chain_to_root(
  requests: &[NetworkRequest],
  by_url: &FxHashMap<&str, usize>,
  start: usize,
  document_url: &str,
) -> Vec<usize> {
  let mut chain = vec![start];
  let mut current = start;

  while requests[current].url != document_url {
    let initiator = requests[current].initiator_url.as_str();
    if initiator.is_empty() {
      break;
    }
    let Some(&parent) = by_url.get(initiator) else { break };
    if chain.contains(&parent) {
      break;
    }
    chain.push(parent);
    current = parent;
  }

  chain.reverse();
  chain
}

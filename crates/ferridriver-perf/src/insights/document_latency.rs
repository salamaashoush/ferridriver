//! Document request latency: redirects, server response time and
//! whether the main document came back compressed.
//!
//! Mirrors `devtools-frontend` `insights/DocumentLatency.ts`, thresholds
//! included.

use crate::handlers::network::NetworkRequest;
use crate::insights::{Check, Insight, Severity};

/// `DevTools` throttling cannot resolve a server response faster than
/// ~570ms, so the failure line sits at 600ms to keep those out; the
/// number developers are told to aim at is 100ms.
const TOO_SLOW_THRESHOLD_MS: f64 = 600.0;

/// Compression savings below this are not worth reporting.
const IGNORE_THRESHOLD_IN_BYTES: i64 = 1400;

#[must_use]
pub fn run(document: Option<&NetworkRequest>) -> Option<Insight> {
  let document = document?;

  let server_response_ms = crate::units::micros_to_ms(document.timing.server_response_time).round();
  let redirect_total: i64 = document.redirects.iter().map(|r| r.dur).sum();
  let redirect_ms = crate::units::micros_to_ms(redirect_total);
  let savings = compression_savings(document);

  let checks = vec![
    Check {
      name: "noRedirects".into(),
      passed: document.redirects.is_empty(),
      detail: if document.redirects.is_empty() {
        "Avoids redirects".into()
      } else {
        format!(
          "Had redirects ({} redirects, +{redirect_ms:.0} ms)",
          document.redirects.len()
        )
      },
    },
    Check {
      name: "serverResponseIsFast".into(),
      passed: server_response_ms < TOO_SLOW_THRESHOLD_MS,
      detail: if server_response_ms < TOO_SLOW_THRESHOLD_MS {
        format!("Server responds quickly (observed {server_response_ms:.0} ms)")
      } else {
        format!("Server responded slowly (observed {server_response_ms:.0} ms)")
      },
    },
    Check {
      name: "usesCompression".into(),
      passed: savings == 0,
      detail: if savings == 0 {
        "Applies text compression".into()
      } else {
        format!("No compression applied (~{savings} bytes could be saved)")
      },
    },
  ];

  let failed = checks.iter().any(|c| !c.passed);
  Some(Insight {
    key: "DocumentLatency".into(),
    title: "Document request latency".into(),
    description: "Your first network request is the most important. Reduce its latency by avoiding redirects, \
                  ensuring a fast server response, and enabling text compression."
      .into(),
    severity: if failed { Severity::Fail } else { Severity::Pass },
    checks,
    metrics: vec![
      ("serverResponseTimeMs".into(), server_response_ms),
      ("redirectDurationMs".into(), redirect_ms),
      ("uncompressedResponseBytes".into(), crate::units::count_to_f64(savings)),
    ],
    items: Vec::new(),
  })
}

/// Estimated bytes a text response would have saved under gzip.
///
/// The multipliers are `HTTPArchive` averages, carried over verbatim
/// because changing them would put our numbers quietly out of step with
/// what `DevTools` reports for the same page.
fn compression_savings(request: &NetworkRequest) -> i64 {
  if request.is_compressed() {
    return 0;
  }
  let original = request.decoded_body_length;
  let ratio = match request.mime_type.as_str() {
    // Stylesheets compress extremely well.
    "text/css" => 0.8,
    "text/html" | "text/javascript" => 0.67,
    "text/plain"
    | "text/xml"
    | "text/x-component"
    | "application/javascript"
    | "application/json"
    | "application/manifest+json"
    | "application/vnd.api+json"
    | "application/xml"
    | "application/xhtml+xml"
    | "application/rss+xml"
    | "application/atom+xml"
    | "application/vnd.ms-fontobject"
    | "application/x-font-ttf"
    | "application/x-font-opentype"
    | "application/x-font-truetype"
    | "image/svg+xml"
    | "image/x-icon"
    | "image/vnd.microsoft.icon"
    | "font/ttf"
    | "font/eot"
    | "font/otf"
    | "font/opentype" => 0.5,
    // Anything else is likely compressed already.
    _ => return 0,
  };
  let estimated = crate::units::f64_to_count((crate::units::count_to_f64(original) * ratio).round());
  if estimated < IGNORE_THRESHOLD_IN_BYTES {
    0
  } else {
    estimated
  }
}

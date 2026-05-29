//! HAR (HTTP Archive) replay for `page.routeFromHAR` / `context.routeFromHAR`.
//!
//! Replay-only: a recorded HAR file is parsed and its entries are served back
//! for matching requests via the normal route/fulfill plumbing. HAR *recording*
//! (`update: true`) is not implemented; callers asking for it get a typed
//! [`crate::error::FerriError::Unsupported`].
//!
//! Matching mirrors Playwright's replay: the first entry whose request method
//! and URL equal the incoming request wins; on no match the configured
//! [`HarNotFound`] action (abort or fall through to the network) is taken.

use crate::error::{FerriError, Result};
use crate::route::{FulfillResponse, RouteHandler};
use std::sync::Arc;

/// What to do when no HAR entry matches a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HarNotFound {
  /// Abort the request (Playwright default).
  #[default]
  Abort,
  /// Fall through to the real network.
  Fallback,
}

/// Options for `routeFromHAR`.
#[derive(Debug, Clone, Default)]
pub struct RouteFromHarOptions {
  /// Only serve requests whose URL matches this matcher (Playwright `url`).
  pub url: Option<crate::url_matcher::UrlMatcher>,
  /// Action when no recorded entry matches.
  pub not_found: HarNotFound,
}

// ── HAR JSON subset (serde) ────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct HarFile {
  log: HarLog,
}

#[derive(serde::Deserialize)]
struct HarLog {
  #[serde(default)]
  entries: Vec<HarEntry>,
}

#[derive(serde::Deserialize)]
struct HarEntry {
  request: HarRequest,
  response: HarResponse,
}

#[derive(serde::Deserialize)]
struct HarRequest {
  method: String,
  url: String,
}

#[derive(serde::Deserialize)]
struct HarResponse {
  status: i32,
  #[serde(default)]
  headers: Vec<HarHeader>,
  #[serde(default)]
  content: HarContent,
}

#[derive(serde::Deserialize)]
struct HarHeader {
  name: String,
  value: String,
}

#[derive(serde::Deserialize, Default)]
struct HarContent {
  #[serde(default)]
  text: Option<String>,
  #[serde(default)]
  encoding: Option<String>,
  #[serde(default, rename = "mimeType")]
  mime_type: Option<String>,
}

/// A single replayable response keyed by `(method, url)`.
struct Recorded {
  method: String,
  url: String,
  response: FulfillResponse,
}

impl HarEntry {
  fn into_recorded(self) -> Recorded {
    let body = match (&self.response.content.text, self.response.content.encoding.as_deref()) {
      (Some(text), Some("base64")) => {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
          .decode(text)
          .unwrap_or_else(|_| text.clone().into_bytes())
      },
      (Some(text), _) => text.clone().into_bytes(),
      (None, _) => Vec::new(),
    };
    let headers = self
      .response
      .headers
      .into_iter()
      // Drop hop-by-hop / length headers that the fulfill path recomputes.
      .filter(|h| {
        let n = h.name.to_ascii_lowercase();
        n != "content-length" && n != "content-encoding"
      })
      .map(|h| (h.name, h.value))
      .collect();
    Recorded {
      method: self.request.method.to_ascii_uppercase(),
      url: self.request.url,
      response: FulfillResponse {
        status: self.response.status,
        headers,
        body,
        content_type: self.response.content.mime_type,
      },
    }
  }
}

/// Parse a HAR file from disk into a replay [`RouteHandler`].
///
/// # Errors
///
/// Returns an error if the file cannot be read or is not valid HAR JSON.
pub fn route_handler_from_file(path: &std::path::Path, not_found: HarNotFound) -> Result<RouteHandler> {
  let bytes = std::fs::read(path).map_err(|e| FerriError::backend(format!("read HAR {}: {e}", path.display())))?;
  let parsed: HarFile =
    serde_json::from_slice(&bytes).map_err(|e| FerriError::backend(format!("parse HAR {}: {e}", path.display())))?;
  let entries: Arc<Vec<Recorded>> = Arc::new(parsed.log.entries.into_iter().map(HarEntry::into_recorded).collect());

  Ok(Arc::new(move |route| {
    let req = route.request();
    let method = req.method.to_ascii_uppercase();
    let url = req.url.clone();
    let hit = entries
      .iter()
      .find(|e| e.method == method && e.url == url)
      .or_else(|| entries.iter().find(|e| e.url == url));
    match hit {
      Some(rec) => route.fulfill(rec.response.clone()),
      None => match not_found {
        HarNotFound::Abort => route.abort("failed"),
        HarNotFound::Fallback => route.fallback(crate::route::ContinueOverrides::default()),
      },
    }
  }))
}

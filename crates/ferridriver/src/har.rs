//! HAR (HTTP Archive) replay for `page.routeFromHAR` / `context.routeFromHAR`.
//!
//! Replay: a recorded HAR — plain `.har` JSON (inline or `_file`-attached
//! bodies) or a `.zip` archive (`har.har` + `<sha1>.<ext>` body entries) —
//! is parsed and its entries are served back for matching requests via the
//! normal route/fulfill plumbing. Recording (`update: true`) is handled by
//! the context layer, which registers a [`crate::tracing::HarRecorder`]
//! flushed at context close.
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
  /// Record network into the HAR instead of replaying it. The file is
  /// written when the context closes (Playwright `update: true`).
  pub update: bool,
  /// Body policy for `update` recording. Playwright default: `attach`.
  pub update_content: Option<crate::tracing::HarContentPolicy>,
  /// Detail mode for `update` recording. Playwright default: `minimal`.
  pub update_mode: Option<crate::tracing::HarMode>,
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
  /// `attach`-policy body reference: zip entry name or file next to the
  /// `.har` (Playwright `content._file`).
  #[serde(default, rename = "_file")]
  file: Option<String>,
}

/// A single replayable response keyed by `(method, url)`.
struct Recorded {
  method: String,
  url: String,
  response: FulfillResponse,
}

impl HarEntry {
  fn into_recorded(self, resolve_file: &dyn Fn(&str) -> Option<Vec<u8>>) -> Recorded {
    let body = if let Some(name) = &self.response.content.file {
      resolve_file(name).unwrap_or_default()
    } else {
      match (&self.response.content.text, self.response.content.encoding.as_deref()) {
        (Some(text), Some("base64")) => {
          use base64::Engine;
          base64::engine::general_purpose::STANDARD
            .decode(text)
            .unwrap_or_else(|_| text.clone().into_bytes())
        },
        (Some(text), _) => text.clone().into_bytes(),
        (None, _) => Vec::new(),
      }
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

/// Parse a HAR file from disk into a replay [`RouteHandler`]. Accepts a
/// plain `.har` JSON file (with inline or `_file`-attached bodies read
/// from sibling files) or a `.zip` archive (the `.har` entry plus
/// `<sha1>.<ext>` body entries, as written by Playwright and by
/// ferridriver's `attach` recorder).
///
/// # Errors
///
/// Returns an error if the file cannot be read, the zip has no `.har`
/// entry, or the HAR JSON is invalid.
pub fn route_handler_from_file(path: &std::path::Path, not_found: HarNotFound) -> Result<RouteHandler> {
  let entries: Arc<Vec<Recorded>> = Arc::new(if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
    load_zip_entries(path)?
  } else {
    load_plain_entries(path)?
  });

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

/// Load replay entries from a plain `.har` JSON file; `_file` body
/// references resolve against the HAR's directory.
fn load_plain_entries(path: &std::path::Path) -> Result<Vec<Recorded>> {
  let bytes = std::fs::read(path).map_err(|e| FerriError::backend(format!("read HAR {}: {e}", path.display())))?;
  let parsed: HarFile =
    serde_json::from_slice(&bytes).map_err(|e| FerriError::backend(format!("parse HAR {}: {e}", path.display())))?;
  let base_dir = path.parent().map(std::path::Path::to_path_buf);
  let resolve = |name: &str| -> Option<Vec<u8>> {
    let dir = base_dir.as_deref()?;
    std::fs::read(dir.join(name)).ok()
  };
  Ok(
    parsed
      .log
      .entries
      .into_iter()
      .map(|e| e.into_recorded(&resolve))
      .collect(),
  )
}

/// Load replay entries from a `.zip` HAR archive: the first `*.har`
/// entry is the log; `_file` body references resolve to zip entries.
fn load_zip_entries(path: &std::path::Path) -> Result<Vec<Recorded>> {
  let file = std::fs::File::open(path).map_err(|e| FerriError::backend(format!("read HAR {}: {e}", path.display())))?;
  let mut archive =
    zip::ZipArchive::new(file).map_err(|e| FerriError::backend(format!("open HAR zip {}: {e}", path.display())))?;

  let mut har_json: Option<Vec<u8>> = None;
  let mut resources: rustc_hash::FxHashMap<String, Vec<u8>> = rustc_hash::FxHashMap::default();
  for i in 0..archive.len() {
    let mut entry = archive
      .by_index(i)
      .map_err(|e| FerriError::backend(format!("read HAR zip {}: {e}", path.display())))?;
    let name = entry.name().to_string();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut entry, &mut bytes)
      .map_err(|e| FerriError::backend(format!("read HAR zip entry {name}: {e}")))?;
    if name.to_ascii_lowercase().ends_with(".har") && har_json.is_none() {
      har_json = Some(bytes);
    } else {
      resources.insert(name, bytes);
    }
  }
  let har_json =
    har_json.ok_or_else(|| FerriError::backend(format!("HAR zip {} contains no .har entry", path.display())))?;
  let parsed: HarFile =
    serde_json::from_slice(&har_json).map_err(|e| FerriError::backend(format!("parse HAR {}: {e}", path.display())))?;
  let resolve = |name: &str| -> Option<Vec<u8>> { resources.get(name).cloned() };
  Ok(
    parsed
      .log
      .entries
      .into_iter()
      .map(|e| e.into_recorded(&resolve))
      .collect(),
  )
}

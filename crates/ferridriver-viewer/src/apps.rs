//! The embedded Playwright front-ends.
//!
//! Each app is a static build lifted out of the `playwright-core` npm package
//! by `scripts/vendor-playwright-assets.sh` and committed as a zip. They are
//! unpacked into memory on first use, so a binary that never opens a viewer
//! pays nothing but the bytes in its `.rodata`.

use axum::body::{Body, Bytes};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

/// Playwright release the embedded apps were built from. The wire protocols
/// implemented against them (trace format, test server, recorder) are pinned
/// to this version.
pub const PLAYWRIGHT_VERSION: &str = include_str!("../assets/PLAYWRIGHT_VERSION").trim_ascii();

/// One embedded front-end.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum App {
  /// Trace viewer (`index.html`) and UI mode (`uiMode.html`) — one build.
  TraceViewer,
  /// Recorder / inspector: resume, step, pick locator, call log.
  Recorder,
}

impl App {
  fn archive(self) -> &'static [u8] {
    match self {
      Self::TraceViewer => include_bytes!("../assets/traceviewer.zip"),
      Self::Recorder => include_bytes!("../assets/recorder.zip"),
    }
  }

  fn cache(self) -> &'static AppCache {
    match self {
      Self::TraceViewer => &TRACE_VIEWER,
      Self::Recorder => &RECORDER,
    }
  }

  /// One file out of the app, `None` when the app has no such entry.
  #[must_use]
  pub fn asset(self, path: &str) -> Option<Bytes> {
    let path = normalize(path);
    self.cache().get_or_init(|| unpack(self.archive())).get(path).cloned()
  }

  /// Every entry name in the app (test-facing; the server looks entries up
  /// by name).
  #[must_use]
  pub fn entry_names(self) -> Vec<String> {
    let mut names: Vec<String> = self
      .cache()
      .get_or_init(|| unpack(self.archive()))
      .keys()
      .cloned()
      .collect();
    names.sort();
    names
  }

  /// Serve one file as an HTTP response, 404 when the app has no such entry.
  #[must_use]
  pub fn response(self, path: &str) -> Response {
    let path = normalize(path);
    let Some(bytes) = self.asset(path) else {
      return StatusCode::NOT_FOUND.into_response();
    };
    // Content-Type is load-bearing, not cosmetic: a browser refuses to
    // register a service worker that does not arrive as JavaScript, and the
    // trace viewer is a service worker with a UI attached.
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache = if is_immutable(path) {
      "public, max-age=31536000, immutable"
    } else {
      "no-cache"
    };
    Response::builder()
      .header(header::CONTENT_TYPE, mime.as_ref())
      .header(header::CACHE_CONTROL, cache)
      .body(Body::from(bytes))
      .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
  }
}

/// Entry points and the service worker are revalidated so a re-vendored app
/// takes effect on reload; everything else carries a build hash in its name
/// and can be cached forever.
fn is_immutable(path: &str) -> bool {
  let is_html = std::path::Path::new(path)
    .extension()
    .is_some_and(|ext| ext.eq_ignore_ascii_case("html"));
  !is_html && path != "sw.bundle.js" && path != "manifest.webmanifest"
}

fn normalize(path: &str) -> &str {
  let path = path.trim_start_matches('/');
  if path.is_empty() { "index.html" } else { path }
}

type AppCache = std::sync::OnceLock<rustc_hash::FxHashMap<String, Bytes>>;

static TRACE_VIEWER: AppCache = std::sync::OnceLock::new();
static RECORDER: AppCache = std::sync::OnceLock::new();

fn unpack(archive: &'static [u8]) -> rustc_hash::FxHashMap<String, Bytes> {
  let mut assets = rustc_hash::FxHashMap::default();
  let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(archive)) else {
    tracing::error!(target: "ferridriver::viewer", "embedded web app archive is unreadable");
    return assets;
  };
  for index in 0..zip.len() {
    let Ok(mut entry) = zip.by_index(index) else { continue };
    if entry.is_dir() {
      continue;
    }
    let name = entry.name().trim_start_matches("./").to_string();
    let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or_default());
    if std::io::Read::read_to_end(&mut entry, &mut bytes).is_ok() {
      assets.insert(name, Bytes::from(bytes));
    }
  }
  assets
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn version_is_a_bare_semver() {
    assert!(
      PLAYWRIGHT_VERSION.split('.').count() == 3 && PLAYWRIGHT_VERSION.starts_with(char::is_numeric),
      "unexpected pinned version {PLAYWRIGHT_VERSION:?}"
    );
  }

  #[test]
  fn trace_viewer_carries_both_front_ends_and_its_worker() {
    for entry in ["index.html", "uiMode.html", "snapshot.html", "sw.bundle.js", "LICENSE"] {
      assert!(App::TraceViewer.asset(entry).is_some(), "missing {entry}");
    }
  }

  #[test]
  fn the_recorder_unpacks() {
    assert!(App::Recorder.asset("index.html").is_some());
  }

  #[test]
  fn empty_path_serves_the_entry_point() {
    assert_eq!(App::Recorder.asset(""), App::Recorder.asset("index.html"));
    assert_eq!(App::Recorder.asset("/"), App::Recorder.asset("index.html"));
  }

  #[test]
  fn javascript_content_type_and_revalidated_worker() {
    let response = App::TraceViewer.response("sw.bundle.js");
    let headers = response.headers();
    let mime = headers[header::CONTENT_TYPE].to_str().expect("content type");
    assert!(mime.contains("javascript"), "service worker served as {mime}");
    assert_eq!(headers[header::CACHE_CONTROL], "no-cache");
  }

  #[test]
  fn hashed_assets_are_immutable() {
    let name = App::TraceViewer
      .entry_names()
      .into_iter()
      .find(|name| {
        name.starts_with("assets/")
          && std::path::Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("js"))
      })
      .expect("hashed asset");
    let response = App::TraceViewer.response(&name);
    assert!(
      response.headers()[header::CACHE_CONTROL]
        .to_str()
        .is_ok_and(|v| v.contains("immutable"))
    );
  }

  #[test]
  fn unknown_entry_is_not_found() {
    assert_eq!(App::TraceViewer.response("nope.js").status(), StatusCode::NOT_FOUND);
  }
}

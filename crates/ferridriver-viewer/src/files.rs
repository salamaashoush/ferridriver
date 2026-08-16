//! `GET /trace/file?path=<absolute path>` — how the viewer reads traces.
//!
//! The trace viewer never loads a trace by uploading it: its service worker
//! asks this route for one entry at a time. Two shapes answer it
//! (`traceViewer.ts::serveTraceDataRoute`):
//!
//! * an existing file is streamed back (zips, `.trace` streams, screencast
//!   frames, resource bodies);
//! * a MISSING `<prefix>.json` is answered with a synthesized descriptor —
//!   `{ entries: [{ name, path }] }` listing the loose files a still-running
//!   recording has produced so far. That is the whole live-trace mechanism:
//!   no zip is built while a test runs, the viewer just re-reads the growing
//!   files. A directory can be listed the same way through the
//!   `traces.dir` marker.
//!
//! Paths arrive from the browser, so every one is confined to a caller-
//! declared set of roots before anything is opened.

use std::path::{Component, Path, PathBuf};

use axum::body::Body;
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

/// Directory listing marker: `?path=<dir>/traces.dir` describes every trace
/// in `<dir>` (`traceViewer.ts:65`).
pub const TRACES_DIR_MARKER: &str = "traces.dir";

/// The directories a viewer may read files from.
///
/// Playwright allows the cwd, the config dir, and every project's test /
/// output dir; a `show-trace` of one file allows that file's directory.
#[derive(Clone, Debug, Default)]
pub struct FileRoots {
  roots: Vec<PathBuf>,
}

impl FileRoots {
  #[must_use]
  pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
    let mut normalized: Vec<PathBuf> = roots.into_iter().map(|root| normalize(&root)).collect();
    normalized.sort();
    normalized.dedup();
    Self { roots: normalized }
  }

  /// Whether `path` (already normalized) sits inside one of the roots.
  #[must_use]
  pub fn allows(&self, path: &Path) -> bool {
    self.roots.iter().any(|root| path.starts_with(root))
  }

  #[must_use]
  pub fn roots(&self) -> &[PathBuf] {
    &self.roots
  }
}

/// Resolve `..` / `.` lexically without touching the filesystem — the path
/// may not exist yet (a descriptor is synthesized for a missing `.json`).
fn normalize(path: &Path) -> PathBuf {
  let mut out = PathBuf::new();
  for component in path.components() {
    match component {
      Component::ParentDir => {
        out.pop();
      },
      Component::CurDir => {},
      other => out.push(other.as_os_str()),
    }
  }
  out
}

/// The `path` query parameter, percent-decoded.
fn query_path(request: &Request) -> Option<String> {
  let query = request.uri().query()?;
  for pair in query.split('&') {
    let (key, value) = pair.split_once('=')?;
    if key == "path" {
      return Some(percent_decode(value));
    }
  }
  None
}

fn percent_decode(value: &str) -> String {
  let bytes = value.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut index = 0;
  while index < bytes.len() {
    match bytes[index] {
      b'%' if index + 2 < bytes.len() => {
        let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
        if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
          out.push(byte);
          index += 3;
        } else {
          out.push(bytes[index]);
          index += 1;
        }
      },
      b'+' => {
        out.push(b' ');
        index += 1;
      },
      byte => {
        out.push(byte);
        index += 1;
      },
    }
  }
  String::from_utf8_lossy(&out).into_owned()
}

/// Serve one trace file (or a synthesized descriptor) for `request`.
pub async fn serve(roots: &FileRoots, request: Request) -> Response {
  let Some(raw) = query_path(&request) else {
    return StatusCode::BAD_REQUEST.into_response();
  };
  let path = normalize(Path::new(&raw));
  if !path.is_absolute() || !roots.allows(&path) {
    return StatusCode::FORBIDDEN.into_response();
  }

  if path.is_file() {
    // Streamed, with Range support: the viewer reads zip central directories
    // with byte-range requests instead of downloading whole archives.
    let served = tower::ServiceExt::oneshot(tower_http::services::ServeFile::new(&path), request).await;
    return match served {
      Ok(response) => response.into_response(),
      Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
  }

  if path.file_name().is_some_and(|name| name == TRACES_DIR_MARKER) {
    return match path.parent() {
      Some(dir) => descriptor_response(dir, None),
      None => StatusCode::NOT_FOUND.into_response(),
    };
  }

  if path.extension().is_some_and(|ext| ext == "json") {
    let (Some(dir), Some(prefix)) = (path.parent(), path.file_stem().and_then(|s| s.to_str())) else {
      return StatusCode::NOT_FOUND.into_response();
    };
    return descriptor_response(dir, Some(prefix));
  }

  StatusCode::NOT_FOUND.into_response()
}

fn descriptor_response(dir: &Path, prefix: Option<&str>) -> Response {
  let Some(descriptor) = trace_descriptor(dir, prefix) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  Response::builder()
    .header(header::CONTENT_TYPE, "application/json")
    // A live recording grows between polls; a cached descriptor would freeze
    // the viewer on the entries the first poll happened to see.
    .header(header::CACHE_CONTROL, "no-store")
    .body(Body::from(descriptor.to_string()))
    .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// `{ entries: [{ name, path }] }` for every file of `dir` whose name starts
/// with `prefix`, plus everything under its `resources/` directory
/// (`traceViewer.ts::traceDescriptor`).
#[must_use]
pub fn trace_descriptor(dir: &Path, prefix: Option<&str>) -> Option<serde_json::Value> {
  let read = std::fs::read_dir(dir).ok()?;
  let mut entries = Vec::new();
  for entry in read.flatten() {
    let name = entry.file_name().to_string_lossy().into_owned();
    if prefix.is_none_or(|prefix| name.starts_with(prefix)) {
      let path = entry.path();
      entries.push(serde_json::json!({ "name": name, "path": file_path_url(&path) }));
    }
  }
  let resources = dir.join("resources");
  if let Ok(read) = std::fs::read_dir(&resources) {
    for entry in read.flatten() {
      let name = format!("resources/{}", entry.file_name().to_string_lossy());
      entries.push(serde_json::json!({ "name": name, "path": file_path_url(&entry.path()) }));
    }
  }
  Some(serde_json::json!({ "entries": entries }))
}

/// How the viewer addresses one file: `file?path=<percent-encoded>` relative
/// to the app's own directory (`traceViewer.ts::toFilePathUrl`).
#[must_use]
pub fn file_path_url(path: &Path) -> String {
  format!("file?path={}", encode_component(&path.to_string_lossy()))
}

/// Percent-encode everything outside the unreserved set, `/` included — the
/// value travels as a single query parameter.
#[must_use]
pub fn encode_component(value: &str) -> String {
  use std::fmt::Write as _;

  let mut encoded = String::with_capacity(value.len());
  for byte in value.bytes() {
    match byte {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => encoded.push(byte as char),
      _ => {
        let _ = write!(encoded, "%{byte:02X}");
      },
    }
  }
  encoded
}

#[cfg(test)]
mod tests {
  use super::*;

  fn get(uri: &str) -> Request {
    Request::builder().uri(uri).body(Body::empty()).expect("request")
  }

  #[test]
  fn roots_reject_outside_and_traversal() {
    let roots = FileRoots::new([PathBuf::from("/tmp/run")]);
    assert!(roots.allows(Path::new("/tmp/run/traces/a.trace")));
    assert!(!roots.allows(Path::new("/tmp/other/a.trace")));
    assert!(!roots.allows(&normalize(Path::new("/tmp/run/../etc/passwd"))));
  }

  #[test]
  fn query_path_is_percent_decoded() {
    let request = get("/trace/file?path=%2Ftmp%2Fa%20b%2Ftrace.zip&timestamp=7");
    assert_eq!(query_path(&request).as_deref(), Some("/tmp/a b/trace.zip"));
    assert!(query_path(&get("/trace/file")).is_none());
  }

  #[test]
  fn file_url_round_trips_through_the_query_parser() {
    let url = file_path_url(Path::new("/tmp/a b/trace.zip"));
    let request = get(&format!("/trace/{url}"));
    assert_eq!(query_path(&request).as_deref(), Some("/tmp/a b/trace.zip"));
  }

  #[tokio::test]
  async fn missing_json_synthesizes_a_descriptor_of_the_live_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let traces = dir.path().join("traces");
    std::fs::create_dir_all(traces.join("resources")).expect("mkdir");
    std::fs::write(traces.join("t1.trace"), b"{}").expect("write");
    std::fs::write(traces.join("t1.network"), b"").expect("write");
    std::fs::write(traces.join("t2.trace"), b"").expect("write");
    std::fs::write(traces.join("resources").join("abc"), b"body").expect("write");

    let roots = FileRoots::new([dir.path().to_path_buf()]);
    let uri = format!("/trace/{}", file_path_url(&traces.join("t1.json")));
    let response = serve(&roots, get(&uri)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1 << 20).await.expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    let names: Vec<&str> = json["entries"]
      .as_array()
      .expect("entries")
      .iter()
      .filter_map(|e| e["name"].as_str())
      .collect();
    assert!(names.contains(&"t1.trace"), "{names:?}");
    assert!(names.contains(&"t1.network"), "{names:?}");
    assert!(names.contains(&"resources/abc"), "{names:?}");
    assert!(!names.contains(&"t2.trace"), "prefix filter leaked: {names:?}");
  }

  #[tokio::test]
  async fn traces_dir_marker_lists_everything() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.trace"), b"").expect("write");
    std::fs::write(dir.path().join("b.trace"), b"").expect("write");
    let roots = FileRoots::new([dir.path().to_path_buf()]);
    let uri = format!("/trace/{}", file_path_url(&dir.path().join(TRACES_DIR_MARKER)));
    let response = serve(&roots, get(&uri)).await;
    let body = axum::body::to_bytes(response.into_body(), 1 << 20).await.expect("body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json["entries"].as_array().map(Vec::len), Some(2));
  }

  #[tokio::test]
  async fn existing_file_is_served_and_outside_paths_are_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("trace.zip");
    std::fs::write(&file, b"PK\x03\x04").expect("write");
    let roots = FileRoots::new([dir.path().to_path_buf()]);

    let response = serve(&roots, get(&format!("/trace/{}", file_path_url(&file)))).await;
    assert_eq!(response.status(), StatusCode::OK);

    let outside = serve(&roots, get("/trace/file?path=%2Fetc%2Fpasswd")).await;
    assert_eq!(outside.status(), StatusCode::FORBIDDEN);

    let relative = serve(&roots, get("/trace/file?path=trace.zip")).await;
    assert_eq!(relative.status(), StatusCode::FORBIDDEN);
  }
}

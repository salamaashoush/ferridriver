//! Playwright's own web front-ends, embedded and served from ferridriver.
//!
//! The trace viewer, the UI-mode app, the recorder/inspector and the HTML
//! report front-end are static builds vendored out of `playwright-core`
//! (see `scripts/vendor-playwright-assets.sh`) and compiled into the binary.
//! Nothing here shells out, downloads, or needs node — a `ferridriver` on a
//! disconnected machine opens the same UI as an online one.
//!
//! Two things make those apps work, and both live in this crate:
//!
//! * [`apps`] — the archives and the asset responses (content types and
//!   cache policy the service worker registration depends on);
//! * [`files`] — the `/trace/file?path=` route the viewer reads traces
//!   through, including the synthesized descriptor that serves a recording
//!   while it is still being written.
//!
//! Callers mount [`router`] on their own server and point a browser at
//! [`app_url`].

pub mod apps;
pub mod dump;
pub mod files;
pub mod model;

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::response::Response;
use axum::routing::get;

pub use apps::{App, PLAYWRIGHT_VERSION};
pub use files::{FileRoots, TRACES_DIR_MARKER};

/// Path prefix Playwright's apps are served under. Baked into the vendored
/// bundles' asset URLs and service-worker scope, so it is not configurable.
pub const TRACE_PREFIX: &str = "/trace";

struct ViewerState {
  app: App,
  roots: FileRoots,
}

/// Routes serving `app` under `/trace`, reading traces from `roots`.
pub fn router(app: App, roots: FileRoots) -> Router {
  let state = Arc::new(ViewerState { app, roots });
  Router::new()
    .route("/trace/file", get(file))
    .route("/trace", get(index))
    .route("/trace/", get(index))
    .route("/trace/{*path}", get(asset))
    .with_state(state)
}

async fn index(State(state): State<Arc<ViewerState>>) -> Response {
  state.app.response("index.html")
}

async fn asset(State(state): State<Arc<ViewerState>>, request: Request) -> Response {
  let path = request
    .uri()
    .path()
    .strip_prefix("/trace/")
    .unwrap_or_default()
    .to_string();
  state.app.response(&path)
}

async fn file(State(state): State<Arc<ViewerState>>, request: Request) -> Response {
  files::serve(&state.roots, request).await
}

/// URL of one embedded app, with `params` as its query string.
///
/// `web_app` is the app's entry point: `index.html` for the trace viewer,
/// `uiMode.html` for UI mode (`traceViewer.ts::installRootRedirect`).
#[must_use]
pub fn app_url(base: &str, web_app: &str, params: &[(&str, String)]) -> String {
  let mut url = format!("{}{TRACE_PREFIX}/{web_app}", base.trim_end_matches('/'));
  for (index, (key, value)) in params.iter().enumerate() {
    url.push(if index == 0 { '?' } else { '&' });
    url.push_str(key);
    url.push('=');
    url.push_str(&files::encode_component(value));
  }
  url
}

/// How a trace file, directory, or descriptor is named in an `app_url`
/// `trace` parameter. Remote traces are passed through as-is; local ones
/// become a `file?path=` reference the [`files`] route answers.
#[must_use]
pub fn trace_param(trace: &std::path::Path) -> String {
  files::file_path_url(trace)
}

/// Open `url` in a chromium app window and return when it is closed.
///
/// An application window rather than a browser tab, because that is what
/// these pages are: Playwright opens its viewer and its UI mode the same
/// way (`traceViewer.ts::openTraceViewerApp`). Closing the window is how
/// a person ends the session, so the caller treats returning as "done".
///
/// # Errors
///
/// Errors when no browser could be launched — the caller should fall
/// back to printing the URL.
pub async fn open_app_window(url: &str) -> Result<(), String> {
  let browser = ferridriver::chromium()
    .launch(ferridriver::options::LaunchOptions {
      headless: Some(false),
      args: vec![format!("--app={url}"), "--window-size=1280,800".to_string()],
      ..ferridriver::options::LaunchOptions::default()
    })
    .await
    .map_err(|e| e.to_string())?;

  // `--app` opened the window already; `page()` adopts it.
  let page = Box::pin(browser.page()).await.map_err(|e| e.to_string())?;
  tokio::select! {
    _ = page.wait_for_event("close", Some(86_400_000)) => {},
    _ = tokio::signal::ctrl_c() => {},
  }
  let _ = browser.close().await;
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use axum::body::Body;
  use axum::http::StatusCode;
  use tower::ServiceExt;

  async fn get_path(router: Router, uri: &str) -> Response {
    router
      .oneshot(Request::builder().uri(uri).body(Body::empty()).expect("request"))
      .await
      .expect("response")
  }

  #[tokio::test]
  async fn serves_ui_mode_and_the_worker_from_the_trace_prefix() {
    let router = router(App::TraceViewer, FileRoots::default());
    for uri in [
      "/trace/uiMode.html",
      "/trace/sw.bundle.js",
      "/trace/index.html",
      "/trace/",
    ] {
      let response = get_path(router.clone(), uri).await;
      assert_eq!(response.status(), StatusCode::OK, "{uri}");
    }
  }

  #[tokio::test]
  async fn nested_asset_paths_resolve() {
    let router = router(App::TraceViewer, FileRoots::default());
    let name = App::TraceViewer
      .entry_names()
      .into_iter()
      .find(|n| n.starts_with("assets/"))
      .expect("nested asset");
    let response = get_path(router, &format!("/trace/{name}")).await;
    assert_eq!(response.status(), StatusCode::OK);
  }

  #[tokio::test]
  async fn file_route_is_confined_to_declared_roots() {
    let router = router(
      App::TraceViewer,
      FileRoots::new([std::path::PathBuf::from("/tmp/allowed")]),
    );
    let response = get_path(router, "/trace/file?path=%2Fetc%2Fpasswd").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
  }

  #[test]
  fn app_urls_carry_encoded_parameters() {
    let url = app_url(
      "http://127.0.0.1:9323",
      "uiMode.html",
      &[("ws", "abc123".into()), ("pathSeparator", "/".into())],
    );
    assert_eq!(
      url,
      "http://127.0.0.1:9323/trace/uiMode.html?ws=abc123&pathSeparator=%2F"
    );
  }

  #[test]
  fn trace_parameter_points_at_the_file_route() {
    assert_eq!(
      trace_param(std::path::Path::new("/runs/trace.zip")),
      "file?path=%2Fruns%2Ftrace.zip"
    );
  }
}

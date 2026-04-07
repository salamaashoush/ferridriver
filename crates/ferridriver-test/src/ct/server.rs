//! ComponentServer: embedded HTTP server for component testing.
//!
//! Serves static files (HTML, JS, WASM, CSS) from a directory on a random port.
//! Used by both WASM and Vite paths — the WASM path serves compiled output directly,
//! the Vite path uses this as a fallback or the Vite dev server directly.

use std::net::SocketAddr;
use std::path::Path;

use axum::Router;
use tower_http::services::ServeDir;

/// A lightweight HTTP server for serving component test assets.
pub struct ComponentServer {
  addr: SocketAddr,
  shutdown_tx: tokio::sync::oneshot::Sender<()>,
  handle: tokio::task::JoinHandle<()>,
}

impl ComponentServer {
  /// Start serving files from `root_dir` on a random available port.
  ///
  /// # Errors
  ///
  /// Returns an error if the server fails to bind.
  pub async fn start(root_dir: &Path) -> Result<Self, String> {
    let service = ServeDir::new(root_dir).append_index_html_on_directories(true);

    let app = Router::new().fallback_service(service);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
      .await
      .map_err(|e| format!("bind failed: {e}"))?;

    let addr = listener.local_addr().map_err(|e| format!("local_addr: {e}"))?;

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let handle = tokio::spawn(async move {
      axum::serve(listener, app)
        .with_graceful_shutdown(async {
          let _ = shutdown_rx.await;
        })
        .await
        .ok();
    });

    Ok(Self {
      addr,
      shutdown_tx,
      handle,
    })
  }

  /// The base URL of the server (e.g. `http://127.0.0.1:39201`).
  #[must_use]
  pub fn url(&self) -> String {
    format!("http://{}", self.addr)
  }

  /// The port the server is listening on.
  #[must_use]
  pub fn port(&self) -> u16 {
    self.addr.port()
  }

  /// Stop the server.
  pub async fn stop(self) {
    let _ = self.shutdown_tx.send(());
    let _ = self.handle.await;
  }
}

/// The minimal HTML wrapper that loads a WASM component.
/// Sets `data-mounted="true"` on body after WASM init completes.
pub fn wasm_html_wrapper(wasm_js_path: &str) -> String {
  format!(
    r#"<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"></head>
<body>
<div id="app"></div>
<script type="module">
import init from './{wasm_js_path}';
await init();
document.body.setAttribute('data-mounted', 'true');
</script>
</body>
</html>"#
  )
}

/// The minimal HTML wrapper that loads a JS component (from Vite).
/// The Vite dev server handles HMR and bundling; this just provides the shell.
pub fn vite_html_wrapper(entry_path: &str) -> String {
  format!(
    r#"<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"></head>
<body>
<div id="app"></div>
<script type="module" src="{entry_path}"></script>
</body>
</html>"#
  )
}

//! Ferridriver component testing adapter for Dioxus.
//!
//! Integrates with `dx serve` — the Dioxus CLI dev server.
//!
//! # Usage
//!
//! ```ignore
//! use ferridriver_ct_dioxus::DioxusComponentTest;
//!
//! #[tokio::test]
//! async fn test_counter() {
//!     let ct = DioxusComponentTest::new("./examples/ct-dioxus")
//!         .start()
//!         .await
//!         .unwrap();
//!
//!     let page = ct.new_page().await.unwrap();
//!     page.locator("button").click().await.unwrap();
//!     let text = page.locator("#count").text_content().await.unwrap();
//!     assert_eq!(text.unwrap(), "1");
//!
//!     ct.stop().await;
//! }
//! ```

use std::path::PathBuf;

use ferridriver_test::ct::devserver::{DevServer, DevServerConfig};

/// Builder for Dioxus component tests.
pub struct DioxusComponentTest {
  project_dir: PathBuf,
  port: Option<u16>,
}

/// A running Dioxus component test environment.
pub struct DioxusTestEnv {
  dev_server: DevServer,
  browser: ferridriver::Browser,
}

impl DioxusComponentTest {
  pub fn new(project_dir: impl Into<PathBuf>) -> Self {
    Self {
      project_dir: project_dir.into(),
      port: None,
    }
  }

  /// Override the port (default: auto-assigned by dx).
  #[must_use]
  pub fn port(mut self, port: u16) -> Self {
    self.port = Some(port);
    self
  }

  /// Start `dx serve` and launch a browser.
  pub async fn start(self) -> Result<DioxusTestEnv, String> {
    let mut config = DevServerConfig::dioxus(&self.project_dir);
    if let Some(port) = self.port {
      config.args.push("--port".into());
      config.args.push(port.to_string());
    }

    let dev_server = ferridriver_test::ct::devserver::start(&config).await?;

    let browser = ferridriver::Browser::launch(ferridriver::options::LaunchOptions::default())
      .await
      .map_err(|e| format!("browser launch: {e}"))?;

    Ok(DioxusTestEnv { dev_server, browser })
  }
}

impl DioxusTestEnv {
  #[must_use]
  pub fn url(&self) -> &str {
    self.dev_server.url()
  }

  pub async fn new_page(&self) -> Result<ferridriver::Page, String> {
    self.browser.new_page_with_url(self.dev_server.url()).await
  }

  pub async fn stop(self) {
    let _ = self.browser.close().await;
    self.dev_server.stop().await;
  }
}

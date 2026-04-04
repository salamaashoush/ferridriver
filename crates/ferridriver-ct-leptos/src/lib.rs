//! Ferridriver component testing adapter for Leptos.
//!
//! Integrates with Leptos's real toolchain:
//! - **CSR mode**: Uses `trunk serve` (the standard Leptos/Yew dev server)
//! - **SSR mode**: Uses `cargo leptos watch`
//!
//! # Usage
//!
//! ```ignore
//! use ferridriver_ct_leptos::LeptosComponentTest;
//!
//! #[tokio::test]
//! async fn test_counter() {
//!     let ct = LeptosComponentTest::new("./examples/ct-leptos")
//!         .csr()      // or .ssr()
//!         .start()
//!         .await
//!         .unwrap();
//!
//!     let page = ct.new_page().await.unwrap();
//!     page.locator("#inc").click().await.unwrap();
//!     let count = page.locator("#count").text_content().await.unwrap();
//!     assert_eq!(count.unwrap(), "1");
//!
//!     ct.stop().await;
//! }
//! ```

use std::path::{Path, PathBuf};

use ferridriver_test::ct::devserver::{DevServer, DevServerConfig};

/// Leptos rendering mode.
#[derive(Debug, Clone, Copy, Default)]
pub enum LeptosMode {
  /// Client-side rendering via `trunk serve`.
  #[default]
  Csr,
  /// Server-side rendering via `cargo leptos watch`.
  Ssr,
}

/// Builder for Leptos component tests.
pub struct LeptosComponentTest {
  project_dir: PathBuf,
  mode: LeptosMode,
}

/// A running Leptos component test environment.
pub struct LeptosTestEnv {
  dev_server: DevServer,
  browser: ferridriver::Browser,
}

impl LeptosComponentTest {
  /// Create a new Leptos CT builder for the given project directory.
  /// The directory must contain a `Cargo.toml` with Leptos dependencies.
  pub fn new(project_dir: impl Into<PathBuf>) -> Self {
    Self {
      project_dir: project_dir.into(),
      mode: LeptosMode::default(),
    }
  }

  /// Use CSR mode (trunk serve). This is the default.
  #[must_use]
  pub fn csr(mut self) -> Self {
    self.mode = LeptosMode::Csr;
    self
  }

  /// Use SSR mode (cargo leptos watch).
  #[must_use]
  pub fn ssr(mut self) -> Self {
    self.mode = LeptosMode::Ssr;
    self
  }

  /// Start the dev server and launch a browser.
  ///
  /// # Errors
  ///
  /// Returns an error if trunk/cargo-leptos is not installed,
  /// the project fails to build, or the browser fails to launch.
  pub async fn start(self) -> Result<LeptosTestEnv, String> {
    let config = match self.mode {
      LeptosMode::Csr => DevServerConfig::trunk(&self.project_dir),
      LeptosMode::Ssr => DevServerConfig::cargo_leptos(&self.project_dir),
    };

    let dev_server = ferridriver_test::ct::devserver::start(&config).await?;

    let browser = ferridriver::Browser::launch(ferridriver::options::LaunchOptions::default())
      .await
      .map_err(|e| format!("browser launch: {e}"))?;

    Ok(LeptosTestEnv { dev_server, browser })
  }
}

impl LeptosTestEnv {
  /// The dev server URL.
  #[must_use]
  pub fn url(&self) -> &str {
    self.dev_server.url()
  }

  /// Create a new page already navigated to the component.
  pub async fn new_page(&self) -> Result<ferridriver::Page, String> {
    self.browser.new_page_with_url(self.dev_server.url()).await
  }

  /// Stop everything.
  pub async fn stop(self) {
    let _ = self.browser.close().await;
    self.dev_server.stop().await;
  }
}

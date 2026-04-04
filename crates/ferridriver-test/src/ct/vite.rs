//! Vite-based component testing for JS frameworks (React, Vue, Svelte, Solid).
//!
//! Manages a Vite dev server that bundles and serves component test files.
//! Each test mounts a component into a minimal HTML shell, then interacts
//! via the ferridriver Page API.
//!
//! Flow:
//! 1. `ViteServer::start()` spawns `npx vite` (or `bunx vite`) with config
//! 2. Discovers the dev server URL from stdout
//! 3. Browser navigates to the component test page
//! 4. mount() renders the component into `#app`
//! 5. Test interacts via Page API
//!
//! Supported frameworks:
//! - React (via `@vitejs/plugin-react`)
//! - Vue (via `@vitejs/plugin-vue`)
//! - Svelte (via `@sveltejs/vite-plugin-svelte`)
//! - Solid (via `vite-plugin-solid`)

use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Supported JS frameworks for component testing.
#[derive(Debug, Clone, Copy)]
pub enum JsFramework {
  React,
  Vue,
  Svelte,
  Solid,
}

impl JsFramework {
  /// The mount code template for each framework.
  /// `{component}` is replaced with the import path.
  /// `{props}` is replaced with the serialized props object.
  #[must_use]
  pub fn mount_code(&self) -> &'static str {
    match self {
      Self::React => r#"
        import React from 'react';
        import { createRoot } from 'react-dom/client';
        window.__ferriMount = (Component, props) => {
          const root = createRoot(document.getElementById('app'));
          root.render(React.createElement(Component, props));
          document.body.setAttribute('data-mounted', 'true');
        };
      "#,
      Self::Vue => r#"
        import { createApp } from 'vue';
        window.__ferriMount = (Component, props) => {
          const app = createApp(Component, props);
          app.mount('#app');
          document.body.setAttribute('data-mounted', 'true');
        };
      "#,
      Self::Svelte => r#"
        window.__ferriMount = (Component, props) => {
          new Component({ target: document.getElementById('app'), props });
          document.body.setAttribute('data-mounted', 'true');
        };
      "#,
      Self::Solid => r#"
        import { render } from 'solid-js/web';
        window.__ferriMount = (Component, props) => {
          render(() => Component(props), document.getElementById('app'));
          document.body.setAttribute('data-mounted', 'true');
        };
      "#,
    }
  }

  /// Framework name for display.
  #[must_use]
  pub fn name(&self) -> &'static str {
    match self {
      Self::React => "react",
      Self::Vue => "vue",
      Self::Svelte => "svelte",
      Self::Solid => "solid",
    }
  }
}

/// Manages a Vite dev server for component testing.
pub struct ViteServer {
  /// The dev server URL (e.g. `http://localhost:5173`).
  url: String,
  /// The Vite process.
  child: tokio::process::Child,
  /// Project root.
  project_dir: PathBuf,
}

impl ViteServer {
  /// Start a Vite dev server in the given project directory.
  ///
  /// Assumes `vite` is installed in the project (`node_modules/.bin/vite` or global).
  /// Discovers the URL from Vite's stdout (e.g. `Local: http://localhost:5173/`).
  ///
  /// # Errors
  ///
  /// Returns an error if Vite fails to start or URL discovery times out.
  pub async fn start(project_dir: &Path) -> Result<Self, String> {
    // Try bunx first, fall back to npx.
    let (cmd, args) = if which("bunx") {
      ("bunx", vec!["--bun", "vite", "--host"])
    } else {
      ("npx", vec!["vite", "--host"])
    };

    let mut child = tokio::process::Command::new(cmd)
      .args(&args)
      .current_dir(project_dir)
      .stdout(Stdio::piped())
      .stderr(Stdio::piped())
      .spawn()
      .map_err(|e| format!("spawn vite: {e}"))?;

    // Read stdout to find the URL.
    let stdout = child.stdout.take().ok_or("no stdout from vite")?;
    let url = discover_vite_url(stdout).await?;

    Ok(Self {
      url,
      child,
      project_dir: project_dir.to_path_buf(),
    })
  }

  /// The base URL of the Vite dev server.
  #[must_use]
  pub fn url(&self) -> &str {
    &self.url
  }

  /// Stop the Vite dev server.
  pub async fn stop(mut self) {
    let _ = self.child.kill().await;
  }
}

/// Read Vite's stdout line by line until we find the dev server URL.
async fn discover_vite_url(
  stdout: tokio::process::ChildStdout,
) -> Result<String, String> {
  use tokio::io::{AsyncBufReadExt, BufReader};

  let reader = BufReader::new(stdout);
  let mut lines = reader.lines();
  let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);

  while let Ok(Some(line)) = tokio::time::timeout_at(
    deadline,
    lines.next_line(),
  )
  .await
  .map_err(|_| "timeout waiting for Vite URL".to_string())?
  {
    // Vite prints: "  ➜  Local:   http://localhost:5173/"
    let trimmed = line.trim();
    if let Some(url_start) = trimmed.find("http://") {
      let url = trimmed[url_start..].trim_end_matches('/').to_string();
      return Ok(url);
    }
    if let Some(url_start) = trimmed.find("https://") {
      let url = trimmed[url_start..].trim_end_matches('/').to_string();
      return Ok(url);
    }
  }

  Err("Vite exited without providing a URL".into())
}

/// Generate a component test entry file that mounts a component.
///
/// This file is served by Vite. It imports the component, calls `__ferriMount()`,
/// and the test framework's `mount()` evaluates this in the browser.
///
/// Returns the entry file content.
pub fn generate_entry(
  framework: JsFramework,
  component_import: &str,
  props_json: Option<&str>,
) -> String {
  let props = props_json.unwrap_or("{}");
  format!(
    r#"
{mount_code}
import Component from '{component_import}';
window.__ferriMount(Component, {props});
"#,
    mount_code = framework.mount_code(),
    component_import = component_import,
    props = props,
  )
}

/// Check if a command exists on PATH.
fn which(cmd: &str) -> bool {
  std::process::Command::new("which")
    .arg(cmd)
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .is_ok_and(|s| s.success())
}

/// High-level: start Vite, generate entry, return URL for the component.
///
/// The caller should navigate the browser to this URL and wait for `[data-mounted]`.
pub async fn mount_vite_component(
  project_dir: &Path,
  framework: JsFramework,
  component_path: &str,
  props_json: Option<&str>,
) -> Result<(String, ViteServer), String> {
  // Write the entry file into the project.
  let entry = generate_entry(framework, component_path, props_json);
  let entry_path = project_dir.join("__ferri_ct_entry.tsx");
  std::fs::write(&entry_path, &entry).map_err(|e| format!("write entry: {e}"))?;

  // Write an index.html that loads the entry.
  let html = super::server::vite_html_wrapper("/__ferri_ct_entry.tsx");
  let html_path = project_dir.join("__ferri_ct_index.html");
  std::fs::write(&html_path, &html).map_err(|e| format!("write html: {e}"))?;

  let server = ViteServer::start(project_dir).await?;
  let url = format!("{}/__ferri_ct_index.html", server.url());

  Ok((url, server))
}

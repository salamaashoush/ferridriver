//! Generic dev server manager: spawns a process, discovers its URL, stops it.
//!
//! Works with any dev server that prints a URL to stdout/stderr:
//! - Vite: `bunx vite` / `npx vite`
//! - Trunk: `trunk serve`
//! - Dioxus: `dx serve`
//! - cargo-leptos: `cargo leptos watch`
//! - Any custom command

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};

/// Configuration for launching a dev server.
#[derive(Debug, Clone)]
pub struct DevServerConfig {
  /// The command to run (e.g. "trunk", "dx", "npx", "bunx").
  pub cmd: String,
  /// Arguments (e.g. ["serve", "--port", "0"]).
  pub args: Vec<String>,
  /// Working directory (the project root).
  pub cwd: PathBuf,
  /// Timeout for URL discovery (seconds).
  pub timeout_secs: u64,
}

/// Presets for common framework dev servers.
impl DevServerConfig {
  /// Vite dev server (uses bun if available, falls back to npx).
  pub fn vite(project_dir: &Path) -> Self {
    let (cmd, args) = if which("bunx") {
      ("bunx".into(), vec!["--bun".into(), "vite".into()])
    } else {
      ("npx".into(), vec!["vite".into()])
    };
    Self {
      cmd,
      args,
      cwd: project_dir.into(),
      timeout_secs: 30,
    }
  }

  /// Trunk dev server (Leptos CSR, Yew).
  pub fn trunk(project_dir: &Path) -> Self {
    Self {
      cmd: "trunk".into(),
      args: vec!["serve".into()],
      cwd: project_dir.into(),
      timeout_secs: 60,
    }
  }

  /// Dioxus dev server.
  pub fn dioxus(project_dir: &Path) -> Self {
    Self {
      cmd: "dx".into(),
      args: vec!["serve".into()],
      cwd: project_dir.into(),
      timeout_secs: 60,
    }
  }

  /// cargo-leptos (Leptos SSR).
  pub fn cargo_leptos(project_dir: &Path) -> Self {
    Self {
      cmd: "cargo".into(),
      args: vec!["leptos".into(), "watch".into()],
      cwd: project_dir.into(),
      timeout_secs: 120,
    }
  }
}

/// A running dev server.
pub struct DevServer {
  url: String,
  child: tokio::process::Child,
  /// Temporary files created by the CT framework (cleaned up on stop).
  pub generated_files: Vec<PathBuf>,
}

impl DevServer {
  /// The dev server URL.
  #[must_use]
  pub fn url(&self) -> &str {
    &self.url
  }

  /// Stop the dev server and clean up generated files.
  pub async fn stop(mut self) {
    let _ = self.child.kill().await;
    let _ = self.child.wait().await;
    for f in &self.generated_files {
      let _ = std::fs::remove_file(f);
    }
  }
}

/// Start a dev server and discover its URL from stdout/stderr.
///
/// # Errors
///
/// Returns an error if the command fails to spawn, the URL is not found
/// within the timeout, or the process exits early.
pub async fn start(config: &DevServerConfig) -> Result<DevServer, String> {
  let mut child = tokio::process::Command::new(&config.cmd)
    .args(&config.args)
    .current_dir(&config.cwd)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .map_err(|e| {
      format!(
        "failed to spawn `{}` — is it installed? ({e})\nInstall: {}",
        config.cmd,
        install_hint(&config.cmd),
      )
    })?;

  let stdout = child.stdout.take();
  let stderr = child.stderr.take();

  let url = discover_url(stdout, stderr, config.timeout_secs).await?;

  Ok(DevServer {
    url,
    child,
    generated_files: Vec::new(),
  })
}

/// Read stdout + stderr concurrently looking for an HTTP URL.
async fn discover_url(
  stdout: Option<tokio::process::ChildStdout>,
  stderr: Option<tokio::process::ChildStderr>,
  timeout_secs: u64,
) -> Result<String, String> {
  let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();

  if let Some(out) = stdout {
    let tx = tx.clone();
    tokio::spawn(async move {
      let mut lines = BufReader::new(out).lines();
      while let Ok(Some(line)) = lines.next_line().await {
        let _ = tx.send(line);
      }
    });
  }
  if let Some(err) = stderr {
    let tx = tx.clone();
    tokio::spawn(async move {
      let mut lines = BufReader::new(err).lines();
      while let Ok(Some(line)) = lines.next_line().await {
        let _ = tx.send(line);
      }
    });
  }
  drop(tx);

  let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

  loop {
    let line = tokio::select! {
      line = rx.recv() => match line {
        Some(l) => l,
        None => return Err("dev server exited without providing a URL".into()),
      },
      () = tokio::time::sleep_until(deadline) => {
        return Err(format!("timeout ({timeout_secs}s) waiting for dev server URL"));
      }
    };

    if let Some(url) = extract_url(&line) {
      return Ok(url);
    }
  }
}

/// Try to extract an HTTP URL from a log line.
fn extract_url(line: &str) -> Option<String> {
  let trimmed = line.trim();

  for prefix in ["http://127.0.0.1:", "http://localhost:", "http://0.0.0.0:"] {
    if let Some(start) = trimmed.find(prefix) {
      let url_part = &trimmed[start..];
      let end = url_part.find(|c: char| c.is_whitespace()).unwrap_or(url_part.len());
      let url = url_part[..end].trim_end_matches('/');
      return Some(url.replace("0.0.0.0", "127.0.0.1"));
    }
  }

  // Trunk-specific: "server listening at 0.0.0.0:8080"
  if trimmed.contains("listening at") {
    if let Some(addr_start) = trimmed.rfind("at ") {
      let addr = trimmed[addr_start + 3..].trim().replace("0.0.0.0", "127.0.0.1");
      if addr.contains(':') {
        return Some(format!("http://{addr}"));
      }
    }
  }

  None
}

fn install_hint(cmd: &str) -> &'static str {
  match cmd {
    "trunk" => "cargo install trunk",
    "dx" => "cargo install dioxus-cli",
    "cargo" => "cargo install cargo-leptos",
    "bunx" | "bun" => "curl -fsSL https://bun.sh/install | bash",
    "npx" | "npm" => "install Node.js from https://nodejs.org",
    _ => "(check framework docs)",
  }
}

fn which(cmd: &str) -> bool {
  std::process::Command::new("which")
    .arg(cmd)
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .status()
    .is_ok_and(|s| s.success())
}

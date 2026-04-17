//! `ArtifactsJs`: wrapper that gives scripts a dedicated write-scoped
//! directory for outputs like screenshots, PDFs, traces, downloaded bodies.
//!
//! Separate from `fs` (which is rooted at `script_root` and is primarily
//! for reading fixtures + importing modules). `artifacts` is rooted at
//! `artifacts_root` and is primarily for writing — keeps outputs out of
//! your source tree.
//!
//! Same sandbox rules as `fs`: absolute paths, `..`, and symlink escapes
//! are rejected. Names are resolved relative to the artifacts root.

use std::sync::Arc;

use rquickjs::JsLifetime;
use rquickjs::class::Trace;

use crate::error::ScriptError;
use crate::fs::PathSandbox;

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Artifacts")]
pub struct ArtifactsJs {
  #[qjs(skip_trace)]
  sandbox: Arc<PathSandbox>,
}

impl ArtifactsJs {
  #[must_use]
  pub fn new(sandbox: Arc<PathSandbox>) -> Self {
    Self { sandbox }
  }

  fn io_err(op: &'static str, msg: String) -> rquickjs::Error {
    rquickjs::Error::new_from_js_message("artifacts", op, msg)
  }

  fn sandbox_err(err: &ScriptError) -> rquickjs::Error {
    rquickjs::Error::new_from_js_message("artifacts", "sandbox", err.message.clone())
  }
}

#[rquickjs::methods]
impl ArtifactsJs {
  /// Absolute path to the artifacts root (read-only).
  #[qjs(get, rename = "root")]
  pub fn root(&self) -> String {
    self.sandbox.root().to_string_lossy().into_owned()
  }

  /// Write UTF-8 text to `name`. Creates parent directories as needed.
  #[qjs(rename = "write")]
  pub async fn write(&self, name: String, contents: String) -> rquickjs::Result<()> {
    let sb = self.sandbox.clone();
    let resolved = sb.resolve_write(&name).map_err(|e| Self::sandbox_err(&e))?;
    tokio::fs::write(&resolved, contents)
      .await
      .map_err(|e| Self::io_err("write", e.to_string()))
  }

  /// Write raw bytes to `name`. Creates parent directories as needed.
  /// Use this for screenshots, PDFs, downloads, or any binary payload.
  #[qjs(rename = "writeBytes")]
  pub async fn write_bytes(&self, name: String, bytes: Vec<u8>) -> rquickjs::Result<()> {
    let sb = self.sandbox.clone();
    let resolved = sb.resolve_write(&name).map_err(|e| Self::sandbox_err(&e))?;
    tokio::fs::write(&resolved, bytes)
      .await
      .map_err(|e| Self::io_err("writeBytes", e.to_string()))
  }

  /// Read `name` as UTF-8 text.
  #[qjs(rename = "read")]
  pub async fn read(&self, name: String) -> rquickjs::Result<String> {
    let sb = self.sandbox.clone();
    let resolved = sb.resolve_read(&name).map_err(|e| Self::sandbox_err(&e))?;
    tokio::fs::read_to_string(&resolved)
      .await
      .map_err(|e| Self::io_err("read", e.to_string()))
  }

  /// Read `name` as raw bytes (Uint8Array in JS).
  #[qjs(rename = "readBytes")]
  pub async fn read_bytes(&self, name: String) -> rquickjs::Result<Vec<u8>> {
    let sb = self.sandbox.clone();
    let resolved = sb.resolve_read(&name).map_err(|e| Self::sandbox_err(&e))?;
    tokio::fs::read(&resolved)
      .await
      .map_err(|e| Self::io_err("readBytes", e.to_string()))
  }

  /// List entries at the artifacts root (or a subdirectory).
  #[qjs(rename = "list")]
  pub async fn list(&self) -> rquickjs::Result<Vec<String>> {
    let root = self.sandbox.root().to_path_buf();
    let mut entries = tokio::fs::read_dir(&root)
      .await
      .map_err(|e| Self::io_err("list", e.to_string()))?;
    let mut names = Vec::new();
    while let Some(entry) = entries
      .next_entry()
      .await
      .map_err(|e| Self::io_err("list", e.to_string()))?
    {
      names.push(entry.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
  }

  /// List entries in a subdirectory of the artifacts root.
  #[qjs(rename = "readdir")]
  pub async fn readdir(&self, subpath: String) -> rquickjs::Result<Vec<String>> {
    let sb = self.sandbox.clone();
    let resolved = sb.resolve_read(&subpath).map_err(|e| Self::sandbox_err(&e))?;
    let mut entries = tokio::fs::read_dir(&resolved)
      .await
      .map_err(|e| Self::io_err("readdir", e.to_string()))?;
    let mut names = Vec::new();
    while let Some(entry) = entries
      .next_entry()
      .await
      .map_err(|e| Self::io_err("readdir", e.to_string()))?
    {
      names.push(entry.file_name().to_string_lossy().into_owned());
    }
    Ok(names)
  }

  /// True if `name` exists inside the artifacts root.
  ///
  /// Paths that would escape the root are treated as non-existent (no
  /// exception thrown) so probing code can discover presence without having
  /// to try/catch sandbox errors.
  #[qjs(rename = "exists")]
  pub async fn exists(&self, name: String) -> rquickjs::Result<bool> {
    match self.sandbox.resolve_read(&name) {
      Ok(resolved) => Ok(tokio::fs::try_exists(&resolved).await.unwrap_or(false)),
      Err(_) => Ok(false),
    }
  }

  /// Remove a file at `name`. Returns `false` if the file did not exist.
  #[qjs(rename = "remove")]
  pub async fn remove(&self, name: String) -> rquickjs::Result<bool> {
    let sb = self.sandbox.clone();
    let resolved = match sb.resolve_read(&name) {
      Ok(p) => p,
      Err(_) => return Ok(false),
    };
    match tokio::fs::remove_file(&resolved).await {
      Ok(()) => Ok(true),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
      Err(e) => Err(Self::io_err("remove", e.to_string())),
    }
  }
}

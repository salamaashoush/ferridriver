//! On-disk session registry.
//!
//! Each bound browser writes one descriptor file
//! `<cache>/ferridriver/sessions/<id>.json`. Any process can list live
//! sessions by reading that directory and probing each endpoint, and resolve
//! an id to its socket endpoint to reattach. This is the discovery mechanism
//! behind `ferridriver list` / `ferridriver attach <id>` / `-s <id>`.
//!
//! The descriptor is intentionally small and matches the data Playwright's
//! own browser registry exposes (title, endpoint, workspace dir, metadata) so
//! the CLI surface lines up, without copying its storage format.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;

/// A persisted record of one bound browser.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionDescriptor {
  /// The session id (also the descriptor file stem).
  pub id: String,
  /// Socket path (Unix domain socket / Windows named pipe) the session
  /// server listens on, or a `ws://` URL when bound over TCP.
  pub endpoint: String,
  /// PID of the process that owns the bound browser.
  pub pid: u32,
  /// Browser engine: `chromium`, `firefox`, or `webkit`.
  pub browser_name: String,
  /// Working directory associated with the session, for dashboards that
  /// group sessions by project. `None` when unset.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub workspace_dir: Option<String>,
  /// Arbitrary caller metadata echoed back by `list`. Mirrors Playwright's
  /// `metadata` bind option.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub metadata: Option<serde_json::Value>,
}

/// Handle to the session registry directory.
#[derive(Debug, Clone)]
pub struct Registry {
  dir: PathBuf,
}

impl Registry {
  /// Open the registry at the default location
  /// (`<user-cache>/ferridriver/sessions`), creating it if needed.
  ///
  /// `FERRIDRIVER_SESSION_DIR` overrides the location — used by tests and by
  /// operators who want sessions in a non-default place.
  ///
  /// # Errors
  ///
  /// Returns [`crate::SessionError::Io`] if the directory cannot be created.
  pub fn open() -> Result<Self> {
    let dir = match std::env::var_os("FERRIDRIVER_SESSION_DIR") {
      Some(custom) => PathBuf::from(custom),
      None => default_registry_dir(),
    };
    Self::open_at(dir)
  }

  /// Open the registry rooted at an explicit directory.
  ///
  /// # Errors
  ///
  /// Returns [`crate::SessionError::Io`] if the directory cannot be created.
  pub fn open_at(dir: impl Into<PathBuf>) -> Result<Self> {
    let dir = dir.into();
    std::fs::create_dir_all(&dir)?;
    Ok(Self { dir })
  }

  /// Directory backing this registry.
  #[must_use]
  pub fn dir(&self) -> &Path {
    &self.dir
  }

  fn path_for(&self, id: &str) -> PathBuf {
    self.dir.join(format!("{id}.json"))
  }

  /// Write (or overwrite) the descriptor for a session.
  ///
  /// # Errors
  ///
  /// Returns [`crate::SessionError::Json`] if the descriptor fails to serialize or
  /// [`crate::SessionError::Io`] on a write/rename failure.
  pub fn put(&self, descriptor: &SessionDescriptor) -> Result<()> {
    let path = self.path_for(&descriptor.id);
    let json = serde_json::to_vec_pretty(descriptor)?;
    // Write to a temp file then rename so a concurrent reader never observes
    // a half-written descriptor.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
  }

  /// Read the descriptor for `id`, or `None` if no such file exists.
  ///
  /// # Errors
  ///
  /// Returns [`crate::SessionError::Json`] if the file is malformed or
  /// [`crate::SessionError::Io`] on a read failure other than "not found".
  pub fn get(&self, id: &str) -> Result<Option<SessionDescriptor>> {
    let path = self.path_for(id);
    match std::fs::read(&path) {
      Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
      Err(e) => Err(e.into()),
    }
  }

  /// Remove the descriptor for `id`. Missing files are not an error
  /// (idempotent — `unbind` after a crash still succeeds).
  ///
  /// # Errors
  ///
  /// Returns [`crate::SessionError::Io`] on a delete failure other than "not found".
  pub fn remove(&self, id: &str) -> Result<()> {
    match std::fs::remove_file(self.path_for(id)) {
      Ok(()) => Ok(()),
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
      Err(e) => Err(e.into()),
    }
  }

  /// All descriptors currently on disk. Unreadable / malformed files are
  /// skipped (a half-written file from a racing writer, or a stale format)
  /// rather than failing the whole listing.
  ///
  /// # Errors
  ///
  /// Returns [`crate::SessionError::Io`] if the registry directory cannot be read
  /// (a missing directory yields an empty list, not an error).
  pub fn list(&self) -> Result<Vec<SessionDescriptor>> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&self.dir) {
      Ok(e) => e,
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
      Err(e) => return Err(e.into()),
    };
    for entry in entries.flatten() {
      let path = entry.path();
      if path.extension().and_then(|e| e.to_str()) != Some("json") {
        continue;
      }
      if let Ok(bytes) = std::fs::read(&path)
        && let Ok(descriptor) = serde_json::from_slice::<SessionDescriptor>(&bytes)
      {
        out.push(descriptor);
      }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
  }
}

/// Default registry directory: `<user-cache>/ferridriver/sessions`, falling
/// back to the system temp dir when no cache dir is resolvable (headless CI).
fn default_registry_dir() -> PathBuf {
  dirs::cache_dir()
    .unwrap_or_else(std::env::temp_dir)
    .join("ferridriver")
    .join("sessions")
}

#[cfg(test)]
mod tests {
  use super::*;

  fn descriptor(id: &str) -> SessionDescriptor {
    SessionDescriptor {
      id: id.to_string(),
      endpoint: format!("/tmp/ferri-{id}.sock"),
      pid: 4242,
      browser_name: "chromium".into(),
      workspace_dir: Some("/work/proj".into()),
      metadata: Some(serde_json::json!({ "owner": "agent" })),
    }
  }

  #[test]
  fn put_get_remove_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let reg = Registry::open_at(tmp.path()).unwrap();
    assert!(reg.get("a").unwrap().is_none());

    let d = descriptor("a");
    reg.put(&d).unwrap();
    assert_eq!(reg.get("a").unwrap().as_ref(), Some(&d));

    reg.remove("a").unwrap();
    assert!(reg.get("a").unwrap().is_none());
    // Idempotent remove.
    reg.remove("a").unwrap();
  }

  #[test]
  fn list_sorts_and_skips_malformed() {
    let tmp = tempfile::tempdir().unwrap();
    let reg = Registry::open_at(tmp.path()).unwrap();
    reg.put(&descriptor("zeta")).unwrap();
    reg.put(&descriptor("alpha")).unwrap();
    // A junk file in the dir must not break listing.
    std::fs::write(tmp.path().join("garbage.json"), b"not json").unwrap();
    std::fs::write(tmp.path().join("ignore.txt"), b"{}").unwrap();

    let ids: Vec<_> = reg.list().unwrap().into_iter().map(|d| d.id).collect();
    assert_eq!(ids, vec!["alpha", "zeta"]);
  }

  #[test]
  fn put_is_atomic_via_tmp_rename() {
    let tmp = tempfile::tempdir().unwrap();
    let reg = Registry::open_at(tmp.path()).unwrap();
    reg.put(&descriptor("x")).unwrap();
    // No leftover .tmp file after a successful put.
    let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
      .unwrap()
      .flatten()
      .filter(|e| e.path().to_string_lossy().ends_with(".tmp"))
      .collect();
    assert!(leftovers.is_empty(), "tmp file leaked: {leftovers:?}");
  }
}

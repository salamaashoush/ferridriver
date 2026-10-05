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
//!
//! Attaching to a session means running code in the owner's process, so the
//! directory — which also holds each session's socket — is owner-only.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Result, SessionError};
use sha2::{Digest as _, Sha256};

/// A persisted record of one bound browser.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionDescriptor {
  /// The session id (also the descriptor file stem).
  pub id: String,
  /// Socket path (Unix domain socket / Windows named pipe) the session
  /// server listens on, or a `ws://` URL when bound over TCP.
  pub endpoint: String,
  #[serde(default)]
  pub generation: String,
  /// PID of the process that owns the bound browser.
  pub pid: u32,
  /// Reported browser product, normalized for Chromium, Firefox, Safari and `WebKit`.
  pub browser_name: String,
  /// ferridriver version of the process that bound the session. A client
  /// speaking a different build's wire gets a "reopen the session" message
  /// instead of a decode failure. Empty when read from a descriptor written
  /// before the field existed.
  #[serde(default)]
  pub version: String,
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
    if let Ok(metadata) = std::fs::symlink_metadata(&dir)
      && (!metadata.is_dir() || metadata.file_type().is_symlink())
    {
      return Err(
        std::io::Error::new(
          std::io::ErrorKind::InvalidInput,
          "session registry must be a directory, not a symlink",
        )
        .into(),
      );
    }
    std::fs::create_dir_all(&dir)?;
    restrict_to_owner(&dir)?;
    Ok(Self { dir })
  }

  /// Directory backing this registry.
  #[must_use]
  pub fn dir(&self) -> &Path {
    &self.dir
  }

  fn path_for(&self, id: &str) -> PathBuf {
    self.dir.join(format!("{}.json", storage_key(id)))
  }

  /// Publish a descriptor without replacing another session.
  ///
  /// # Errors
  /// Returns a filesystem or serialization error, or an error if the id is already claimed.
  pub fn put(&self, descriptor: &SessionDescriptor) -> Result<()> {
    let mut claim = self.claim(&descriptor.id)?;
    claim.publish(descriptor)?;
    claim.published = false;
    Ok(())
  }

  fn lock(&self, id: &str) -> Result<std::fs::File> {
    let path = self.dir.join(format!("{}.lock", storage_key(id)));
    if let Ok(metadata) = std::fs::symlink_metadata(&path)
      && (!metadata.is_file() || metadata.file_type().is_symlink())
    {
      return Err(SessionError::Dispatch(
        "session claim path is not a regular file".to_owned(),
      ));
    }
    let file = std::fs::OpenOptions::new()
      .create(true)
      .truncate(false)
      .read(true)
      .write(true)
      .open(path)?;
    file.try_lock().map_err(|error| match error {
      std::fs::TryLockError::WouldBlock => SessionError::Dispatch(format!("session '{id}' is already claimed")),
      std::fs::TryLockError::Error(error) => error.into(),
    })?;
    Ok(file)
  }

  pub(crate) fn claim(&self, id: &str) -> Result<RegistryClaim> {
    let lock = self.lock(id)?;
    if self.get(id)?.is_some() {
      return Err(SessionError::Dispatch(format!(
        "session '{id}' already has a descriptor"
      )));
    }
    Ok(RegistryClaim {
      registry: self.clone(),
      id: id.to_owned(),
      _lock: lock,
      published: false,
    })
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
      Ok(bytes) => {
        let descriptor: SessionDescriptor = serde_json::from_slice(&bytes)?;
        if descriptor.id != id {
          return Err(SessionError::Dispatch(
            "session descriptor identity does not match its storage key".to_owned(),
          ));
        }
        Ok(Some(descriptor))
      },
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
    let _claim = self.lock(id)?;
    self.remove_claimed(id)
  }

  fn remove_claimed(&self, id: &str) -> Result<()> {
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

pub(crate) struct RegistryClaim {
  registry: Registry,
  id: String,
  _lock: std::fs::File,
  published: bool,
}

impl RegistryClaim {
  pub(crate) fn publish(&mut self, descriptor: &SessionDescriptor) -> Result<()> {
    use std::io::Write as _;
    if descriptor.id != self.id {
      return Err(SessionError::Dispatch(
        "session publication does not match its claim".to_owned(),
      ));
    }
    let mut file = tempfile::NamedTempFile::new_in(self.registry.dir())?;
    file.write_all(&serde_json::to_vec_pretty(descriptor)?)?;
    file
      .persist_noclobber(self.registry.path_for(&self.id))
      .map_err(|error| error.error)?;
    self.published = true;
    Ok(())
  }

  pub(crate) fn remove(&mut self) -> Result<()> {
    if self.published {
      self.registry.remove_claimed(&self.id)?;
      self.published = false;
    }
    Ok(())
  }
}

impl Drop for RegistryClaim {
  fn drop(&mut self) {
    if let Err(error) = self.remove() {
      tracing::warn!(%error, session = %self.id, "session descriptor cleanup failed");
    }
  }
}

pub(crate) fn storage_key(id: &str) -> String {
  if !id.is_empty()
    && id.len() <= 64
    && id
      .bytes()
      .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_'))
  {
    id.to_owned()
  } else {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut key = String::with_capacity(65);
    key.push('~');
    for byte in Sha256::digest(id.as_bytes()) {
      key.push(char::from(HEX[usize::from(byte >> 4)]));
      key.push(char::from(HEX[usize::from(byte & 15)]));
    }
    key
  }
}

/// Default registry directory: `<user-cache>/ferridriver/sessions`, falling
/// back to the system temp dir when no cache dir is resolvable (headless CI).
/// Restrict `dir` to its owner (`0700`). A session socket lives inside it and
/// a client that reaches that socket can run arbitrary code in the owning
/// process, so directory traversal is the access boundary.
#[cfg(unix)]
fn restrict_to_owner(dir: &Path) -> Result<()> {
  use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
  let metadata = std::fs::symlink_metadata(dir)?;
  if !metadata.is_dir() || metadata.uid() != rustix::process::geteuid().as_raw() {
    return Err(
      std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "session registry must be owned by the current user",
      )
      .into(),
    );
  }
  std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
  Ok(())
}

#[cfg(not(unix))]
fn restrict_to_owner(_dir: &Path) -> Result<()> {
  // Windows sessions bind loopback TCP rather than a socket file in this
  // directory; the descriptor inherits the user profile's ACL.
  Ok(())
}

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
      generation: "test-generation".into(),
      pid: 4242,
      browser_name: "chromium".into(),
      version: crate::WIRE_VERSION.to_string(),
      workspace_dir: Some("/work/proj".into()),
      metadata: Some(serde_json::json!({ "owner": "agent" })),
    }
  }

  #[test]
  fn session_titles_cannot_escape_or_collide_on_case_insensitive_filesystems() {
    let tmp = tempfile::tempdir().unwrap();
    let reg = Registry::open_at(tmp.path().join("registry")).unwrap();
    let outside = tmp.path().join("outside.json");
    std::fs::write(&outside, "sashoush sentinel").unwrap();
    for id in ["../outside", "/absolute/session", "Safari", "safari", "", "a/b"] {
      reg.put(&descriptor(id)).unwrap();
      assert_eq!(reg.get(id).unwrap().unwrap().id, id);
      assert_eq!(reg.path_for(id).parent(), Some(reg.dir()));
    }
    assert_ne!(
      storage_key("Safari").to_lowercase(),
      storage_key("safari").to_lowercase()
    );
    assert_eq!(reg.list().unwrap().len(), 6);
    reg.remove("../outside").unwrap();
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "sashoush sentinel");
  }

  #[test]
  fn a_live_claim_prevents_replacement_and_external_removal() {
    let tmp = tempfile::tempdir().unwrap();
    let reg = Registry::open_at(tmp.path()).unwrap();
    let mut claim = reg.claim("safari").unwrap();
    claim.publish(&descriptor("safari")).unwrap();
    assert!(reg.claim("safari").is_err());
    assert!(reg.put(&descriptor("safari")).is_err());
    assert!(reg.remove("safari").is_err());
    assert!(reg.get("safari").unwrap().is_some());
    drop(claim);
    assert!(reg.get("safari").unwrap().is_none());
    reg.put(&descriptor("safari")).unwrap();
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

//! A rooted output directory: where `artifacts` writes, and the record of
//! what it wrote.
//!
//! This is an ANCHOR, not a boundary. A relative name is resolved against
//! the root, an absolute path is taken as written, and parent directories
//! are created on the way. Nothing here confines a script.
//!
//! The write record is the load-bearing part: an output directory under a
//! size ceiling has to know which files the CURRENT run produced, or the
//! sweep would delete the screenshot a caller is being handed the path to.

use std::path::{Path, PathBuf};

use crate::error::ScriptError;

/// A directory scripts write into, plus every path handed out for writing.
///
/// Cheap to clone — the canonicalised root and a shared handle to the
/// write record. A clone is the same directory, so it is the same record.
#[derive(Debug, Clone)]
pub struct OutputDir {
  root: PathBuf,
  written: std::sync::Arc<std::sync::Mutex<std::collections::BTreeSet<PathBuf>>>,
}

impl OutputDir {
  /// Root the directory at `root`, canonicalising once.
  ///
  /// # Errors
  ///
  /// Returns an error if `root` does not resolve to a directory.
  pub fn new(root: impl AsRef<Path>) -> Result<Self, ScriptError> {
    let root = root.as_ref();
    let canonical = std::fs::canonicalize(root)
      .map_err(|e| ScriptError::internal(format!("{} is not a valid directory: {e}", root.display())))?;
    if !canonical.is_dir() {
      return Err(ScriptError::internal(format!(
        "{} is not a directory",
        canonical.display()
      )));
    }
    Ok(Self {
      root: canonical,
      written: std::sync::Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new())),
    })
  }

  /// The directory a relative name resolves against.
  #[must_use]
  pub fn root(&self) -> &Path {
    &self.root
  }

  /// Every path [`Self::resolve_write`] has handed out.
  #[must_use]
  pub fn written(&self) -> std::collections::BTreeSet<PathBuf> {
    self
      .written
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
  }

  /// Where `name` points, without touching the filesystem.
  ///
  /// # Errors
  ///
  /// Returns an error only for an empty name.
  pub fn resolve(&self, name: &str) -> Result<PathBuf, ScriptError> {
    if name.is_empty() {
      return Err(ScriptError::internal("empty path"));
    }
    let path = Path::new(name);
    Ok(if path.is_absolute() {
      path.to_path_buf()
    } else {
      self.root.join(path)
    })
  }

  /// Resolve `name` for reading, following it to a real file.
  ///
  /// # Errors
  ///
  /// Returns an error if the path cannot be resolved.
  pub fn resolve_read(&self, name: &str) -> Result<PathBuf, ScriptError> {
    let full = self.resolve(name)?;
    std::fs::canonicalize(&full).map_err(|e| ScriptError::internal(format!("cannot resolve {}: {e}", full.display())))
  }

  /// Resolve `name` for writing, creating parent directories and
  /// recording the target.
  ///
  /// # Errors
  ///
  /// Returns an error if a parent directory cannot be created.
  pub fn resolve_write(&self, name: &str) -> Result<PathBuf, ScriptError> {
    let full = self.resolve(name)?;
    if let Some(parent) = full.parent()
      && !parent.as_os_str().is_empty()
      && !parent.exists()
    {
      std::fs::create_dir_all(parent)
        .map_err(|e| ScriptError::internal(format!("cannot create parent directory: {e}")))?;
    }
    if let Ok(mut written) = self.written.lock() {
      written.insert(full.clone());
    }
    Ok(full)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn tmp_dir() -> (tempfile::TempDir, OutputDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = OutputDir::new(tmp.path()).expect("output dir");
    (tmp, dir)
  }

  #[test]
  fn a_relative_name_is_anchored_at_the_root() {
    let (tmp, dir) = tmp_dir();
    std::fs::write(tmp.path().join("ok.txt"), b"hello").expect("write");
    let resolved = dir.resolve_read("ok.txt").expect("resolve");
    assert!(resolved.starts_with(dir.root()));
    assert_eq!(resolved.file_name().expect("name"), "ok.txt");
  }

  /// The paths the runner hands a spec — `testInfo.outputPath()`,
  /// `testInfo.snapshotPath()`, `download.path()` — are absolute and
  /// frequently outside whatever directory the process started in.
  #[test]
  fn an_absolute_path_outside_the_root_is_taken_as_written() {
    let (_tmp, dir) = tmp_dir();
    let outside = tempfile::tempdir().expect("tempdir");
    let write = outside.path().join("nested").join("out.txt");
    assert_eq!(dir.resolve_write(write.to_str().expect("utf8")).expect("write"), write);
    assert!(write.parent().expect("parent").is_dir(), "parents are created");
  }

  #[test]
  fn resolve_write_records_the_target_for_the_sweep() {
    let (tmp, dir) = tmp_dir();
    let resolved = dir.resolve_write("nested/deep/new.txt").expect("resolve");
    assert!(tmp.path().join("nested/deep").is_dir());
    assert!(dir.written().contains(&resolved));
  }

  #[test]
  fn an_empty_name_is_still_a_mistake() {
    let (_tmp, dir) = tmp_dir();
    assert!(dir.resolve("").is_err());
  }
}

//! Scoped filesystem module exposed to scripts as `fs`.
//!
//! Every path passed in from JS is validated against a root directory:
//!
//! 1. Absolute paths are accepted only when they already point inside the
//!    root (e.g. `testInfo.outputPath()`); they are reduced to their
//!    root-relative remainder. Everything else must be root-relative.
//! 2. Reject any `..` component in the requested path.
//! 3. Canonicalise the final path and verify the result stays inside the
//!    canonicalised root (rejects symlinks that escape the root).
//!
//! The canonicalisation happens at the parent directory for write operations
//! (the target file may not exist yet) and at the target itself for reads.

use std::path::{Component, Path, PathBuf};

use crate::error::ScriptError;

/// Enforces sandbox containment for paths used by the `fs` module.
///
/// Cheap to clone — only holds the canonicalised root.
#[derive(Debug, Clone)]
pub struct PathSandbox {
  root: PathBuf,
}

impl PathSandbox {
  /// Build a sandbox rooted at `root`. The root is canonicalised up front,
  /// so subsequent containment checks do not need to re-resolve it.
  pub fn new(root: impl AsRef<Path>) -> Result<Self, ScriptError> {
    let root = root.as_ref();
    let canonical = std::fs::canonicalize(root)
      .map_err(|e| ScriptError::internal(format!("script_root {} is not a valid directory: {e}", root.display())))?;
    if !canonical.is_dir() {
      return Err(ScriptError::internal(format!(
        "script_root {} is not a directory",
        canonical.display()
      )));
    }
    Ok(Self { root: canonical })
  }

  /// Root directory that all paths must stay inside.
  #[must_use]
  pub fn root(&self) -> &Path {
    &self.root
  }

  /// Validate a path for a **read** operation.
  ///
  /// The path must exist and, after canonicalisation, live under the root.
  pub fn resolve_read(&self, rel: &str) -> Result<PathBuf, ScriptError> {
    let candidate = self.syntactic_check(rel)?;
    let full = self.root.join(candidate);
    let canonical = std::fs::canonicalize(&full)
      .map_err(|e| ScriptError::sandbox(format!("fs: cannot resolve {}: {e}", full.display())))?;
    if !canonical.starts_with(&self.root) {
      return Err(ScriptError::sandbox(format!(
        "fs: path escapes script_root: {}",
        canonical.display()
      )));
    }
    Ok(canonical)
  }

  /// Validate a path for a **write** operation.
  ///
  /// The target file may not exist yet, so canonicalisation is applied to
  /// the parent directory; the final filename is appended unchanged and
  /// validated not to contain separators itself.
  pub fn resolve_write(&self, rel: &str) -> Result<PathBuf, ScriptError> {
    let candidate = self.syntactic_check(rel)?;
    let full = self.root.join(&candidate);
    let parent = full
      .parent()
      .ok_or_else(|| ScriptError::sandbox(format!("fs: path has no parent directory: {}", full.display())))?;
    if !parent.exists() {
      std::fs::create_dir_all(parent)
        .map_err(|e| ScriptError::sandbox(format!("fs: cannot create parent directory: {e}")))?;
    }
    let canonical_parent = std::fs::canonicalize(parent)
      .map_err(|e| ScriptError::sandbox(format!("fs: cannot resolve parent directory {}: {e}", parent.display())))?;
    if !canonical_parent.starts_with(&self.root) {
      return Err(ScriptError::sandbox(format!(
        "fs: parent directory escapes script_root: {}",
        canonical_parent.display()
      )));
    }
    let Some(name) = full.file_name() else {
      return Err(ScriptError::sandbox("fs: path has no filename"));
    };
    let target = canonical_parent.join(name);
    // The parent is canonicalised, but the final component is appended
    // unverified. If it is itself a symlink, `tokio::fs::write` would
    // follow it and clobber a file outside the sandbox. Reject it: the
    // sandbox never lets a script create symlinks, so a symlink here
    // was pre-seeded and is always an escape attempt.
    if let Ok(meta) = std::fs::symlink_metadata(&target)
      && meta.file_type().is_symlink()
    {
      return Err(ScriptError::sandbox(format!(
        "fs: refusing to write through symlink: {}",
        target.display()
      )));
    }
    Ok(target)
  }

  fn syntactic_check(&self, rel: &str) -> Result<PathBuf, ScriptError> {
    if rel.is_empty() {
      return Err(ScriptError::sandbox("fs: empty path"));
    }
    let mut path = Path::new(rel);
    if path.is_absolute() {
      // An absolute path is accepted only when it already points inside
      // the sandbox root (e.g. `testInfo.outputPath()` hands tests an
      // absolute path under the runner's cwd, Playwright-style). Reduce
      // it to the root-relative remainder and validate that like any
      // relative input.
      match path.strip_prefix(&self.root) {
        Ok(stripped) => path = stripped,
        Err(_) => {
          return Err(ScriptError::sandbox(format!(
            "fs: absolute path outside script_root: {rel}"
          )));
        },
      }
    }
    for component in path.components() {
      match component {
        Component::ParentDir => {
          return Err(ScriptError::sandbox(format!(
            "fs: path traversal (..) not allowed: {rel}"
          )));
        },
        Component::Prefix(_) | Component::RootDir => {
          return Err(ScriptError::sandbox(format!("fs: path must be relative: {rel}")));
        },
        Component::CurDir | Component::Normal(_) => {},
      }
    }
    Ok(path.to_path_buf())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn tmp_sandbox() -> (tempfile::TempDir, PathSandbox) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let sb = PathSandbox::new(tmp.path()).expect("sandbox");
    (tmp, sb)
  }

  #[test]
  fn rejects_absolute_path_outside_root() {
    let (_tmp, sb) = tmp_sandbox();
    assert!(sb.resolve_read("/etc/passwd").is_err());
    assert!(sb.resolve_write("/etc/out.txt").is_err());
  }

  #[test]
  fn accepts_absolute_path_inside_root() {
    let (tmp, sb) = tmp_sandbox();
    std::fs::write(tmp.path().join("inside.txt"), b"hello").unwrap();
    let abs_read = sb.root().join("inside.txt");
    let resolved = sb.resolve_read(abs_read.to_str().unwrap()).expect("read inside root");
    assert!(resolved.starts_with(sb.root()));
    let abs_write = sb.root().join("nested").join("out.txt");
    let resolved = sb
      .resolve_write(abs_write.to_str().unwrap())
      .expect("write inside root");
    assert!(resolved.starts_with(sb.root()));
  }

  #[test]
  fn rejects_absolute_traversal_back_outside() {
    let (_tmp, sb) = tmp_sandbox();
    let sneaky = format!("{}/nested/../../escape.txt", sb.root().display());
    assert!(sb.resolve_write(&sneaky).is_err());
  }

  #[test]
  fn rejects_parent_dir() {
    let (_tmp, sb) = tmp_sandbox();
    assert!(sb.resolve_read("../escape").is_err());
    assert!(sb.resolve_read("nested/../../escape").is_err());
    assert!(sb.resolve_write("../escape").is_err());
  }

  #[test]
  fn rejects_empty() {
    let (_tmp, sb) = tmp_sandbox();
    assert!(sb.resolve_read("").is_err());
    assert!(sb.resolve_write("").is_err());
  }

  #[test]
  fn resolves_valid_read() {
    let (tmp, sb) = tmp_sandbox();
    std::fs::write(tmp.path().join("ok.txt"), b"hello").unwrap();
    let resolved = sb.resolve_read("ok.txt").expect("resolve");
    assert!(resolved.starts_with(sb.root()));
    assert_eq!(resolved.file_name().unwrap(), "ok.txt");
  }

  #[test]
  fn resolves_valid_write_creates_parent() {
    let (tmp, sb) = tmp_sandbox();
    let resolved = sb.resolve_write("nested/deep/new.txt").expect("resolve");
    assert!(resolved.starts_with(sb.root()));
    assert!(tmp.path().join("nested/deep").is_dir());
  }

  #[cfg(unix)]
  #[test]
  fn rejects_symlink_write_final_component() {
    use std::os::unix::fs::symlink;
    let (tmp, sb) = tmp_sandbox();
    let outside = tempfile::tempdir().unwrap();
    // A pre-seeded symlink at the write target pointing outside the
    // sandbox must NOT be writable through (the parent is in-root but
    // the final component escapes).
    symlink(outside.path().join("escape.txt"), tmp.path().join("out.txt")).unwrap();
    let err = sb.resolve_write("out.txt").unwrap_err();
    assert_eq!(err.kind, crate::error::ScriptErrorKind::SandboxViolation);
    assert!(
      !outside.path().join("escape.txt").exists(),
      "must not have created the target"
    );
  }

  #[cfg(unix)]
  #[test]
  fn rejects_symlink_escape() {
    use std::os::unix::fs::symlink;
    let (tmp, sb) = tmp_sandbox();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret"), b"nope").unwrap();
    symlink(outside.path().join("secret"), tmp.path().join("link")).unwrap();
    let err = sb.resolve_read("link").unwrap_err();
    assert_eq!(err.kind, crate::error::ScriptErrorKind::SandboxViolation);
  }
}

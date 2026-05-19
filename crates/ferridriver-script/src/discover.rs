//! Canonical source-file discovery for extensions and BDD step files.
//!
//! Both hosts (the MCP server's plugin loader and the BDD runner's
//! extension/step discovery) must agree on which file extensions count
//! as loadable source and must walk directories the same way — otherwise
//! a `.tsx` extension visible to the test runner is invisible to the MCP
//! server, which is exactly the inconsistency this module removes.

use std::path::{Path, PathBuf};

/// Extensions rolldown can bundle as an ESM entry. Superset of what
/// either host accepted before: `.cts`/`.cjs`/`.tsx`/`.jsx`/`.mts`/
/// `.mjs` are all valid rolldown entries, so all hosts accept them.
pub const SOURCE_EXTENSIONS: &[&str] = &["js", "cjs", "mjs", "jsx", "ts", "cts", "mts", "tsx"];

/// True when `path` has a bundleable source extension.
#[must_use]
pub fn is_source_file(path: &Path) -> bool {
  path
    .extension()
    .and_then(|e| e.to_str())
    .is_some_and(|ext| SOURCE_EXTENSIONS.contains(&ext))
}

/// Recursively collect every source file under `dir` (sorted, stable).
/// A non-directory or unreadable entry yields an empty result rather
/// than an error — discovery is best-effort; the caller surfaces "no
/// files found" once, with context.
#[must_use]
pub fn walk_source_files(dir: &Path) -> Vec<PathBuf> {
  let mut out = Vec::new();
  walk_into(dir, &mut out);
  out.sort();
  out.dedup();
  out
}

fn walk_into(dir: &Path, out: &mut Vec<PathBuf>) {
  let Ok(rd) = std::fs::read_dir(dir) else { return };
  for entry in rd.flatten() {
    let p = entry.path();
    if p.is_dir() {
      walk_into(&p, out);
    } else if p.is_file() && is_source_file(&p) {
      out.push(p);
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn accepts_the_full_source_set_rejects_others() {
    for ext in ["js", "cjs", "mjs", "jsx", "ts", "cts", "mts", "tsx"] {
      assert!(is_source_file(Path::new(&format!("a.{ext}"))), "{ext} should be source");
    }
    for ext in ["txt", "json", "map", ""] {
      assert!(
        !is_source_file(Path::new(&format!("a.{ext}"))),
        "{ext} must not be source"
      );
    }
  }

  #[test]
  fn walk_recurses_nested_directories() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    std::fs::write(root.join("top.ts"), "").unwrap();
    std::fs::write(root.join("a/mid.tsx"), "").unwrap();
    std::fs::write(root.join("a/b/deep.cts"), "").unwrap();
    std::fs::write(root.join("a/b/skip.txt"), "").unwrap();

    let found = walk_source_files(root);
    let names: Vec<_> = found
      .iter()
      .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
      .collect();
    assert_eq!(
      names,
      vec!["deep.cts", "mid.tsx", "top.ts"],
      "recursive + sorted, .txt excluded"
    );
  }
}

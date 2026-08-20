//! Node's `require.resolve`: a specifier plus the directory it was
//! written in, answered as an absolute path.
//!
//! Host-neutral on purpose. The algorithm is Node's and belongs here; WHO
//! is asking — which file a bundled frame came from, which specifiers the
//! runtime serves natively — is the host's to decide, so nothing in this
//! module looks at a `Ctx` or knows a builtin from a package.
//!
//! Node's algorithm, minus the parts that cannot exist here. It reports a
//! path and nothing else — unlike a loader, which resolves a specifier in
//! order to LOAD it and can therefore insist the target is ESM. `require.resolve('./legacy.cjs')` is a legitimate
//! question with a legitimate answer, so nothing here inspects module
//! format.
//!
//! Not implemented, and documented rather than faked: `require.resolve`'s
//! `{ paths }` option and `require.resolve.paths()`. Both describe a
//! module search path this runtime does not have.
//!
//! Hand-written rather than vendored: upstream llrt is ESM-only and has no
//! `require`, let alone `require.resolve`.

use simd_json::prelude::*;
use std::path::{Path, PathBuf};

/// Extensions tried when a specifier names no file that exists, in order.
///
/// Node's own list is `.js`, `.json`, `.node`; the TypeScript ones are
/// here because this runtime serves them as source and a suite written in
/// TypeScript resolves its own siblings.
const EXTENSIONS: &[&str] = &["js", "mjs", "cjs", "ts", "mts", "cts", "tsx", "jsx", "json"];

/// Index files tried when a specifier names a directory.
const INDEX_STEMS: &[&str] = &["index"];

/// Resolve `specifier` as written in a file inside `base_dir`.
///
/// # Errors
///
/// Returns an error naming the specifier when nothing resolves, the way
/// Node's `MODULE_NOT_FOUND` does.
pub fn resolve(base_dir: &Path, specifier: &str) -> Result<PathBuf, String> {
  if specifier.is_empty() {
    return Err(not_found(specifier, base_dir));
  }

  if is_path_specifier(specifier) {
    let joined = if Path::new(specifier).is_absolute() {
      PathBuf::from(specifier)
    } else {
      base_dir.join(specifier)
    };
    return as_file_or_directory(&joined).ok_or_else(|| not_found(specifier, base_dir));
  }

  from_node_modules(base_dir, specifier).ok_or_else(|| not_found(specifier, base_dir))
}

/// `./x`, `../x`, `/x`, `.` and `..` — everything else is a package name.
fn is_path_specifier(specifier: &str) -> bool {
  specifier.starts_with("./")
    || specifier.starts_with("../")
    || specifier.starts_with('/')
    || specifier == "."
    || specifier == ".."
}

/// Node's LOAD_AS_FILE then LOAD_AS_DIRECTORY.
fn as_file_or_directory(path: &Path) -> Option<PathBuf> {
  as_file(path).or_else(|| as_directory(path))
}

/// The path itself, then the path with each extension appended.
///
/// Appended, never substituted: `./chart.min` resolves to
/// `./chart.min.js`, and `Path::with_extension` would have looked for
/// `./chart.js`.
fn as_file(path: &Path) -> Option<PathBuf> {
  if path.is_file() {
    return canonical(path);
  }
  for ext in EXTENSIONS {
    let mut candidate = path.as_os_str().to_os_string();
    candidate.push(".");
    candidate.push(ext);
    let candidate = PathBuf::from(candidate);
    if candidate.is_file() {
      return canonical(&candidate);
    }
  }
  None
}

/// A directory resolves through its `package.json` entry, then `index.*`.
fn as_directory(path: &Path) -> Option<PathBuf> {
  if !path.is_dir() {
    return None;
  }
  if let Some(entry) = manifest_entry(path) {
    if let Some(found) = as_file_or_directory(&path.join(entry)) {
      return Some(found);
    }
  }
  for stem in INDEX_STEMS {
    if let Some(found) = as_file(&path.join(stem)) {
      return Some(found);
    }
  }
  None
}

/// The file a package's `package.json` points at.
///
/// `exports` first (its `require` / `import` / `default` condition, or a
/// bare string), then `module`, then `main` — the order a bundler reads
/// them in, and the same precedence [`crate::discover`] uses for an
/// package.
fn manifest_entry(pkg_dir: &Path) -> Option<String> {
  let text = std::fs::read_to_string(pkg_dir.join("package.json")).ok()?;
  let mut bytes = text.into_bytes();
  let json = simd_json::to_owned_value(&mut bytes).ok()?;
  if let Some(exports) = json.get("exports") {
    if let Some(entry) = export_target(exports) {
      return Some(entry);
    }
  }
  for field in ["module", "main"] {
    if let Some(value) = json.get(field).and_then(|v| v.as_str()) {
      return Some(value.to_string());
    }
  }
  None
}

/// The root target of an `exports` field: a bare string, or the `"."`
/// entry, resolved through the conditions this runtime presents.
fn export_target(exports: &simd_json::OwnedValue) -> Option<String> {
  if let Some(direct) = exports.as_str() {
    return Some(direct.to_string());
  }
  let root = exports.get(".").unwrap_or(exports);
  if let Some(direct) = root.as_str() {
    return Some(direct.to_string());
  }
  for condition in ["import", "require", "default"] {
    if let Some(value) = root.get(condition) {
      if let Some(direct) = value.as_str() {
        return Some(direct.to_string());
      }
      // A nested condition map (`{ import: { default: "./x.js" } }`).
      if let Some(nested) = export_target(value) {
        return Some(nested);
      }
    }
  }
  None
}

/// Walk `node_modules` upward from `base_dir`, as Node does.
fn from_node_modules(base_dir: &Path, specifier: &str) -> Option<PathBuf> {
  let (package, subpath) = split_package(specifier);
  for dir in base_dir.ancestors() {
    // `node_modules/node_modules` is not a thing; skip a directory that
    // is itself inside one only when it names no package of its own.
    let candidate = dir.join("node_modules").join(&package);
    if !candidate.is_dir() {
      continue;
    }
    let found = match subpath {
      Some(sub) => as_file_or_directory(&candidate.join(sub)),
      None => as_directory(&candidate),
    };
    if found.is_some() {
      return found;
    }
  }
  None
}

/// `@scope/name/sub/path` -> (`@scope/name`, `sub/path`).
fn split_package(specifier: &str) -> (String, Option<&str>) {
  let mut parts = specifier.splitn(if specifier.starts_with('@') { 3 } else { 2 }, '/');
  let mut name = parts.next().unwrap_or(specifier).to_string();
  if specifier.starts_with('@') {
    if let Some(second) = parts.next() {
      name.push('/');
      name.push_str(second);
    }
  }
  let rest = parts.next().filter(|s| !s.is_empty());
  (name, rest)
}

fn canonical(path: &Path) -> Option<PathBuf> {
  Some(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
}

fn not_found(specifier: &str, base_dir: &Path) -> String {
  format!("Cannot find module '{specifier}' from {}", base_dir.display())
}

#[cfg(test)]
mod tests {
  use super::*;

  fn tree() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    std::fs::write(root.join("sibling.ts"), b"").expect("write");
    std::fs::write(root.join("chart.min.js"), b"").expect("write");
    std::fs::create_dir_all(root.join("folder")).expect("mkdir");
    std::fs::write(root.join("folder/index.js"), b"").expect("write");
    std::fs::create_dir_all(root.join("nested/deep")).expect("mkdir");
    std::fs::write(root.join("nested/deep/leaf.ts"), b"").expect("write");
    tmp
  }

  #[test]
  fn a_relative_specifier_gets_its_extension_appended() {
    let tmp = tree();
    let found = resolve(tmp.path(), "./sibling").expect("resolve");
    assert_eq!(found.file_name().expect("name"), "sibling.ts");
  }

  /// An extension is APPENDED, not substituted: `./chart.min` is not a
  /// request for `./chart.js`.
  #[test]
  fn a_dotted_stem_keeps_its_own_suffix() {
    let tmp = tree();
    let found = resolve(tmp.path(), "./chart.min").expect("resolve");
    assert_eq!(found.file_name().expect("name"), "chart.min.js");
  }

  #[test]
  fn a_directory_resolves_through_its_index() {
    let tmp = tree();
    let found = resolve(tmp.path(), "./folder").expect("resolve");
    assert_eq!(found.file_name().expect("name"), "index.js");
  }

  #[test]
  fn a_parent_specifier_resolves_from_the_asking_directory() {
    let tmp = tree();
    let found = resolve(&tmp.path().join("nested/deep"), "../../sibling.ts").expect("resolve");
    assert_eq!(found.file_name().expect("name"), "sibling.ts");
  }

  #[test]
  fn a_missing_module_names_itself_and_where_it_was_asked_from() {
    let tmp = tree();
    let err = resolve(tmp.path(), "./nope").expect_err("missing");
    assert!(err.contains("Cannot find module './nope'"), "{err}");
    assert!(err.contains(&tmp.path().display().to_string()), "{err}");
  }

  #[test]
  fn a_bare_specifier_walks_node_modules_upward() {
    let tmp = tree();
    let pkg = tmp.path().join("node_modules/acme");
    std::fs::create_dir_all(&pkg).expect("mkdir");
    std::fs::write(pkg.join("package.json"), br#"{"main":"./lib/entry.js"}"#).expect("write");
    std::fs::create_dir_all(pkg.join("lib")).expect("mkdir");
    std::fs::write(pkg.join("lib/entry.js"), b"").expect("write");

    let found = resolve(&tmp.path().join("nested/deep"), "acme").expect("resolve");
    assert_eq!(found, canonical(&pkg.join("lib/entry.js")).expect("canonical"));
  }

  #[test]
  fn a_scoped_package_subpath_resolves() {
    let tmp = tree();
    let pkg = tmp.path().join("node_modules/@acme/kit");
    std::fs::create_dir_all(pkg.join("src")).expect("mkdir");
    std::fs::write(pkg.join("package.json"), br#"{"main":"./index.js"}"#).expect("write");
    std::fs::write(pkg.join("src/helper.ts"), b"").expect("write");

    let found = resolve(tmp.path(), "@acme/kit/src/helper").expect("resolve");
    assert_eq!(found.file_name().expect("name"), "helper.ts");
  }

  #[test]
  fn an_exports_condition_decides_the_entry() {
    let tmp = tree();
    let pkg = tmp.path().join("node_modules/conditional");
    std::fs::create_dir_all(&pkg).expect("mkdir");
    std::fs::write(
      pkg.join("package.json"),
      br#"{"exports":{".":{"import":"./esm.js","require":"./cjs.js"}},"main":"./ignored.js"}"#,
    )
    .expect("write");
    std::fs::write(pkg.join("esm.js"), b"").expect("write");
    std::fs::write(pkg.join("cjs.js"), b"").expect("write");
    std::fs::write(pkg.join("ignored.js"), b"").expect("write");

    let found = resolve(tmp.path(), "conditional").expect("resolve");
    assert_eq!(found.file_name().expect("name"), "esm.js");
  }
}

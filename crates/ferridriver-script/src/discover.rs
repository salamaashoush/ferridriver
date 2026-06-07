//! Canonical source-file discovery for extensions and BDD step files.
//!
//! Both hosts (the MCP server's plugin loader and the BDD runner's
//! extension/step discovery) must agree on which file extensions count
//! as loadable source and must walk directories the same way — otherwise
//! a `.tsx` extension visible to the test runner is invisible to the MCP
//! server, which is exactly the inconsistency this module removes.

use std::path::{Path, PathBuf};

use crate::error::ScriptError;

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

/// Resolve configured extension specifiers to concrete ESM entry files.
///
/// Rules:
/// - relative/absolute file => that file
/// - relative/absolute directory with `package.json` => ESM package entry
/// - relative/absolute directory without `package.json` => recursive source scan
/// - bare specifier => ESM package entry from nearest `node_modules`
///
/// CommonJS package entries are rejected. Extension packages should be ESM
/// (`exports`, `module`, `.mjs`/`.mts`, or `type: "module"` for `.js`).
pub fn resolve_extension_specs(specs: &[String], cwd: &Path) -> (Vec<PathBuf>, Vec<(String, ScriptError)>) {
  let mut files = Vec::new();
  let mut errors = Vec::new();
  for spec in specs {
    match resolve_extension_spec(spec, cwd) {
      Ok(mut found) => files.append(&mut found),
      Err(e) => errors.push((spec.clone(), e)),
    }
  }
  files.sort();
  files.dedup();
  (files, errors)
}

fn resolve_extension_spec(spec: &str, cwd: &Path) -> Result<Vec<PathBuf>, ScriptError> {
  if looks_like_path(spec) {
    let p = if Path::new(spec).is_absolute() {
      PathBuf::from(spec)
    } else {
      cwd.join(spec)
    };
    return resolve_path_spec(&p);
  }

  Ok(vec![resolve_package_spec(cwd, spec)?])
}

fn looks_like_path(spec: &str) -> bool {
  spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') || spec == "." || spec == ".."
}

fn resolve_path_spec(path: &Path) -> Result<Vec<PathBuf>, ScriptError> {
  let meta =
    std::fs::metadata(path).map_err(|e| ScriptError::internal(format!("extension path {}: {e}", path.display())))?;
  if meta.is_file() {
    return Ok(vec![path.to_path_buf()]);
  }
  if meta.is_dir() {
    if path.join("package.json").is_file() {
      return Ok(vec![resolve_package_entry(path)?]);
    }
    return Ok(walk_source_files(path));
  }
  Ok(Vec::new())
}

fn resolve_package_spec(cwd: &Path, spec: &str) -> Result<PathBuf, ScriptError> {
  let (pkg_name, subpath) = split_package_spec(spec)?;
  let mut dir = cwd;
  loop {
    let candidate = dir.join("node_modules").join(&pkg_name);
    if candidate.is_dir() {
      if let Some(subpath) = subpath {
        let p = candidate.join(subpath);
        let type_module = package_type_module(&candidate);
        return resolve_subpath_entry(&p, type_module).map_err(|e| ScriptError::internal(format!("{spec}: {e}")));
      }
      return resolve_package_entry(&candidate);
    }
    let Some(parent) = dir.parent() else { break };
    dir = parent;
  }
  Err(ScriptError::internal(format!(
    "extension package `{spec}` not found from {}",
    cwd.display()
  )))
}

fn split_package_spec(spec: &str) -> Result<(String, Option<&str>), ScriptError> {
  if spec.starts_with('@') {
    let mut parts = spec.splitn(3, '/');
    let scope = parts.next().unwrap_or_default();
    let name = parts
      .next()
      .ok_or_else(|| ScriptError::internal(format!("invalid package specifier `{spec}`")))?;
    let pkg = format!("{scope}/{name}");
    Ok((pkg, parts.next()))
  } else {
    let mut parts = spec.splitn(2, '/');
    let pkg = parts.next().unwrap_or_default().to_string();
    Ok((pkg, parts.next()))
  }
}

fn resolve_subpath_entry(path: &Path, root_type_module: bool) -> Result<PathBuf, String> {
  if path.is_file() {
    return ensure_esm_entry(path, root_type_module).map(|()| path.to_path_buf());
  }
  for ext in ["mjs", "mts", "js", "ts"] {
    let p = path.with_extension(ext);
    if p.is_file() {
      return ensure_esm_entry(&p, root_type_module).map(|()| p);
    }
  }
  if path.is_dir() {
    let type_module = package_type_module(path) || root_type_module;
    for name in ["index.mjs", "index.mts", "index.ts", "index.js"] {
      let p = path.join(name);
      if p.is_file() {
        return ensure_esm_entry(&p, type_module).map(|()| p);
      }
    }
  }
  Err(format!("subpath {} is not an ESM source entry", path.display()))
}

fn resolve_package_entry(pkg_dir: &Path) -> Result<PathBuf, ScriptError> {
  let pkg_json = pkg_dir.join("package.json");
  let raw = std::fs::read_to_string(&pkg_json)
    .map_err(|e| ScriptError::internal(format!("read {}: {e}", pkg_json.display())))?;
  let json: serde_json::Value =
    serde_json::from_str(&raw).map_err(|e| ScriptError::internal(format!("parse {}: {e}", pkg_json.display())))?;
  let type_module = json.get("type").and_then(serde_json::Value::as_str) == Some("module");

  if let Some(exports) = json.get("exports").and_then(select_root_export) {
    return entry_from_field(pkg_dir, exports, type_module, "exports");
  }
  if let Some(module) = json.get("module").and_then(serde_json::Value::as_str) {
    return entry_from_field(pkg_dir, module, type_module, "module");
  }
  if let Some(main) = json.get("main").and_then(serde_json::Value::as_str) {
    return entry_from_field(pkg_dir, main, type_module, "main");
  }

  for name in ["index.mjs", "index.mts", "index.ts", "index.js"] {
    let p = pkg_dir.join(name);
    if p.is_file() && ensure_esm_entry(&p, type_module).is_ok() {
      return Ok(p);
    }
  }

  Err(ScriptError::internal(format!(
    "package {} has no ESM entry (expected exports, module, ESM main, or index)",
    pkg_dir.display()
  )))
}

fn select_root_export(v: &serde_json::Value) -> Option<&str> {
  match v {
    serde_json::Value::String(s) => Some(s),
    serde_json::Value::Object(map) => {
      if let Some(root) = map.get(".") {
        return select_conditional_export(root);
      }
      select_conditional_export(v)
    },
    _ => None,
  }
}

fn select_conditional_export(v: &serde_json::Value) -> Option<&str> {
  match v {
    serde_json::Value::String(s) => Some(s),
    serde_json::Value::Object(map) => ["import", "default"]
      .iter()
      .find_map(|k| map.get(*k).and_then(select_conditional_export)),
    _ => None,
  }
}

fn entry_from_field(pkg_dir: &Path, rel: &str, type_module: bool, field: &str) -> Result<PathBuf, ScriptError> {
  let p = pkg_dir.join(rel);
  ensure_esm_entry(&p, type_module)
    .map_err(|e| ScriptError::internal(format!("package {} {field}: {e}", pkg_dir.display())))?;
  Ok(p)
}

fn package_type_module(dir: &Path) -> bool {
  let pkg = dir.join("package.json");
  std::fs::read_to_string(pkg)
    .ok()
    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    .and_then(|v| v.get("type").and_then(serde_json::Value::as_str).map(str::to_string))
    .as_deref()
    == Some("module")
}

fn ensure_esm_entry(path: &Path, type_module: bool) -> Result<(), String> {
  if !path.is_file() {
    return Err(format!("{} does not exist", path.display()));
  }
  match path.extension().and_then(|e| e.to_str()) {
    Some("mjs" | "mts" | "ts" | "tsx" | "jsx") => Ok(()),
    Some("js") if type_module => Ok(()),
    Some("js") => Err(format!("{} is .js but package type is not \"module\"", path.display())),
    Some(other) => Err(format!("{} has unsupported extension .{other}", path.display())),
    None => Err(format!("{} has no extension", path.display())),
  }
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

  #[test]
  fn resolves_esm_package_from_node_modules() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pkg = tmp.path().join("node_modules/@acme/fd-ext");
    std::fs::create_dir_all(pkg.join("dist")).unwrap();
    std::fs::write(
      pkg.join("package.json"),
      r#"{"name":"@acme/fd-ext","type":"module","exports":"./dist/index.js"}"#,
    )
    .unwrap();
    std::fs::write(pkg.join("dist/index.js"), "export const x = 1;").unwrap();

    let (files, errors) = resolve_extension_specs(&["@acme/fd-ext".to_string()], tmp.path());
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(files, vec![pkg.join("dist/index.js")]);
  }

  #[test]
  fn resolves_esm_package_subpath_from_node_modules() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pkg = tmp.path().join("node_modules/@acme/fd-ext");
    std::fs::create_dir_all(pkg.join("dist")).unwrap();
    std::fs::write(pkg.join("package.json"), r#"{"name":"@acme/fd-ext","type":"module"}"#).unwrap();
    std::fs::write(pkg.join("dist/login.js"), "export const x = 1;").unwrap();

    let (files, errors) = resolve_extension_specs(&["@acme/fd-ext/dist/login".to_string()], tmp.path());
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(files, vec![pkg.join("dist/login.js")]);
  }

  #[test]
  fn rejects_commonjs_package_main() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pkg = tmp.path().join("node_modules/cjs-ext");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("package.json"), r#"{"name":"cjs-ext","main":"./index.js"}"#).unwrap();
    std::fs::write(pkg.join("index.js"), "module.exports = {};").unwrap();

    let (files, errors) = resolve_extension_specs(&["cjs-ext".to_string()], tmp.path());
    assert!(files.is_empty());
    assert_eq!(errors.len(), 1);
    assert!(errors[0].1.message.contains("type is not \"module\""), "{errors:?}");
  }
}

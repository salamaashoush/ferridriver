//! Canonical source-file discovery for extensions and BDD step files.
//!
//! Both hosts (the MCP server's extension loader and the BDD runner's
//! extension/step discovery) must agree on which file extensions count
//! as loadable source and must walk directories the same way — otherwise
//! a `.tsx` extension visible to the test runner is invisible to the MCP
//! server, which is exactly the inconsistency this module removes.

use std::path::{Path, PathBuf};

use ferridriver_config::{ExtensionManifest, ExtensionSpec};

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

/// One configured `extensions` entry after resolution.
///
/// The flat `(files, errors)` shape cannot express the two things a host
/// needs beyond the entry list: WHICH package a set of files came from,
/// and what that package declared in its
/// [`ExtensionManifest`] — the host preconditions and settings schemas
/// that must be checked before the package is treated as usable.
#[derive(Debug, Clone)]
pub struct ResolvedExtension {
  /// The `extensions` entry exactly as configured.
  pub spec: String,
  /// Directory of the config layer that declared it.
  pub base_dir: PathBuf,
  /// The package directory, when the spec resolved to a package
  /// (`package.json` present) rather than a loose file or directory.
  pub package_dir: Option<PathBuf>,
  /// The package's `ferridriver` manifest, when it declares one.
  pub manifest: Option<ExtensionManifest>,
  /// Entry files to load as extensions, in resolution order (manifest
  /// `entries` order for a manifest package, sorted for a directory
  /// scan), each carrying whatever its manifest item narrowed it to.
  pub files: Vec<ResolvedEntry>,
}

/// One resolved entry file, with the manifest item's narrowing attached.
///
/// A directory entry expands to many files; each inherits the item's
/// `hosts` and `requires`, because the narrowing was written about the
/// entry, not about one file inside it.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedEntry {
  pub path: PathBuf,
  /// Hosts this entry loads under. Empty means every host.
  #[serde(skip_serializing_if = "Vec::is_empty")]
  pub hosts: Vec<String>,
  /// The entry's own preconditions, when its manifest item declared
  /// any; otherwise the package's apply.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub requires: Option<ferridriver_config::extension_manifest::ExtensionRequires>,
}

impl ResolvedEntry {
  /// Whether this entry loads under `host`.
  #[must_use]
  pub fn runs_under(&self, host: &str) -> bool {
    self.hosts.is_empty() || self.hosts.iter().any(|h| h == host)
  }
}

impl From<PathBuf> for ResolvedEntry {
  fn from(path: PathBuf) -> Self {
    Self {
      path,
      hosts: Vec::new(),
      requires: None,
    }
  }
}

impl ResolvedExtension {
  /// Every entry file, narrowing ignored.
  pub fn paths(&self) -> impl Iterator<Item = &PathBuf> {
    self.files.iter().map(|e| &e.path)
  }

  /// The entries that load under `host`.
  pub fn entries_for_host<'a>(&'a self, host: &'a str) -> impl Iterator<Item = &'a ResolvedEntry> {
    self.files.iter().filter(move |e| e.runs_under(host))
  }
}

/// Resolve configured extension specifiers to concrete ESM entry files.
///
/// Rules:
/// - relative/absolute file => that file
/// - relative/absolute directory with `package.json` => package entries
/// - relative/absolute directory without `package.json` => recursive source scan
/// - bare specifier => package entries from nearest `node_modules`
///
/// A package's entries come from its `ferridriver.entries` manifest when
/// it declares one (any number of entries, in declaration order),
/// otherwise from Node's own single-entry chain (`exports` / `module` /
/// `main` / `index`).
///
/// CommonJS package entries are rejected. Extension packages should be ESM
/// (`exports`, `module`, `.mjs`/`.mts`, or `type: "module"` for `.js`).
pub fn resolve_extension_specs(specs: &[String], cwd: &Path) -> (Vec<PathBuf>, Vec<(String, ScriptError)>) {
  let owned: Vec<ExtensionSpec> = specs
    .iter()
    .map(|s| ExtensionSpec {
      spec: s.clone(),
      base_dir: cwd.to_path_buf(),
    })
    .collect();
  resolve_extension_specs_with_bases(&owned)
}

/// Resolve specs that each carry their OWN base directory.
///
/// A config file's `extensions` entry means "relative to that file",
/// and a package specifier must walk `node_modules` from that file's
/// directory. Resolving every spec against one process cwd instead is
/// what made a user-level extension list break the moment the process
/// ran in a different repository.
///
/// Pair the specs with [`ferridriver_config::FerridriverConfig::extension_specs`].
pub fn resolve_extension_specs_with_bases(specs: &[ExtensionSpec]) -> (Vec<PathBuf>, Vec<(String, ScriptError)>) {
  let (resolved, errors) = resolve_extensions(specs);
  let mut files: Vec<PathBuf> = Vec::new();
  for r in resolved {
    for entry in r.files {
      // Dedup keeping the FIRST occurrence: a manifest's `entries` order
      // is the author's load order, and sorting the flat list would
      // silently reorder it.
      if !files.contains(&entry.path) {
        files.push(entry.path);
      }
    }
  }
  (files, errors)
}

/// Like [`resolve_extension_specs_with_bases`], but keeping each spec's
/// package identity and [`ExtensionManifest`] so a host can check the
/// package's declared preconditions before loading it.
pub fn resolve_extensions(specs: &[ExtensionSpec]) -> (Vec<ResolvedExtension>, Vec<(String, ScriptError)>) {
  let mut resolved = Vec::new();
  let mut errors = Vec::new();
  for ExtensionSpec { spec, base_dir } in specs {
    match resolve_extension_spec(spec, base_dir) {
      Ok(found) => resolved.push(ResolvedExtension {
        spec: spec.clone(),
        base_dir: base_dir.clone(),
        package_dir: found.package_dir,
        manifest: found.manifest,
        files: found.files,
      }),
      Err(e) => errors.push((spec.clone(), e)),
    }
  }
  (resolved, errors)
}

/// What one spec resolved to, before it is paired back with its spec.
struct SpecResolution {
  files: Vec<ResolvedEntry>,
  package_dir: Option<PathBuf>,
  manifest: Option<ExtensionManifest>,
}

impl SpecResolution {
  fn files(files: Vec<PathBuf>) -> Self {
    Self {
      files: files.into_iter().map(ResolvedEntry::from).collect(),
      package_dir: None,
      manifest: None,
    }
  }
}

fn resolve_extension_spec(spec: &str, cwd: &Path) -> Result<SpecResolution, ScriptError> {
  if looks_like_path(spec) {
    let p = if Path::new(spec).is_absolute() {
      PathBuf::from(spec)
    } else {
      tidy(cwd.join(spec))
    };
    return resolve_path_spec(&p);
  }

  resolve_package_spec(cwd, spec)
}

fn looks_like_path(spec: &str) -> bool {
  spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') || spec == "." || spec == ".."
}

/// Rewrite a joined path without its `.` segments, so a configured
/// `./src/a.ts` becomes `<pkg>/src/a.ts` rather than `<pkg>/./src/a.ts`.
///
/// Not cosmetic: an entry path carrying `/./` is handed to rolldown as
/// the bundle entry AND as its cwd, and the `sources` rolldown then emits
/// no longer resolve back to real files — which silently emptied the
/// transitive input set the bytecode caches use for freshness, so an
/// edited helper never invalidated the entry.
///
/// `..` is deliberately preserved: collapsing it lexically changes which
/// file the path names as soon as a symlink is involved. (`Path::
/// components` already skips interior `.`, so re-collecting is what
/// actually rewrites the string; a leading `.` survives as `CurDir` and
/// is dropped here.)
fn tidy(path: PathBuf) -> PathBuf {
  let cleaned: PathBuf = path
    .components()
    .filter(|c| !matches!(c, std::path::Component::CurDir))
    .collect();
  if cleaned.as_os_str().is_empty() {
    PathBuf::from(".")
  } else {
    cleaned
  }
}

fn resolve_path_spec(path: &Path) -> Result<SpecResolution, ScriptError> {
  let meta =
    std::fs::metadata(path).map_err(|e| ScriptError::internal(format!("extension path {}: {e}", path.display())))?;
  if meta.is_file() {
    return Ok(SpecResolution::files(vec![path.to_path_buf()]));
  }
  if meta.is_dir() {
    if path.join("package.json").is_file() {
      return resolve_package(path);
    }
    return Ok(SpecResolution::files(walk_source_files(path)));
  }
  Ok(SpecResolution::files(Vec::new()))
}

fn resolve_package_spec(cwd: &Path, spec: &str) -> Result<SpecResolution, ScriptError> {
  let (pkg_name, subpath) = split_package_spec(spec)?;
  let mut dir = cwd;
  loop {
    let candidate = dir.join("node_modules").join(&pkg_name);
    if candidate.is_dir() {
      let Some(subpath) = subpath else {
        return resolve_package(&candidate);
      };
      // An explicit subpath names the entry itself, so the manifest's
      // `entries` do not apply — but the package's `requires` still do.
      let p = candidate.join(subpath);
      let type_module = package_type_module(&candidate);
      let entry = resolve_subpath_entry(&p, type_module).map_err(|e| ScriptError::internal(format!("{spec}: {e}")))?;
      let manifest = read_package_manifest(&candidate)?;
      return Ok(SpecResolution {
        files: vec![entry.into()],
        package_dir: Some(candidate),
        manifest,
      });
    }
    let Some(parent) = dir.parent() else { break };
    dir = parent;
  }
  Err(ScriptError::internal(format!(
    "extension package `{spec}` not found from {}",
    cwd.display()
  )))
}

/// Read + parse a package's `package.json`.
fn read_package_json(pkg_dir: &Path) -> Result<serde_json::Value, ScriptError> {
  let pkg_json = pkg_dir.join("package.json");
  let raw = std::fs::read_to_string(&pkg_json)
    .map_err(|e| ScriptError::internal(format!("read {}: {e}", pkg_json.display())))?;
  serde_json::from_str(&raw).map_err(|e| ScriptError::internal(format!("parse {}: {e}", pkg_json.display())))
}

/// The package's `ferridriver` manifest. A malformed manifest is an
/// error, not a silent fallback: the author wrote it expecting it to take
/// effect.
fn read_package_manifest(pkg_dir: &Path) -> Result<Option<ExtensionManifest>, ScriptError> {
  let json = read_package_json(pkg_dir)?;
  ExtensionManifest::from_package_json(&json)
    .map_err(|e| ScriptError::internal(format!("package {}: {e}", pkg_dir.display())))
}

/// Resolve every entry of a package directory.
///
/// `ferridriver.entries` wins when present — an extension package
/// commonly has several tool modules plus a shared `lib/`, which Node's
/// single-entry fields cannot express and a directory scan gets wrong
/// (each `lib/` module would load as a tool-less extension).
fn resolve_package(pkg_dir: &Path) -> Result<SpecResolution, ScriptError> {
  let json = read_package_json(pkg_dir)?;
  let manifest = ExtensionManifest::from_package_json(&json)
    .map_err(|e| ScriptError::internal(format!("package {}: {e}", pkg_dir.display())))?;
  let type_module = json.get("type").and_then(serde_json::Value::as_str) == Some("module");

  let files: Vec<ResolvedEntry> = match manifest.as_ref().filter(|m| !m.entries.is_empty()) {
    Some(m) => {
      let mut files: Vec<ResolvedEntry> = Vec::new();
      for entry in &m.entries {
        // A directory entry expands to many files; each inherits what
        // the manifest item said about the entry as a whole.
        for path in resolve_manifest_entry(pkg_dir, &entry.path, type_module)? {
          if !files.iter().any(|e| e.path == path) {
            files.push(ResolvedEntry {
              path,
              hosts: entry.hosts.clone(),
              requires: entry.requires.clone(),
            });
          }
        }
      }
      if files.is_empty() {
        return Err(ScriptError::internal(format!(
          "package {}: `ferridriver.entries` resolved to no source files",
          pkg_dir.display()
        )));
      }
      files
    },
    None => vec![resolve_node_package_entry(pkg_dir, &json, type_module)?.into()],
  };

  Ok(SpecResolution {
    files,
    package_dir: Some(pkg_dir.to_path_buf()),
    manifest,
  })
}

/// One `ferridriver.entries` item: a source file (extension optional) or
/// a directory scanned recursively.
fn resolve_manifest_entry(pkg_dir: &Path, entry: &str, type_module: bool) -> Result<Vec<PathBuf>, ScriptError> {
  let bad = |detail: String| {
    ScriptError::internal(format!(
      "package {}: `ferridriver.entries` item `{entry}`: {detail}",
      pkg_dir.display()
    ))
  };
  if Path::new(entry).is_absolute() {
    return Err(bad("must be a path relative to the package directory".to_string()));
  }
  let path = tidy(pkg_dir.join(entry));
  if path.is_dir() {
    let found = walk_source_files(&path);
    if found.is_empty() {
      return Err(bad(format!("directory {} holds no source files", path.display())));
    }
    return Ok(found);
  }
  resolve_subpath_entry(&path, type_module).map(|p| vec![p]).map_err(bad)
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

/// Node's own single-entry chain, used when the package declares no
/// `ferridriver.entries`.
fn resolve_node_package_entry(
  pkg_dir: &Path,
  json: &serde_json::Value,
  type_module: bool,
) -> Result<PathBuf, ScriptError> {
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
    "package {} has no entry: declare the extension modules in its package.json as \
     \"ferridriver\": {{ \"entries\": [\"./src/tool.ts\"] }}, or give it an ESM \
     exports/module/main/index",
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

  /// The shape an extension package actually has: several tool modules
  /// plus a shared `lib/`. Node's entry fields cannot express it and a
  /// directory scan gets it wrong, so `ferridriver.entries` is what makes
  /// the package loadable at all.
  fn multi_entry_package(root: &Path) -> PathBuf {
    let pkg = root.join("node_modules/@acme/fd-box");
    std::fs::create_dir_all(pkg.join("src/lib")).unwrap();
    std::fs::write(
      pkg.join("package.json"),
      r#"{
        "name": "@acme/fd-box",
        "type": "module",
        "ferridriver": {
          "entries": ["./src/sign.ts", "./src/login.ts"],
          "requires": { "commands": ["acme-cli"], "net": ["*.acme.com"] },
          "settings": { "acme": { "type": "object" } }
        }
      }"#,
    )
    .unwrap();
    std::fs::write(pkg.join("src/sign.ts"), "import './lib/shared'; export const a = 1;").unwrap();
    std::fs::write(pkg.join("src/login.ts"), "import './lib/shared'; export const b = 2;").unwrap();
    std::fs::write(pkg.join("src/lib/shared.ts"), "export const helper = 1;").unwrap();
    pkg
  }

  #[test]
  fn manifest_entries_load_every_entry_and_no_lib_module() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pkg = multi_entry_package(tmp.path());

    let (files, errors) = resolve_extension_specs(&["@acme/fd-box".to_string()], tmp.path());
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
      files,
      vec![pkg.join("src/sign.ts"), pkg.join("src/login.ts")],
      "every declared entry, in declaration order, and nothing from lib/"
    );
  }

  #[test]
  fn manifest_is_surfaced_with_its_package_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pkg = multi_entry_package(tmp.path());

    let (resolved, errors) = resolve_extensions(&[ExtensionSpec {
      spec: "@acme/fd-box".to_string(),
      base_dir: tmp.path().to_path_buf(),
    }]);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(resolved.len(), 1);
    let r = &resolved[0];
    assert_eq!(r.package_dir.as_deref(), Some(pkg.as_path()));
    let manifest = r.manifest.as_ref().expect("manifest");
    assert_eq!(manifest.requires.commands, ["acme-cli"]);
    assert_eq!(manifest.requires.net, ["*.acme.com"]);
    assert!(manifest.settings.contains_key("acme"));
  }

  #[test]
  fn manifest_entries_work_for_a_directory_path_spec_too() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pkg = multi_entry_package(tmp.path());

    let (files, errors) = resolve_extension_specs(&[format!("./{}", "node_modules/@acme/fd-box")], tmp.path());
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(files, vec![pkg.join("src/sign.ts"), pkg.join("src/login.ts")]);
  }

  #[test]
  fn manifest_entry_may_name_a_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pkg = tmp.path().join("pkg");
    std::fs::create_dir_all(pkg.join("tools")).unwrap();
    std::fs::write(
      pkg.join("package.json"),
      r#"{"name":"p","type":"module","ferridriver":{"entries":["./tools"]}}"#,
    )
    .unwrap();
    std::fs::write(pkg.join("tools/a.ts"), "export const a = 1;").unwrap();
    std::fs::write(pkg.join("tools/b.ts"), "export const b = 2;").unwrap();

    let (files, errors) = resolve_extension_specs(&["./pkg".to_string()], tmp.path());
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(files, vec![pkg.join("tools/a.ts"), pkg.join("tools/b.ts")]);
  }

  #[test]
  fn a_missing_manifest_entry_is_an_error_naming_the_entry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pkg = tmp.path().join("pkg");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
      pkg.join("package.json"),
      r#"{"name":"p","type":"module","ferridriver":{"entries":["./src/gone.ts"]}}"#,
    )
    .unwrap();

    let (files, errors) = resolve_extension_specs(&["./pkg".to_string()], tmp.path());
    assert!(files.is_empty());
    assert_eq!(errors.len(), 1);
    assert!(errors[0].1.message.contains("gone.ts"), "{errors:?}");
  }

  #[test]
  fn a_malformed_manifest_is_an_error_not_a_silent_fallback() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pkg = tmp.path().join("pkg");
    std::fs::create_dir_all(&pkg).unwrap();
    // `index.ts` would resolve fine, so a silent fallback would hide the typo.
    std::fs::write(
      pkg.join("package.json"),
      r#"{"name":"p","type":"module","ferridriver":{"entrys":["./index.ts"]}}"#,
    )
    .unwrap();
    std::fs::write(pkg.join("index.ts"), "export const a = 1;").unwrap();

    let (files, errors) = resolve_extension_specs(&["./pkg".to_string()], tmp.path());
    assert!(files.is_empty(), "{files:?}");
    assert!(errors[0].1.message.contains("entrys"), "{errors:?}");
  }

  #[test]
  fn a_package_with_no_entry_at_all_says_how_to_declare_one() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pkg = tmp.path().join("pkg");
    std::fs::create_dir_all(pkg.join("src")).unwrap();
    std::fs::write(pkg.join("package.json"), r#"{"name":"p","type":"module"}"#).unwrap();
    std::fs::write(pkg.join("src/a.ts"), "export const a = 1;").unwrap();

    let (_, errors) = resolve_extension_specs(&["./pkg".to_string()], tmp.path());
    assert_eq!(errors.len(), 1);
    assert!(
      errors[0].1.message.contains("ferridriver") && errors[0].1.message.contains("entries"),
      "the error must name the fix: {errors:?}"
    );
  }

  #[test]
  fn a_subpath_spec_keeps_the_package_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let pkg = multi_entry_package(tmp.path());

    let (resolved, errors) = resolve_extensions(&[ExtensionSpec {
      spec: "@acme/fd-box/src/login".to_string(),
      base_dir: tmp.path().to_path_buf(),
    }]);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
      resolved[0].paths().cloned().collect::<Vec<_>>(),
      vec![pkg.join("src/login.ts")],
      "the named subpath only"
    );
    assert_eq!(
      resolved[0].manifest.as_ref().map(|m| m.requires.commands.clone()),
      Some(vec!["acme-cli".to_string()]),
      "requires still apply to a subpath entry"
    );
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

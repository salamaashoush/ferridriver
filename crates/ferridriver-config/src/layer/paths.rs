//! Config document normalization and per-layer path anchoring.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// Leaf key names whose string value is a filesystem path and is
/// therefore anchored to its own config file's directory.
const PATH_KEYS: &[&str] = &[
  "scriptRoot",
  "artifactsRoot",
  "executablePath",
  "userDataDir",
  "discoverProfile",
  "testDir",
  "outputDir",
  "snapshotDir",
  "tsconfig",
  "storageState",
  "cwd",
];

/// Leaf key names holding an ARRAY of filesystem paths.
const PATH_ARRAY_KEYS: &[&str] = &["globalSetup", "globalTeardown"];

/// Keys Playwright spells `string | string[]`, both forms holding paths.
const PATH_STRING_OR_ARRAY_KEYS: &[&str] = &["stylePath"];

/// Keys whose object value is keyed by the OPERATOR, not by the schema.
/// Their contents are data, so no key inside them may be read as a path
/// name.
const OPAQUE_MAP_KEYS: &[&str] = &["env", "headers", "extraHTTPHeaders", "settings", "worldParameters"];

/// Normalize shorthand shapes so merging sees one canonical structure.
///
/// `extensions = [..]` becomes `extensions = { paths = [..] }`, so a
/// user layer using the shorthand and a project layer setting a policy
/// combine instead of one clobbering the other.
pub(super) fn normalize(value: &mut Value) {
  let Some(map) = value.as_object_mut() else { return };
  if let Some(existing) = map.get_mut("extensions")
    && existing.is_array()
  {
    let paths = std::mem::replace(existing, Value::Object(Map::new()));
    if let Some(obj) = existing.as_object_mut() {
      obj.insert("paths".to_string(), paths);
    }
  }
  if let Some(test) = map.get_mut("test") {
    normalize_test_section(test);
  }
}

/// Expand every `use` block in the `[test]` section, then fold the
/// top-level one into the browser's.
fn normalize_test_section(test: &mut Value) {
  let Some(map) = test.as_object_mut() else { return };
  expand_device(map.get_mut("use"));
  if let Some(browser) = map.get_mut("browser").and_then(Value::as_object_mut) {
    expand_device(browser.get_mut("use"));
  }
  if let Some(projects) = map.get_mut("projects").and_then(Value::as_array_mut) {
    for project in projects {
      let Some(project) = project.as_object_mut() else {
        continue;
      };
      expand_device(project.get_mut("use"));
      if let Some(browser) = project.get_mut("browser").and_then(Value::as_object_mut) {
        expand_device(browser.get_mut("use"));
      }
    }
  }
  hoist_use(map);
}

/// Move `[test].use` into `[test.browser].use`, the one place the bag is
/// stored. A key written in both spellings keeps the browser's, which is
/// the more specific of the two.
fn hoist_use(map: &mut Map<String, Value>) {
  let Some(top) = map.remove("use") else { return };
  let Some(top) = top.as_object().cloned() else {
    // Not an object: put it back so deserialization reports it rather
    // than dropping it here.
    map.insert("use".to_string(), top);
    return;
  };
  let browser = map
    .entry("browser".to_string())
    .or_insert_with(|| Value::Object(Map::new()));
  let Some(browser) = browser.as_object_mut() else { return };
  let target = browser
    .entry("use".to_string())
    .or_insert_with(|| Value::Object(Map::new()));
  let Some(target) = target.as_object_mut() else { return };
  for (key, value) in top {
    target.entry(key).or_insert(value);
  }
}

fn expand_device(block: Option<&mut Value>) {
  if let Some(block) = block.and_then(Value::as_object_mut) {
    crate::test::expand_device_keys(block);
  }
}

/// Rewrite every relative filesystem path in `value` to be absolute
/// against `dir`.
pub(super) fn anchor_paths(value: &mut Value, dir: &Path) {
  match value {
    Value::Object(map) => {
      for (key, child) in map.iter_mut() {
        match key.as_str() {
          // Alias targets are shim files; virtual modules are inline
          // source and must not be touched.
          "alias" => anchor_map_values(child, dir),
          "paths" => anchor_extension_specs(child, dir),
          "virtualModules" | "virtual_modules" => {},
          // `file` is too common a leaf name to anchor globally, so the
          // secrets table anchors its own member.
          "secrets" => anchor_member(child, "file", dir),
          // Anchoring matches a LEAF KEY NAME at any depth, so a
          // caller-keyed map must be skipped whole: an instance's env
          // variable (or a request header) literally named `cwd` /
          // `testDir` / `outputDir` would otherwise be rewritten into an
          // absolute path behind the operator's back.
          k if OPAQUE_MAP_KEYS.contains(&k) => {},
          k if PATH_KEYS.contains(&k) => anchor_in_place(child, dir),
          k if PATH_ARRAY_KEYS.contains(&k) => anchor_array(child, dir),
          k if PATH_STRING_OR_ARRAY_KEYS.contains(&k) => {
            if child.is_array() {
              anchor_array(child, dir);
            } else {
              anchor_in_place(child, dir);
            }
          },
          _ => anchor_paths(child, dir),
        }
      }
    },
    Value::Array(items) => {
      for item in items.iter_mut() {
        anchor_paths(item, dir);
      }
    },
    _ => {},
  }
}

/// Anchor exactly one named member of an object, leaving its siblings alone.
fn anchor_member(value: &mut Value, member: &str, dir: &Path) {
  if let Some(child) = value.as_object_mut().and_then(|map| map.get_mut(member)) {
    anchor_in_place(child, dir);
  }
}

fn anchor_map_values(value: &mut Value, dir: &Path) {
  if let Some(map) = value.as_object_mut() {
    for child in map.values_mut() {
      anchor_in_place(child, dir);
    }
  }
}

fn anchor_array(value: &mut Value, dir: &Path) {
  if let Some(items) = value.as_array_mut() {
    for item in items.iter_mut() {
      anchor_in_place(item, dir);
    }
  }
}

/// Anchor only the entries an extension spec resolver would treat as
/// paths. A bare or scoped package specifier (`@acme/ext`) must stay a
/// specifier so `node_modules` resolution still applies.
///
/// `~`-relative entries are expanded HERE rather than left for a later
/// consumer: the extension resolver classifies anything that is not
/// `./`, `../` or absolute as a package name, so an unexpanded
/// `~/.config/...` entry failed with "extension package not found"
/// instead of a missing-file error.
fn anchor_extension_specs(value: &mut Value, dir: &Path) {
  if let Some(items) = value.as_array_mut() {
    for item in items.iter_mut() {
      let Some(text) = item.as_str() else { continue };
      if text.starts_with('~') {
        *item = Value::String(shellexpand::tilde(text).into_owned());
        continue;
      }
      if spec_looks_like_path(text) {
        anchor_in_place(item, dir);
      }
    }
  }
}

/// Mirrors `ferridriver_script::discover`'s path-vs-package rule.
fn spec_looks_like_path(spec: &str) -> bool {
  spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') || spec == "." || spec == ".."
}

fn anchor_in_place(value: &mut Value, dir: &Path) {
  if let Some(text) = value.as_str()
    && let Some(resolved) = anchored(dir, text)
  {
    *value = Value::String(resolved);
  }
}

/// The absolute form of `text` against `dir`, or `None` when it must be
/// left alone: already absolute, `~`-relative (expanded later, by the
/// consumer that knows about `~`), or carrying a `${VAR}` template a
/// caller substitutes before use.
pub(super) fn anchored(dir: &Path, text: &str) -> Option<String> {
  // `has_root`, not `is_absolute`: on Windows `/abs` names the current
  // drive's root, which is not a path relative to the config.
  if text.is_empty() || text.starts_with('~') || text.contains("${") || Path::new(text).has_root() {
    return None;
  }
  Some(normalize_path(&dir.join(text)).to_string_lossy().into_owned())
}

pub(super) fn resolve_against(dir: &Path, entry: &str) -> PathBuf {
  let expanded = shellexpand::tilde(entry);
  let path = Path::new(expanded.as_ref());
  if path.is_absolute() {
    path.to_path_buf()
  } else {
    normalize_path(&dir.join(path))
  }
}

/// Lexically remove `.` and `..` components. Unlike `canonicalize`
/// this does not require the path to exist, which matters for output
/// directories a run has yet to create.
///
/// A `..` that would climb past the root is DROPPED, as the OS drops it
/// (`/..` is `/`). Keeping it produced paths like `/../private/tmp/x`,
/// which name the same file but compare as a prefix of nothing.
#[must_use]
pub fn normalize_path(path: &Path) -> PathBuf {
  let mut out = PathBuf::new();
  let mut rooted = false;
  for component in path.components() {
    match component {
      std::path::Component::CurDir => {},
      std::path::Component::ParentDir => {
        if !out.pop() && !rooted {
          out.push("..");
        }
      },
      other => {
        rooted |= matches!(other, std::path::Component::RootDir | std::path::Component::Prefix(_));
        out.push(other.as_os_str());
      },
    }
  }
  out
}

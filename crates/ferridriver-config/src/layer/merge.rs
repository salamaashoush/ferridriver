//! Config document deep-merging, provenance recording, and environment overrides.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value};

use super::{APPEND_KEYS, ConfigWarning, Origin, Provenance};

/// Fold `overlay` onto `base` by the rules stacked config layers fold
/// by: objects merge key by key, the [`APPEND_KEYS`] arrays concatenate,
/// and every other value is replaced. `defineConfig` folds a config
/// module's documents with this, so a `.ts` config combines exactly as
/// `.toml` layers do. The whole module is one layer, so where each key
/// came from is the module's file, recorded when the layer is applied.
pub fn merge_documents(base: &mut Value, overlay: &Value) {
  merge(
    base,
    overlay,
    "",
    &Origin::File(PathBuf::new()),
    &mut Provenance::default(),
  );
}

/// Deep-merge `overlay` into `base`, recording provenance for every
/// leaf the overlay wins.
pub(super) fn merge(base: &mut Value, overlay: &Value, prefix: &str, origin: &Origin, provenance: &mut Provenance) {
  match (base, overlay) {
    (Value::Object(base_map), Value::Object(overlay_map)) => {
      for (key, value) in overlay_map {
        let path = if prefix.is_empty() {
          key.clone()
        } else {
          format!("{prefix}.{key}")
        };
        let appends = value.is_array() && APPEND_KEYS.contains(&key.as_str());
        match base_map.get_mut(key) {
          Some(existing) if appends => {
            let added = append_unique(existing, value);
            if added {
              provenance.contributed(&path, origin);
            }
            provenance.win(&path, origin);
          },
          Some(existing) => merge(existing, value, &path, origin, provenance),
          None => {
            if appends {
              provenance.contributed(&path, origin);
            }
            base_map.insert(key.clone(), value.clone());
            record_leaves(value, &path, origin, provenance);
          },
        }
      }
    },
    (base, _) => {
      *base = overlay.clone();
      provenance.win(prefix, origin);
    },
  }
}

/// Concatenate `overlay` onto `existing`, dropping entries already
/// present so a repeated layer (or a diamond `extends`) cannot
/// duplicate a Chrome flag or an extension path.
/// Returns whether this overlay actually added anything, so a layer that
/// only repeats entries an earlier one already supplied is not reported
/// as a contributor.
pub(super) fn append_unique(existing: &mut Value, overlay: &Value) -> bool {
  let (Some(target), Some(items)) = (existing.as_array_mut(), overlay.as_array()) else {
    *existing = overlay.clone();
    return true;
  };
  let mut added = false;
  for item in items {
    if !target.contains(item) {
      target.push(item.clone());
      added = true;
    }
  }
  added
}

/// Record provenance for every leaf under a freshly inserted subtree.
fn record_leaves(value: &Value, prefix: &str, origin: &Origin, provenance: &mut Provenance) {
  match value {
    Value::Object(map) if !map.is_empty() => {
      for (key, child) in map {
        let path = if prefix.is_empty() {
          key.clone()
        } else {
          format!("{prefix}.{key}")
        };
        record_leaves(child, &path, origin, provenance);
      }
    },
    _ => {
      // The FIRST layer to supply an additive array is a contributor too.
      // Recording only the later layers that appended to it left a
      // two-layer `chromeArgs` looking like it came from one file.
      let leaf = prefix.rsplit('.').next().unwrap_or(prefix);
      if value.is_array() && APPEND_KEYS.contains(&leaf) {
        provenance.contributed(prefix, origin);
      }
      provenance.win(prefix, origin);
    },
  }
}

/// Apply `FERRIDRIVER_<SECTION>__<KEY>` overrides.
///
/// `__` separates nesting levels and a single `_` inside a segment is
/// a camelCase boundary, so
/// `FERRIDRIVER_MCP__BROWSER__INSTANCE_ARGS_COMMAND` sets
/// `mcp.browser.instanceArgsCommand`. Values parse as JSON when they
/// can (`true`, `7`, `["--flag"]`) and stay strings otherwise.
///
/// A name with no `__` is NOT a config key: the legacy single-segment
/// variables (`FERRIDRIVER_WORKERS`, `FERRIDRIVER_BACKEND`, ...) keep
/// their existing meaning in the test runner, and `FERRIDRIVER_CONFIG`
/// / `FERRIDRIVER_NO_INHERIT` / `FERRIDRIVER_DEBUG` are loader
/// controls. The two top-level path keys are the documented exception.
pub(super) fn apply_env(
  env: &BTreeMap<String, String>,
  document: &mut Value,
  provenance: &mut Provenance,
  warnings: &mut Vec<ConfigWarning>,
) {
  const TOP_LEVEL: &[(&str, &str)] = &[
    ("FERRIDRIVER_SCRIPT_ROOT", "scriptRoot"),
    ("FERRIDRIVER_ARTIFACTS_ROOT", "artifactsRoot"),
  ];

  for (name, raw) in env {
    let Some(rest) = name.strip_prefix("FERRIDRIVER_") else {
      continue;
    };
    let dotted = if rest.contains("__") {
      rest.split("__").map(camel_case_segment).collect::<Vec<_>>().join(".")
    } else if let Some((_, key)) = TOP_LEVEL.iter().find(|(var, _)| var == name) {
      (*key).to_string()
    } else {
      continue;
    };

    if dotted.split('.').any(str::is_empty) {
      warnings.push(ConfigWarning {
        source: format!("${name}"),
        message: "empty key segment (check for a stray `__`)".to_string(),
      });
      continue;
    }

    let value = parse_env_value(raw);
    let overlay = nest(&dotted, value);
    merge(document, &overlay, "", &Origin::Env(name.clone()), provenance);
  }
}

/// `INSTANCE_ARGS_COMMAND` -> `instanceArgsCommand`.
pub(super) fn camel_case_segment(segment: &str) -> String {
  let mut out = String::with_capacity(segment.len());
  for (idx, word) in segment.split('_').filter(|w| !w.is_empty()).enumerate() {
    let lower = word.to_ascii_lowercase();
    if idx == 0 {
      out.push_str(&lower);
      continue;
    }
    let mut chars = lower.chars();
    if let Some(first) = chars.next() {
      out.extend(first.to_uppercase());
      out.push_str(chars.as_str());
    }
  }
  out
}

pub(super) fn parse_env_value(raw: &str) -> Value {
  serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// Build `{a: {b: value}}` from `"a.b"`.
pub(super) fn nest(dotted: &str, value: Value) -> Value {
  let mut segments: Vec<&str> = dotted.split('.').collect();
  let mut current = value;
  while let Some(key) = segments.pop() {
    let mut map = Map::new();
    map.insert(key.to_string(), current);
    current = Value::Object(map);
  }
  current
}

//! The `ferridriver` field of an extension package's `package.json`.
//!
//! An extension package is an npm-shaped directory: a `package.json`
//! plus source. Node's own entry fields (`exports` / `module` / `main`)
//! describe ONE entry, which is wrong for an extension package — a
//! package that ships five tool files plus a `lib/` of shared helpers
//! has five entries, and the helpers must be bundled through the
//! entries' imports rather than loaded as extensions in their own right
//! (each would be warned about for declaring no tools).
//!
//! ```json
//! {
//!   "name": "@acme/ferridriver-acme",
//!   "type": "module",
//!   "ferridriver": {
//!     "entries": ["./src/login.ts", "./src/sign.ts"],
//!     "requires": {
//!       "commands": ["acme-cli"],
//!       "env": ["ACME_HOME"],
//!       "net": ["*.acme.com"],
//!       "sidecars": ["acme-gate"]
//!     },
//!     "settings": {
//!       "acme": { "type": "object", "properties": { "origin": { "type": "string" } } }
//!     }
//!   }
//! }
//! ```
//!
//! `requires` is the package's half of a contract: what the host must
//! already provide for the package to work at all. It grants nothing —
//! per-tool authority still comes from `defineTool`'s `allow`, clamped
//! by `[extensions.policy]` — but it turns four silent runtime failures
//! (a missing binary, an unlisted `allowEnv` name, a host the operator
//! ceiling forbids, an undeclared sidecar) into load-time diagnostics.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The key an extension package declares its manifest under.
pub const MANIFEST_KEY: &str = "ferridriver";

/// Parsed `package.json` -> `ferridriver` manifest.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionManifest {
  /// Manifest version the package was written against. Absent ⇒ 1, the
  /// shape that shipped before packages could claim a specifier.
  pub api_version: Option<u32>,
  /// The package's own name, for diagnostics that must say WHICH package
  /// claimed a specifier. Falls back to the directory name when absent
  /// (`package.json`'s own `name` is not part of this manifest).
  pub name: Option<String>,
  /// Entry modules to load as extensions, in declaration order. Each is
  /// a path relative to the package directory (a file, with the
  /// extension optional, or a directory scanned recursively). Anything
  /// not named here is reachable only as an import of an entry, which is
  /// what keeps a `lib/` tree out of the extension list.
  ///
  /// A bare string is the common case. The object form narrows an entry
  /// to some hosts, or gives it preconditions the rest of the package
  /// does not share.
  pub entries: Vec<ExtensionEntry>,
  /// Import specifiers this package serves, and how.
  pub provides: ExtensionProvides,
  /// Host preconditions. See the module docs: declarations, not grants.
  pub requires: ExtensionRequires,
  /// JSON Schema per `[extensions.settings.<key>]` block the package
  /// reads, keyed the same way settings are resolved (tool namespace, or
  /// a full tool name). Validated against the operator's actual settings
  /// at load, so a mistyped key is an error instead of an `undefined`
  /// the handler reads at 3am.
  pub settings: BTreeMap<String, serde_json::Value>,
}

/// Every host an entry may be narrowed to, in the order a report lists
/// them.
///
/// The canonical set lives here rather than in the script crate because
/// the manifest is parsed before any host exists;
/// `ferridriver_script::ExtensionHost` is pinned against it by a test so
/// the two cannot drift.
pub const EXTENSION_HOSTS: &[&str] = &["mcp", "bdd", "test", "script"];

/// One `ferridriver.entries` item.
///
/// Written as a bare string almost always. The object form exists for
/// the two things a string cannot say: that an entry belongs to some
/// hosts and not others, and that it needs something the rest of the
/// package does not.
///
/// Scoping `requires` to the entry is what keeps a narrow declaration
/// from being expensive: an MCP-only entry that names a binary in
/// `commands` used to block its WHOLE package when that binary was
/// absent, taking down fixtures and providers that never needed it,
/// even on a host where the entry does not load at all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionEntry {
  /// Path relative to the package directory.
  pub path: String,
  /// Hosts this entry loads under. Empty means every host.
  pub hosts: Vec<String>,
  /// Preconditions for THIS entry, replacing the package's own when
  /// present rather than adding to them.
  pub requires: Option<ExtensionRequires>,
}

impl From<&str> for ExtensionEntry {
  fn from(path: &str) -> Self {
    Self {
      path: path.to_string(),
      hosts: Vec::new(),
      requires: None,
    }
  }
}

impl ExtensionEntry {
  /// Whether this entry loads under `host`.
  #[must_use]
  pub fn runs_under(&self, host: &str) -> bool {
    self.hosts.is_empty() || self.hosts.iter().any(|h| h == host)
  }

  /// The preconditions to check for this entry, given its package's.
  #[must_use]
  pub fn effective_requires<'a>(&'a self, package: &'a ExtensionRequires) -> &'a ExtensionRequires {
    self.requires.as_ref().unwrap_or(package)
  }
}

/// The object form's fields, so a typo inside it is named instead of
/// collapsing into "did not match any variant".
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct EntryObject {
  path: String,
  #[serde(default)]
  hosts: Vec<String>,
  #[serde(default)]
  requires: Option<ExtensionRequires>,
}

impl<'de> Deserialize<'de> for ExtensionEntry {
  fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
    struct EntryVisitor;

    impl<'de> serde::de::Visitor<'de> for EntryVisitor {
      type Value = ExtensionEntry;

      fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an entry path, or an object with `path` and optionally `hosts` / `requires`")
      }

      fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(ExtensionEntry {
          path: value.to_string(),
          hosts: Vec::new(),
          requires: None,
        })
      }

      fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        let object = EntryObject::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
        for host in &object.hosts {
          if !EXTENSION_HOSTS.contains(&host.as_str()) {
            return Err(serde::de::Error::custom(format!(
              "entry `{}`: unknown host `{host}` (expected one of {})",
              object.path,
              EXTENSION_HOSTS.join(", ")
            )));
          }
        }
        Ok(ExtensionEntry {
          path: object.path,
          hosts: object.hosts,
          requires: object.requires,
        })
      }
    }

    deserializer.deserialize_any(EntryVisitor)
  }
}

impl Serialize for ExtensionEntry {
  fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    // Round-trips to what the author wrote: an entry that narrows
    // nothing goes back out as the string it came in as.
    if self.hosts.is_empty() && self.requires.is_none() {
      return serializer.serialize_str(&self.path);
    }
    let mut map = serializer.serialize_map(None)?;
    map.serialize_entry("path", &self.path)?;
    if !self.hosts.is_empty() {
      map.serialize_entry("hosts", &self.hosts)?;
    }
    if let Some(requires) = &self.requires {
      map.serialize_entry("requires", requires)?;
    }
    map.end()
  }
}

/// What a package SERVES: import specifiers other modules — a spec, a
/// step file, another extension — may import and have resolved to this
/// package's own code.
///
/// This is the mechanism that lets a suite written against some other
/// package run unmodified: the specifier it imports is claimed here and
/// answered by one module instance per VM, rather than being a file the
/// suite had to be edited to point at.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionProvides {
  /// `specifier -> module file`, relative to the package directory. The
  /// file is bundled with the package and evaluated before anything that
  /// imports it.
  pub modules: BTreeMap<String, String>,
  /// `specifier -> another specifier this package also provides`. An
  /// alias may not target a specifier the package does not own — a
  /// package cannot re-point someone else's name, nor a native one.
  pub aliases: BTreeMap<String, String>,
}

impl ExtensionProvides {
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.modules.is_empty() && self.aliases.is_empty()
  }
}

/// What a package needs from its host.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ExtensionRequires {
  /// Programs that must be on `PATH` — the binaries the package's
  /// `allow.commands` templates execute.
  pub commands: Vec<String>,
  /// `process.env` names the operator must expose through
  /// `[scripting].allowEnv`. A script cannot self-grant these.
  pub env: Vec<String>,
  /// Host patterns the package's tools target. Must fit inside the
  /// `[extensions.policy]` net ceiling, or none of its HTTP can work.
  pub net: Vec<String>,
  /// `[[sidecars]]` names the package will `sidecars.connect(...)`.
  pub sidecars: Vec<String>,
}

impl ExtensionManifest {
  /// Read the `ferridriver` field out of a parsed `package.json`.
  ///
  /// `Ok(None)` means the package carries no manifest (a plain npm
  /// package, resolved through Node's own entry fields).
  ///
  /// # Errors
  ///
  /// Returns the serde message when the field is present but malformed
  /// — including an unknown key, which is almost always a typo in a
  /// field the author expected to take effect.
  pub fn from_package_json(json: &serde_json::Value) -> Result<Option<Self>, String> {
    let Some(raw) = json.get(MANIFEST_KEY) else {
      return Ok(None);
    };
    let manifest: Self = serde_path_to_error::deserialize(raw).map_err(|e| {
      let path = e.path().to_string();
      let inner = e.into_inner();
      if path.is_empty() || path == "." {
        format!("`{MANIFEST_KEY}` is invalid: {inner}")
      } else {
        format!("`{MANIFEST_KEY}.{path}` is invalid: {inner}")
      }
    })?;
    Ok(Some(manifest))
  }

  /// True when the manifest declares nothing at all (`"ferridriver": {}`).
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.entries.is_empty() && self.requires.is_empty() && self.settings.is_empty() && self.provides.is_empty()
  }
}

impl ExtensionRequires {
  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.commands.is_empty() && self.env.is_empty() && self.net.is_empty() && self.sidecars.is_empty()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn pkg(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).expect("package.json fixture")
  }

  #[test]
  fn absent_field_is_none() {
    let parsed = ExtensionManifest::from_package_json(&pkg(r#"{"name":"x","type":"module"}"#)).expect("parse");
    assert!(parsed.is_none());
  }

  #[test]
  fn full_manifest_parses_camel_case() {
    let parsed = ExtensionManifest::from_package_json(&pkg(
      r#"{
        "name": "@acme/ext",
        "ferridriver": {
          "entries": ["./src/a.ts", "./src/b.ts"],
          "requires": {
            "commands": ["acme-cli"],
            "env": ["ACME_HOME"],
            "net": ["*.acme.com"],
            "sidecars": ["acme-gate"]
          },
          "settings": { "acme": { "type": "object" } }
        }
      }"#,
    ))
    .expect("parse")
    .expect("manifest present");

    assert_eq!(
      parsed.entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
      ["./src/a.ts", "./src/b.ts"],
      "entry order is preserved"
    );
    assert_eq!(parsed.requires.commands, ["acme-cli"]);
    assert_eq!(parsed.requires.env, ["ACME_HOME"]);
    assert_eq!(parsed.requires.net, ["*.acme.com"]);
    assert_eq!(parsed.requires.sidecars, ["acme-gate"]);
    assert_eq!(parsed.settings["acme"]["type"], "object");
    assert!(!parsed.is_empty());
  }

  #[test]
  fn empty_manifest_is_reported_empty() {
    let parsed = ExtensionManifest::from_package_json(&pkg(r#"{"ferridriver":{}}"#))
      .expect("parse")
      .expect("present");
    assert!(parsed.is_empty());
  }

  #[test]
  fn unknown_key_is_an_error_not_a_silent_drop() {
    let err =
      ExtensionManifest::from_package_json(&pkg(r#"{"ferridriver":{"entrys":["./a.ts"]}}"#)).expect_err("must fail");
    assert!(err.contains("entrys"), "the typo must be named: {err}");
  }

  #[test]
  fn unknown_requires_key_is_an_error() {
    let err = ExtensionManifest::from_package_json(&pkg(r#"{"ferridriver":{"requires":{"exec":["a"]}}}"#))
      .expect_err("must fail");
    assert!(err.contains("exec"), "{err}");
  }

  #[test]
  fn wrong_type_is_an_error() {
    let err =
      ExtensionManifest::from_package_json(&pkg(r#"{"ferridriver":{"entries":"./a.ts"}}"#)).expect_err("must fail");
    assert!(err.contains("entries"), "{err}");
  }

  #[test]
  fn roundtrips_through_json() {
    let manifest = ExtensionManifest {
      entries: vec!["./src/a.ts".into()],
      requires: ExtensionRequires {
        commands: vec!["acme".into()],
        ..Default::default()
      },
      settings: BTreeMap::from([("acme".to_string(), serde_json::json!({"type":"object"}))]),
      ..Default::default()
    };
    let json = serde_json::to_value(&manifest).expect("serialize");
    assert_eq!(json["requires"]["commands"][0], "acme");
    let back: ExtensionManifest = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, manifest);
  }
  #[test]
  fn an_entry_is_a_string_or_an_object() {
    let parsed = ExtensionManifest::from_package_json(&pkg(
      r#"{"ferridriver":{"entries":[
        "./src/plain.ts",
        {"path":"./src/mcp-only.ts","hosts":["mcp"]},
        {"path":"./src/gated.ts","requires":{"commands":["acme-cli"]}}
      ]}}"#,
    ))
    .expect("parse")
    .expect("manifest");

    assert_eq!(parsed.entries[0].path, "./src/plain.ts");
    assert!(parsed.entries[0].hosts.is_empty(), "a bare string narrows nothing");
    assert!(parsed.entries[0].requires.is_none());

    assert_eq!(parsed.entries[1].hosts, ["mcp"]);
    assert!(parsed.entries[1].runs_under("mcp"));
    assert!(
      !parsed.entries[1].runs_under("test"),
      "a narrowed entry stays where it was put"
    );
    assert!(
      parsed.entries[0].runs_under("test"),
      "an unnarrowed entry runs everywhere"
    );

    let package = ExtensionRequires {
      commands: vec!["package-wide".into()],
      ..Default::default()
    };
    assert_eq!(
      parsed.entries[2].effective_requires(&package).commands,
      ["acme-cli"],
      "an entry's own requires REPLACE the package's rather than adding to them"
    );
    assert_eq!(
      parsed.entries[0].effective_requires(&package).commands,
      ["package-wide"],
      "an entry with none falls back to the package's"
    );
  }

  #[test]
  fn an_unknown_host_is_named() {
    let err = ExtensionManifest::from_package_json(&pkg(
      r#"{"ferridriver":{"entries":[{"path":"./a.ts","hosts":["mpc"]}]}}"#,
    ))
    .expect_err("unknown host");
    assert!(err.contains("mpc"), "{err}");
    assert!(err.contains("mcp"), "the expected set is listed: {err}");
  }

  #[test]
  fn a_typo_inside_the_object_form_names_the_key() {
    let err = ExtensionManifest::from_package_json(&pkg(
      r#"{"ferridriver":{"entries":[{"path":"./a.ts","host":["mcp"]}]}}"#,
    ))
    .expect_err("unknown key");
    assert!(err.contains("host"), "{err}");
  }

  #[test]
  fn an_entry_serializes_back_to_the_form_it_was_written_in() {
    let manifest = ExtensionManifest {
      entries: vec![
        "./src/plain.ts".into(),
        ExtensionEntry {
          path: "./src/mcp.ts".to_string(),
          hosts: vec!["mcp".to_string()],
          requires: None,
        },
      ],
      ..Default::default()
    };
    let json = serde_json::to_value(&manifest).expect("serialize");
    assert_eq!(
      json["entries"][0], "./src/plain.ts",
      "an unnarrowed entry stays a string"
    );
    assert_eq!(json["entries"][1]["path"], "./src/mcp.ts");
    assert_eq!(json["entries"][1]["hosts"][0], "mcp");
    let back: ExtensionManifest = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, manifest);
  }
}

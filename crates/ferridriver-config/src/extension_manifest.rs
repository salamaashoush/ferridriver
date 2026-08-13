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
  /// Entry modules to load as extensions, in declaration order. Each is
  /// a path relative to the package directory (a file, with the
  /// extension optional, or a directory scanned recursively). Anything
  /// not named here is reachable only as an import of an entry, which is
  /// what keeps a `lib/` tree out of the extension list.
  pub entries: Vec<String>,
  /// Host preconditions. See the module docs: declarations, not grants.
  pub requires: ExtensionRequires,
  /// JSON Schema per `[extensions.settings.<key>]` block the package
  /// reads, keyed the same way settings are resolved (tool namespace, or
  /// a full tool name). Validated against the operator's actual settings
  /// at load, so a mistyped key is an error instead of an `undefined`
  /// the handler reads at 3am.
  pub settings: BTreeMap<String, serde_json::Value>,
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
    self.entries.is_empty() && self.requires.is_empty() && self.settings.is_empty()
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

    assert_eq!(parsed.entries, ["./src/a.ts", "./src/b.ts"], "entry order is preserved");
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
    };
    let json = serde_json::to_value(&manifest).expect("serialize");
    assert_eq!(json["requires"]["commands"][0], "acme");
    let back: ExtensionManifest = serde_json::from_value(json).expect("deserialize");
    assert_eq!(back, manifest);
  }
}

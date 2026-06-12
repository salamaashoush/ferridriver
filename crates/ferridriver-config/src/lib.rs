//! Unified ferridriver configuration.
//!
//! Defines the canonical schema for `ferridriver.toml` and exposes typed
//! sub-sections that downstream crates (`ferridriver-mcp`, `ferridriver-test`,
//! `ferridriver-bdd`) consume.
//!
//! # Layout
//!
//! ```toml
//! # ferridriver.toml
//!
//! [mcp]
//! [mcp.server]
//! name = "my-server"
//!
//! [mcp.browser]
//! backend = "cdp-pipe"
//! headless = true
//!
//! [test]
//! testMatch = ["**/*.spec.ts"]
//! workers = 4
//!
//! [test.browser]
//! browser = "chromium"
//! ```
//!
//! # Search order
//!
//! 1. Explicit path passed by the caller.
//! 2. `./ferridriver.{toml,yaml,yml,json}` in the current working directory.
//! 3. `~/.config/ferridriver/config.{toml,yaml,yml,json}`.

pub mod command_spec;
pub mod mcp;
pub mod test;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Top-level configuration document.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FerridriverConfig {
  /// Extension files (plugins): each a single `.js`/`.mjs`/`.ts`/`.mts`
  /// file or a directory scanned shallowly for those. An extension
  /// registers MCP tools (`tool`) and/or BDD steps
  /// (`Given`/`When`/`Then`); the MCP server consumes its tools and the
  /// test runner consumes its steps. Top-level (not under `mcp`) because
  /// both hosts load it.
  pub extensions: Vec<String>,
  /// Declared sidecar processes, exposed to scripts as
  /// `sidecars.connect(name)`. Top-level (sibling of `[mcp]` / `[test]`)
  /// because both the MCP server / `run` path and the test runner consume
  /// them. Connecting is by declared name only — a script can never spawn
  /// an arbitrary process.
  pub sidecars: Vec<Sidecar>,
  /// Sandbox-relaxation knobs for the scripting VM (default-deny).
  pub scripting: ScriptingConfig,
  /// Options for the rolldown bundling pipeline (BDD step files,
  /// extensions, `ferridriver run` scripts). Top-level because every
  /// host that bundles consumes it.
  pub bundler: BundlerConfig,
  /// MCP server configuration.
  pub mcp: mcp::McpConfig,
  /// Test runner configuration.
  pub test: test::TestConfig,
  /// Directory of the config file this document was loaded from (set by
  /// [`Self::load_from`]); `None` for a default/in-memory config.
  /// Relative paths inside the document (e.g. `bundler.alias` targets)
  /// resolve against it.
  #[serde(skip)]
  pub source_dir: Option<PathBuf>,
}

/// Options for the rolldown bundling pipeline.
///
/// ```toml
/// [bundler.alias]
/// "@wdio/utils" = "./shims/wdio-utils.ts"
///
/// [bundler.virtualModules]
/// "box:env" = "export const env = 'staging';"
/// ```
#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct BundlerConfig {
  /// Redirect a bare import specifier to a shim file. The target is a
  /// `.js`/`.mjs`/`.ts`/`.mts` path, resolved against the config file's
  /// directory (or the process cwd when no config file exists). The
  /// shim is bundled and transpiled like any other source file. Lets
  /// legacy imports (e.g. a WDIO helper package) be served by local
  /// compatibility shims without forking the importing code.
  pub alias: BTreeMap<String, String>,
  /// Virtual modules: import specifier -> inline ES-module JS source.
  /// The specifier never touches the filesystem. For TypeScript or
  /// multi-file shims use `alias` instead.
  pub virtual_modules: BTreeMap<String, String>,
}

/// One declared sidecar process. Driven over fd 3/4 with NUL-delimited
/// JSON by `ferridriver-script`'s sidecar transport. `command[0]` is the
/// program; the rest are its arguments (fd 3/4 are wired by the transport,
/// not via argv).
#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Sidecar {
  /// The name scripts connect by (`sidecars.connect("<name>")`). Must be
  /// unique across all declared sidecars.
  pub name: String,
  /// Program + arguments. Must be non-empty.
  pub command: Vec<String>,
  /// Extra environment variables for the child (merged onto the inherited
  /// environment). Keys are used verbatim (not camelCased).
  pub env: Option<BTreeMap<String, String>>,
  /// Working directory for the child. Defaults to the parent's cwd.
  pub cwd: Option<String>,
}

/// Opt-in relaxations of the scripting sandbox. Every field defaults to
/// the locked-down value; an operator who widens it is stating they
/// understand the exposure — same posture as `allow.net`.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ScriptingConfig {
  /// Server environment variable names a script may read via
  /// `process.env`. Empty (default) ⇒ `process.env` is `{}`. Only names
  /// listed here, and only if present in the server's environment, are
  /// exposed — a script never sees an ambient secret the operator did
  /// not name.
  pub allow_env: Vec<String>,
  /// Capability grants for first-party scripts and BDD step files.
  /// Plugins/tools do not inherit these automatically; they must opt in
  /// through their own `allow.commands` manifest.
  pub allow: ScriptingAllow,
}

/// First-party scripting capability grants.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ScriptingAllow {
  /// Named commands exposed through `ferridriver.commands` /
  /// `commands` to `ferridriver run`, MCP `run_script`, and BDD step
  /// files. The command schema is intentionally the same as plugin
  /// `allow.commands`.
  pub commands: BTreeMap<String, command_spec::CommandSpec>,
}

pub use command_spec::{CommandOutput, CommandRun, CommandSpec, ResolvedCommand, ResolvedExec};

impl FerridriverConfig {
  /// Load the unified configuration document.
  ///
  /// If `explicit` is `Some`, that path is read directly and the format is
  /// inferred from the file extension. Otherwise the standard search paths
  /// are tried; if none exist, `Self::default()` is returned.
  ///
  /// # Errors
  ///
  /// Returns an error if a file is found but cannot be read or parsed.
  pub fn load(explicit: Option<&Path>) -> anyhow::Result<Self> {
    let path = match explicit {
      Some(p) => Some(p.to_path_buf()),
      None => find_default_path(),
    };

    let Some(path) = path else {
      return Ok(Self::default());
    };

    Self::load_from(&path)
  }

  /// Load the unified configuration from an explicit file path.
  ///
  /// # Errors
  ///
  /// Returns an error if the file cannot be read or parsed.
  pub fn load_from(path: &Path) -> anyhow::Result<Self> {
    let content =
      std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("failed to read config {}: {e}", path.display()))?;

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("toml");
    let mut cfg: FerridriverConfig = match ext {
      "toml" => toml::from_str(&content).map_err(|e| anyhow::anyhow!("invalid TOML config {}: {e}", path.display()))?,
      "yaml" | "yml" => {
        serde_yaml::from_str(&content).map_err(|e| anyhow::anyhow!("invalid YAML config {}: {e}", path.display()))?
      },
      "json" => {
        serde_json::from_str(&content).map_err(|e| anyhow::anyhow!("invalid JSON config {}: {e}", path.display()))?
      },
      other => anyhow::bail!("unsupported config format: {other} (expected toml/yaml/yml/json)"),
    };

    cfg
      .validate()
      .map_err(|e| anyhow::anyhow!("invalid config {}: {e}", path.display()))?;
    cfg.source_dir = path.parent().map(Path::to_path_buf);

    tracing::debug!("loaded ferridriver config from {}", path.display());
    Ok(cfg)
  }

  /// Validate cross-field invariants the serde layer can't express.
  ///
  /// # Errors
  ///
  /// Returns an error if two sidecars share a `name`, or a sidecar has an
  /// empty `command`.
  pub fn validate(&self) -> anyhow::Result<()> {
    let mut seen = std::collections::HashSet::new();
    for s in &self.sidecars {
      if s.command.is_empty() {
        anyhow::bail!("sidecar '{}' has an empty command", s.name);
      }
      if !seen.insert(s.name.as_str()) {
        anyhow::bail!("duplicate sidecar name '{}'", s.name);
      }
    }
    Ok(())
  }
}

/// Search the cwd and `~/.config/ferridriver/` for the canonical config file.
#[must_use]
pub fn find_default_path() -> Option<PathBuf> {
  let exts = ["toml", "yaml", "yml", "json"];

  for ext in &exts {
    let p = PathBuf::from(format!("ferridriver.{ext}"));
    if p.exists() {
      return Some(p);
    }
  }

  if let Some(cd) = dirs::config_dir() {
    let dir = cd.join("ferridriver");
    for ext in &exts {
      let p = dir.join(format!("config.{ext}"));
      if p.exists() {
        return Some(p);
      }
    }
  }

  None
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn default_root_is_empty() {
    let root = FerridriverConfig::default();
    assert_eq!(root.mcp.server_name(), "ferridriver");
    assert!(root.test.test_match.is_empty());
  }

  #[test]
  fn bundler_section_parses_and_source_dir_is_recorded() {
    let dir = std::env::temp_dir().join("ferridriver-config-bundler-ok");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ferridriver.toml");
    std::fs::write(
      &path,
      r#"
[bundler.alias]
"@wdio/utils" = "./shims/wdio-utils.ts"

[bundler.virtualModules]
"box:env" = "export const env = 'staging';"
"#,
    )
    .unwrap();

    let root = FerridriverConfig::load_from(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
      root.bundler.alias.get("@wdio/utils").map(String::as_str),
      Some("./shims/wdio-utils.ts")
    );
    assert_eq!(
      root.bundler.virtual_modules.get("box:env").map(String::as_str),
      Some("export const env = 'staging';")
    );
    assert_eq!(root.source_dir.as_deref(), Some(dir.as_path()));
  }

  #[test]
  fn load_toml_with_both_sections() {
    let dir = std::env::temp_dir().join("ferridriver-config-toml-both");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ferridriver.toml");
    std::fs::write(
      &path,
      r#"
[mcp.server]
name = "unified-test"

[mcp.browser]
backend = "cdp-raw"
headless = true

[test]
workers = 7
testMatch = ["tests/**/*.spec.ts"]

[test.browser]
browser = "chromium"
backend = "cdp-pipe"
"#,
    )
    .unwrap();

    let root = FerridriverConfig::load_from(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(root.mcp.server_name(), "unified-test");
    assert!(root.mcp.headless());
    assert_eq!(root.test.workers, 7);
    assert_eq!(root.test.test_match, vec!["tests/**/*.spec.ts"]);
  }

  #[test]
  fn load_yaml_with_both_sections() {
    let dir = std::env::temp_dir().join("ferridriver-config-yaml-both");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ferridriver.yaml");
    std::fs::write(
      &path,
      r#"
mcp:
  server:
    name: "yaml-unified"
  browser:
    headless: true
test:
  workers: 5
"#,
    )
    .unwrap();

    let root = FerridriverConfig::load_from(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(root.mcp.server_name(), "yaml-unified");
    assert!(root.mcp.headless());
    assert_eq!(root.test.workers, 5);
  }

  #[test]
  fn load_json_with_both_sections() {
    let dir = std::env::temp_dir().join("ferridriver-config-json-both");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ferridriver.json");
    std::fs::write(
      &path,
      r#"{
        "mcp": { "server": { "name": "json-unified" } },
        "test": { "workers": 9 }
      }"#,
    )
    .unwrap();

    let root = FerridriverConfig::load_from(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(root.mcp.server_name(), "json-unified");
    assert_eq!(root.test.workers, 9);
  }

  #[test]
  fn serde_json_roundtrip_default() {
    let root = FerridriverConfig::default();
    let json = serde_json::to_value(&root).expect("serialize default");
    let parsed: FerridriverConfig = serde_json::from_value(json.clone()).expect("deserialize back");
    let json2 = serde_json::to_value(&parsed).expect("serialize parsed");
    assert_eq!(json, json2, "default config should round-trip cleanly through JSON");
  }

  #[test]
  fn serde_json_roundtrip_populated() {
    let mut root = FerridriverConfig::default();
    root.mcp.server.name = Some("custom".into());
    root.mcp.browser.backend = Some("cdp-raw".into());
    root.mcp.browser.headless = Some(true);
    root.mcp.browser.chrome_args = vec!["--no-sandbox".into()];
    root.test.workers = 4;
    root.test.timeout = 60_000;
    root.test.test_match = vec!["custom/**/*.spec.ts".into()];
    root.test.browser.headless = true;
    root.test.browser.use_options.is_mobile = true;
    root.test.browser.use_options.locale = Some("en-GB".into());

    let json = serde_json::to_value(&root).expect("serialize populated");
    let parsed: FerridriverConfig = serde_json::from_value(json.clone()).expect("deserialize populated");
    let json2 = serde_json::to_value(&parsed).expect("serialize roundtripped");
    assert_eq!(json, json2, "populated config should round-trip");

    assert_eq!(parsed.mcp.server.name.as_deref(), Some("custom"));
    assert_eq!(parsed.mcp.browser.backend.as_deref(), Some("cdp-raw"));
    assert_eq!(parsed.mcp.browser.headless, Some(true));
    assert_eq!(parsed.test.workers, 4);
    assert!(parsed.test.browser.headless);
    assert!(parsed.test.browser.use_options.is_mobile);
  }

  #[test]
  fn load_toml_with_sidecars() {
    let dir = std::env::temp_dir().join("ferridriver-config-sidecars-ok");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ferridriver.toml");
    std::fs::write(
      &path,
      r#"
[[sidecars]]
name = "tooling"
command = ["my-helper", "--serve"]
cwd = "/tmp"

[sidecars.env]
LOG = "debug"
"#,
    )
    .unwrap();

    let root = FerridriverConfig::load_from(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(root.sidecars.len(), 1);
    let s = &root.sidecars[0];
    assert_eq!(s.name, "tooling");
    assert_eq!(s.command, vec!["my-helper", "--serve"]);
    assert_eq!(s.cwd.as_deref(), Some("/tmp"));
    assert_eq!(
      s.env.as_ref().and_then(|e| e.get("LOG")).map(String::as_str),
      Some("debug")
    );
  }

  #[test]
  fn duplicate_sidecar_name_is_an_error() {
    let dir = std::env::temp_dir().join("ferridriver-config-sidecars-dup");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ferridriver.toml");
    std::fs::write(
      &path,
      r#"
[[sidecars]]
name = "dup"
command = ["a"]

[[sidecars]]
name = "dup"
command = ["b"]
"#,
    )
    .unwrap();

    let err = FerridriverConfig::load_from(&path).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(err.to_string().contains("duplicate sidecar name"), "got: {err}");
  }

  #[test]
  fn empty_sidecar_command_is_an_error() {
    let dir = std::env::temp_dir().join("ferridriver-config-sidecars-empty");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ferridriver.toml");
    std::fs::write(
      &path,
      r#"
[[sidecars]]
name = "broken"
command = []
"#,
    )
    .unwrap();

    let err = FerridriverConfig::load_from(&path).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(err.to_string().contains("empty command"), "got: {err}");
  }

  #[test]
  fn unsupported_extension_errors() {
    let dir = std::env::temp_dir().join("ferridriver-config-bad-ext");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ferridriver.ini");
    std::fs::write(&path, "[mcp]\n").unwrap();

    let err = FerridriverConfig::load_from(&path).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(err.to_string().contains("unsupported config format"));
  }
}

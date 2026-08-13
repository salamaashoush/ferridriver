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
//! Files LAYER, lowest precedence first: machine, user, git root,
//! cwd, `*.local.*`, then `-c/--config`, then `FERRIDRIVER_*__*`
//! environment overrides. See [`layer`] for the full stack, the merge
//! rules, and how relative paths are anchored to their own file.

pub mod browser;
pub mod command_spec;
pub mod extension_manifest;
pub mod layer;
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
  ///
  /// Two shapes: the shorthand array of paths, or a table carrying
  /// `paths` plus an operator `policy` ceiling (see
  /// [`ExtensionPolicyConfig`]).
  pub extensions: ExtensionsConfig,
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
  /// Root of the scripting sandbox: every `fs` call and every dynamic
  /// `import` a script makes is confined here. Relative to the config
  /// file that set it. Defaults to `.ferridriver/scripts`.
  ///
  /// Previously only settable by implementing `McpServerConfig` in
  /// Rust, which meant an operator could not move it at all.
  pub script_root: Option<String>,
  /// Root for script outputs (screenshots, PDFs, traces, downloads),
  /// exposed to scripts as `artifacts`. Kept separate from
  /// [`Self::script_root`] so outputs never land in the source tree.
  /// Defaults to `.ferridriver/artifacts`.
  pub artifacts_root: Option<String>,
  /// Scripting-engine limits (per-call timeout, memory ceiling, console
  /// caps, session-VM pool).
  pub engine: EngineConfig,
  /// MCP server configuration.
  pub mcp: mcp::McpConfig,
  /// Test runner configuration.
  pub test: test::TestConfig,
  /// Directory of the highest-precedence config FILE that contributed
  /// to this document; `None` for a default/in-memory config. Paths
  /// inside the document are already anchored to their own layer's
  /// directory, so this is for diagnostics and for resolving values a
  /// caller supplies later (not for re-resolving document paths).
  #[serde(skip)]
  pub source_dir: Option<PathBuf>,
  /// Base directory each `extensions` entry was declared in, keyed by
  /// the entry as written. A package specifier (`@acme/ext`) resolves
  /// through `node_modules` starting here, so an extension declared in
  /// the user layer finds the user layer's packages rather than
  /// whichever repository the process happens to run in.
  #[serde(skip)]
  pub extension_bases: BTreeMap<String, PathBuf>,
}

/// Scripting-engine limits. Every field is optional so the engine's own
/// defaults stay the single source of truth for what "unset" means.
///
/// The session-VM knobs matter to any long-lived MCP server: a session
/// VM holds an extension's module state, and when the pool evicts or
/// reaps one, that state is gone. An operator who needs sessions to
/// survive a long idle gap can now say so.
#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct EngineConfig {
  /// Wall-clock ceiling for one script/tool call, in milliseconds.
  pub timeout_ms: Option<u64>,
  /// Memory ceiling for a session VM, in bytes.
  pub max_memory_bytes: Option<usize>,
  /// Total captured console bytes per call.
  pub max_console_bytes: Option<usize>,
  /// Captured bytes per single console entry.
  pub max_console_entry_bytes: Option<usize>,
  /// How many session VMs stay warm before the least-recently-used idle
  /// one is evicted.
  pub max_session_vms: Option<usize>,
  /// Seconds a session may sit untouched before its VM (and its
  /// durable `vars`) are reaped. `0` disables reaping.
  pub session_idle_ttl_secs: Option<u64>,
}

/// One `extensions` entry plus the directory it was declared in.
///
/// The base directory is what makes a layered `extensions` list work:
/// a relative path means "next to the file that declared it", and a
/// package specifier walks `node_modules` from there.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExtensionSpec {
  /// The entry exactly as configured (already absolute when it was a
  /// relative path; still a bare specifier when it named a package).
  pub spec: String,
  /// Directory of the config layer that declared it.
  pub base_dir: PathBuf,
}

/// The `extensions` key: either the shorthand list of paths or the
/// detailed table with an operator policy.
///
/// ```toml
/// # shorthand
/// extensions = ["./extensions", "./tools/login.ts"]
///
/// # detailed
/// [extensions]
/// paths = ["./extensions"]
/// [extensions.policy]
/// net = ["*.acme.com", "localhost"]
/// commands = "argvOnly"
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ExtensionsConfig {
  /// Shorthand: just the paths; no operator policy (back-compat).
  Paths(Vec<String>),
  /// Detailed: paths plus the operator policy ceiling.
  Detailed(ExtensionsDetailed),
}

impl Default for ExtensionsConfig {
  fn default() -> Self {
    Self::Paths(Vec::new())
  }
}

impl ExtensionsConfig {
  /// The configured extension paths/specs, whichever shape was used.
  #[must_use]
  pub fn paths(&self) -> &[String] {
    match self {
      Self::Paths(p) => p,
      Self::Detailed(d) => &d.paths,
    }
  }

  /// The operator policy ceiling. The shorthand shape has none, which
  /// resolves to the default (fully open) policy.
  #[must_use]
  pub fn policy(&self) -> ExtensionPolicyConfig {
    match self {
      Self::Paths(_) => ExtensionPolicyConfig::default(),
      Self::Detailed(d) => d.policy.clone(),
    }
  }

  /// Per-extension settings (empty for the shorthand shape).
  #[must_use]
  pub fn settings(&self) -> BTreeMap<String, serde_json::Value> {
    match self {
      Self::Paths(_) => BTreeMap::new(),
      Self::Detailed(d) => d.settings.clone(),
    }
  }
}

/// The detailed `[extensions]` table.
#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ExtensionsDetailed {
  /// Extension paths/specs — same values the shorthand array carries.
  pub paths: Vec<String>,
  /// Per-extension settings, keyed by the extension's namespace (the
  /// part before the first `.` in its tool names, e.g. `box` for
  /// `acme.login`) or by a full tool name for tool-specific values.
  /// Delivered to a handler as `settings`.
  ///
  /// Extensions previously had no configuration channel at all, so an
  /// author had to smuggle deployment values through tool arguments or
  /// an allow-listed environment variable.
  pub settings: BTreeMap<String, serde_json::Value>,
  /// Operator policy ceiling applied to every loaded extension.
  pub policy: ExtensionPolicyConfig,
}

/// Operator ceiling over extension capability manifests. An extension
/// author declares what a tool NEEDS (`allow` in `defineTool`); this is
/// what the operator GRANTS. The effective authority a tool runs with
/// is the intersection of the two — a manifest can never widen past the
/// ceiling.
#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ExtensionPolicyConfig {
  /// Host ceiling for extension HTTP (`allow.net`). Absent ⇒ manifests
  /// keep today's semantics (no declaration = unrestricted). Present ⇒
  /// every tool's HTTP flips to default-deny: a tool with no `allow.net`
  /// gets exactly this list; a tool with one keeps only the entries this
  /// list subsumes. An explicit empty list denies all extension HTTP.
  pub net: Option<Vec<String>>,
  /// Ceiling on `allow.commands` declarations.
  pub commands: ExtensionCommandsCeiling,
}

/// What kinds of `allow.commands` declarations the operator permits.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionCommandsCeiling {
  /// Both shell-string and argv-array command specs (default).
  #[default]
  Any,
  /// Only argv-array specs. A shell-string spec (`sh -c` line, where
  /// `$(…)`, pipes and redirection live) fails the tool's registration.
  ArgvOnly,
  /// No command declarations at all; a tool declaring any fails
  /// registration.
  None,
}

/// Options for the rolldown bundling pipeline.
///
/// ```toml
/// [bundler.alias]
/// "@wdio/utils" = "./shims/wdio-utils.ts"
///
/// [bundler.virtualModules]
/// "acme:env" = "export const env = 'staging';"
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
pub use extension_manifest::{ExtensionManifest, ExtensionRequires};

impl FerridriverConfig {
  /// Load the unified configuration by resolving the whole layer
  /// stack against the real process environment.
  ///
  /// `explicit` is the `-c/--config` path. It is applied ON TOP of the
  /// discovered layers rather than replacing them, so a project can
  /// pin a couple of settings without losing the operator's
  /// user-level extensions and browser instances.
  ///
  /// Warnings (unknown keys, an unreadable `extends`) are logged here;
  /// callers that want to render them should use
  /// [`layer::resolve`] directly.
  ///
  /// # Errors
  ///
  /// Returns an error if a config file cannot be read or parsed, or if
  /// the merged document violates the schema.
  pub fn load(explicit: Option<&Path>) -> anyhow::Result<Self> {
    Self::load_layered(explicit, true)
  }

  /// Like [`Self::load`], but the caller states whether the discovered
  /// layers participate. `inherit = false` (CLI `--no-inherit`) keeps
  /// only `explicit`, or the cwd's own file when there is none.
  ///
  /// # Errors
  ///
  /// Same as [`Self::load`].
  pub fn load_layered(explicit: Option<&Path>, inherit: bool) -> anyhow::Result<Self> {
    let mut opts = layer::LoadOptions::from_process(explicit);
    // A false argument must not re-enable inheritance that the
    // environment already switched off.
    opts.inherit = opts.inherit && inherit;
    let resolved = layer::resolve(&opts)?;
    for w in &resolved.warnings {
      tracing::warn!(source = %w.source, "{}", w.message);
    }
    for l in &resolved.layers {
      tracing::debug!(kind = l.kind.label(), path = %l.path.display(), "config layer applied");
    }
    Ok(resolved.config)
  }

  /// Load ONE config file, with no inheritance.
  ///
  /// Paths inside the file are still anchored to the file's own
  /// directory and unknown keys are still reported, but no machine,
  /// user, project or environment layer participates. For the normal
  /// operator-facing path use [`Self::load`].
  ///
  /// # Errors
  ///
  /// Returns an error if the file cannot be read or parsed, or if its
  /// contents violate the schema.
  pub fn load_from(path: &Path) -> anyhow::Result<Self> {
    let resolved = layer::resolve(&layer::LoadOptions {
      explicit: Some(path.to_path_buf()),
      cwd: path.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf),
      user_config_dir: None,
      machine_config_dir: None,
      env: BTreeMap::new(),
      inherit: false,
    })?;
    for w in &resolved.warnings {
      tracing::warn!(source = %w.source, "{}", w.message);
    }
    tracing::debug!("loaded ferridriver config from {}", path.display());
    Ok(resolved.config)
  }

  /// Scripting sandbox root, with the documented default applied.
  #[must_use]
  pub fn script_root(&self) -> PathBuf {
    self
      .script_root
      .as_deref()
      .map_or_else(|| PathBuf::from(".ferridriver/scripts"), PathBuf::from)
  }

  /// Artifacts root, with the documented default applied.
  #[must_use]
  pub fn artifacts_root(&self) -> PathBuf {
    self
      .artifacts_root
      .as_deref()
      .map_or_else(|| PathBuf::from(".ferridriver/artifacts"), PathBuf::from)
  }

  /// Every configured extension entry paired with the directory its
  /// declaring layer lives in, for resolvers that need a base for
  /// `node_modules` lookups. Entries declared before layering
  /// (constructed in memory) fall back to `source_dir`, then the cwd.
  #[must_use]
  pub fn extension_specs(&self) -> Vec<ExtensionSpec> {
    let fallback = self
      .source_dir
      .clone()
      .or_else(|| std::env::current_dir().ok())
      .unwrap_or_else(|| PathBuf::from("."));
    self
      .extensions
      .paths()
      .iter()
      .map(|spec| ExtensionSpec {
        base_dir: self
          .extension_bases
          .get(spec)
          .cloned()
          .unwrap_or_else(|| fallback.clone()),
        spec: spec.clone(),
      })
      .collect()
  }

  /// Validate cross-field invariants the serde layer can't express.
  ///
  /// # Errors
  ///
  /// Returns an error if two sidecars share a `name`, a sidecar has an
  /// empty `command`, or a `[test]` browser/backend spelling is not
  /// recognised (including inside a project).
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

    self
      .test
      .browser
      .validate()
      .map_err(|e| anyhow::anyhow!("[test.browser]: {e}"))?;
    for project in &self.test.projects {
      if let Some(browser) = &project.browser {
        browser
          .validate()
          .map_err(|e| anyhow::anyhow!("[test] project '{}': {e}", project.name))?;
      }
    }
    Ok(())
  }
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
"acme:env" = "export const env = 'staging';"
"#,
    )
    .unwrap();

    let root = FerridriverConfig::load_from(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    // Anchored to the config file's own directory at load, so a
    // consumer never has to guess which cwd the value meant.
    assert_eq!(
      root.bundler.alias.get("@wdio/utils").map(String::as_str),
      Some(dir.join("shims/wdio-utils.ts").to_string_lossy().as_ref())
    );
    assert_eq!(
      root.bundler.virtual_modules.get("acme:env").map(String::as_str),
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
    root.mcp.browser.backend = Some(mcp::BackendChoice::CdpRaw);
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
    assert_eq!(parsed.mcp.browser.backend, Some(mcp::BackendChoice::CdpRaw));
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
  fn extensions_shorthand_array_parses() {
    let dir = std::env::temp_dir().join("ferridriver-config-ext-shorthand");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ferridriver.toml");
    std::fs::write(&path, "extensions = [\"./ext\", \"./tools/login.ts\"]\n").unwrap();

    let root = FerridriverConfig::load_from(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
      root.extensions.paths(),
      [
        dir.join("ext").to_string_lossy().into_owned(),
        dir.join("tools/login.ts").to_string_lossy().into_owned()
      ]
    );
    assert_eq!(root.extensions.policy(), ExtensionPolicyConfig::default());
    assert_eq!(root.extensions.policy().net, None);
    assert_eq!(root.extensions.policy().commands, ExtensionCommandsCeiling::Any);
  }

  #[test]
  fn extensions_detailed_table_with_policy_parses() {
    let dir = std::env::temp_dir().join("ferridriver-config-ext-detailed");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ferridriver.toml");
    std::fs::write(
      &path,
      r#"
[extensions]
paths = ["./ext"]

[extensions.policy]
net = ["*.acme.com", "localhost"]
commands = "argvOnly"
"#,
    )
    .unwrap();

    let root = FerridriverConfig::load_from(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
      root.extensions.paths(),
      [dir.join("ext").to_string_lossy().into_owned()]
    );
    let policy = root.extensions.policy();
    assert_eq!(
      policy.net.as_deref(),
      Some(["*.acme.com".to_string(), "localhost".to_string()].as_slice())
    );
    assert_eq!(policy.commands, ExtensionCommandsCeiling::ArgvOnly);
  }

  #[test]
  fn extensions_policy_empty_net_and_none_commands_parse() {
    let dir = std::env::temp_dir().join("ferridriver-config-ext-deny");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ferridriver.toml");
    std::fs::write(
      &path,
      r#"
[extensions]
[extensions.policy]
net = []
commands = "none"
"#,
    )
    .unwrap();

    let root = FerridriverConfig::load_from(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    let policy = root.extensions.policy();
    assert_eq!(policy.net.as_deref(), Some([].as_slice()), "empty list must stay Some");
    assert_eq!(policy.commands, ExtensionCommandsCeiling::None);
  }

  #[test]
  fn extensions_config_roundtrips_both_shapes() {
    let shorthand = ExtensionsConfig::Paths(vec!["./a".into()]);
    let json = serde_json::to_value(&shorthand).unwrap();
    assert_eq!(json, serde_json::json!(["./a"]));
    assert_eq!(serde_json::from_value::<ExtensionsConfig>(json).unwrap(), shorthand);

    let detailed = ExtensionsConfig::Detailed(ExtensionsDetailed {
      paths: vec!["./a".into()],
      settings: BTreeMap::new(),
      policy: ExtensionPolicyConfig {
        net: Some(vec!["*.acme.com".into()]),
        commands: ExtensionCommandsCeiling::ArgvOnly,
      },
    });
    let json = serde_json::to_value(&detailed).unwrap();
    assert_eq!(json["policy"]["commands"], serde_json::json!("argvOnly"));
    assert_eq!(serde_json::from_value::<ExtensionsConfig>(json).unwrap(), detailed);
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

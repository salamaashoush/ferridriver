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
//! test_match = ["**/*.spec.ts"]
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

pub mod mcp;
pub mod test;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Top-level configuration document.
#[derive(Debug, Default, Deserialize, Serialize, TS)]
#[serde(default)]
#[ts(export, export_to = "./", rename_all = "camelCase")]
pub struct FerridriverConfig {
  /// MCP server configuration.
  pub mcp: mcp::McpConfig,
  /// Test runner configuration.
  pub test: test::TestConfig,
}

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
    let cfg: FerridriverConfig = match ext {
      "toml" => toml::from_str(&content).map_err(|e| anyhow::anyhow!("invalid TOML config {}: {e}", path.display()))?,
      "yaml" | "yml" => {
        serde_yaml::from_str(&content).map_err(|e| anyhow::anyhow!("invalid YAML config {}: {e}", path.display()))?
      },
      "json" => {
        serde_json::from_str(&content).map_err(|e| anyhow::anyhow!("invalid JSON config {}: {e}", path.display()))?
      },
      other => anyhow::bail!("unsupported config format: {other} (expected toml/yaml/yml/json)"),
    };

    tracing::debug!("loaded ferridriver config from {}", path.display());
    Ok(cfg)
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
test_match = ["tests/**/*.spec.ts"]

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

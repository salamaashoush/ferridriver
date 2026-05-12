//! MCP server configuration types.
//!
//! Loaded from the `[mcp]` table of the unified `ferridriver.toml`. Provides
//! data fields plus pure helper methods. The `McpServerConfig` trait
//! implementation that wires this into the live MCP server lives in
//! `ferridriver-mcp::config` (where the trait is defined).

use std::collections::HashMap;
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ferridriver::backend::BackendKind;
use ferridriver::state::ConnectMode;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Default TTL for cached command outputs (5 minutes).
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(300);

/// Timeout for verifying a browser port is responsive.
pub const DISCOVER_TCP_TIMEOUT: Duration = Duration::from_millis(500);

/// Default MCP server name returned by `get_info`.
pub const DEFAULT_SERVER_NAME: &str = "ferridriver";

/// Root MCP-section configuration loaded from a unified `ferridriver.{toml,yaml,json}`
/// file under the `[mcp]` table.
#[derive(Debug, Default, Deserialize, Serialize, TS)]
#[serde(default, rename_all = "camelCase")]
#[ts(export, export_to = "./")]
pub struct McpConfig {
  /// MCP server metadata.
  pub server: ServerConfig,
  /// Browser launch and instance configuration.
  pub browser: BrowserConfig,

  // -- runtime fields (not deserialized) --
  /// Cached command outputs (populated at runtime).
  #[serde(skip)]
  #[ts(skip)]
  command_cache: CommandCache,
  /// Pre-built combined instructions string.
  #[serde(skip)]
  #[ts(skip)]
  instructions_cache: std::sync::OnceLock<String>,
}

/// MCP server metadata configuration.
#[derive(Debug, Default, Deserialize, Serialize, TS)]
#[serde(default)]
#[ts(export, export_to = "./", rename = "McpServerConfig", rename_all = "camelCase")]
pub struct ServerConfig {
  /// Server name for MCP `get_info` (default: "ferridriver").
  #[ts(optional)]
  pub name: Option<String>,
  /// Full override of server instructions. If set, replaces the default instructions entirely.
  #[ts(optional)]
  pub instructions: Option<String>,
  /// Additional instructions appended to the default ferridriver instructions.
  /// Ignored if `instructions` is set.
  #[ts(optional)]
  pub extra_instructions: Option<String>,
}

/// Browser launch and per-instance configuration.
#[derive(Debug, Default, Deserialize, Serialize, TS)]
#[serde(default)]
#[ts(export, export_to = "./", rename = "McpBrowserConfig", rename_all = "camelCase")]
pub struct BrowserConfig {
  /// Browser backend: "cdp-pipe" (default), "cdp-raw", "bidi".
  #[ts(optional)]
  pub backend: Option<String>,
  /// Run browsers in headless mode.
  #[ts(optional)]
  pub headless: Option<bool>,
  /// Path to Chrome/Chromium executable.
  #[ts(optional)]
  pub executable_path: Option<String>,
  /// Default viewport dimensions for new pages.
  #[ts(optional)]
  pub viewport: Option<ViewportDef>,
  /// Base Chrome arguments applied to ALL browser instances.
  pub chrome_args: Vec<String>,
  /// External command to get per-instance Chrome args.
  /// `${INSTANCE}` is replaced with the instance name.
  /// Output: one arg per line, or JSON array of strings.
  #[ts(optional)]
  pub instance_args_command: Option<String>,
  /// External command to discover a running browser instance.
  /// `${INSTANCE}` is replaced with the instance name.
  /// Output: a `ws://` URL on the first line, or empty for "not found".
  #[ts(optional)]
  pub instance_discover_command: Option<String>,
  /// Cache TTL in seconds for command outputs (default: 300).
  #[ts(optional, type = "number")]
  pub command_cache_ttl: Option<u64>,
  /// Static per-instance overrides (keyed by instance name).
  pub instances: HashMap<String, InstanceConfig>,
  /// Default config for instances not listed in `instances`.
  #[ts(optional)]
  pub default_instance: Option<InstanceConfig>,
}

/// Per-instance configuration.
#[derive(Debug, Default, Clone, Deserialize, Serialize, TS)]
#[serde(default)]
#[ts(export, export_to = "./", rename = "McpInstanceConfig", rename_all = "camelCase")]
pub struct InstanceConfig {
  /// Additional Chrome arguments for this instance.
  pub chrome_args: Vec<String>,
  /// Explicit WebSocket URL to connect to (skip launch).
  #[ts(optional)]
  pub connect_url: Option<String>,
  /// Path to Chrome profile directory for `DevToolsActivePort` discovery.
  /// `${INSTANCE}` is replaced with the instance name. Supports `~` expansion.
  #[ts(optional)]
  pub discover_profile: Option<String>,
}

/// Viewport dimensions.
#[derive(Debug, Clone, Deserialize, Serialize, TS)]
#[ts(export, export_to = "./", rename = "McpViewportDef", rename_all = "camelCase")]
pub struct ViewportDef {
  #[ts(optional)]
  pub width: Option<i64>,
  #[ts(optional)]
  pub height: Option<i64>,
}

impl McpConfig {
  /// Resolve the `BackendKind` from config (defaults to `CdpPipe`).
  #[must_use]
  pub fn backend_kind(&self) -> BackendKind {
    match self.browser.backend.as_deref() {
      Some("cdp-raw") => BackendKind::CdpRaw,
      Some("bidi") => BackendKind::Bidi,
      #[cfg(target_os = "macos")]
      Some("webkit") => BackendKind::WebKit,
      _ => BackendKind::CdpPipe,
    }
  }

  /// Whether headless mode is enabled (defaults to false).
  #[must_use]
  pub fn headless(&self) -> bool {
    self.browser.headless.unwrap_or(false)
  }

  /// Cache TTL for command outputs.
  fn cache_ttl(&self) -> Duration {
    self
      .browser
      .command_cache_ttl
      .map_or(DEFAULT_CACHE_TTL, Duration::from_secs)
  }

  /// Base Chrome args applied to every browser instance.
  #[must_use]
  pub fn chrome_args(&self) -> Vec<String> {
    self.browser.chrome_args.clone()
  }

  /// Resolve Chrome args for a named instance: static per-instance args
  /// followed by dynamic args returned by `instance_args_command`.
  #[must_use]
  pub fn chrome_args_for_instance(&self, instance: &str) -> Vec<String> {
    let mut args = Vec::new();

    if let Some(ic) = self.browser.instances.get(instance) {
      args.extend(ic.chrome_args.iter().cloned());
    } else if let Some(ref default) = self.browser.default_instance {
      args.extend(default.chrome_args.iter().cloned());
    }

    if let Some(ref cmd_template) = self.browser.instance_args_command {
      let cmd = cmd_template.replace("${INSTANCE}", instance);
      match self.command_cache.get_or_exec(&cmd, self.cache_ttl()) {
        Ok(lines) => args.extend(lines),
        Err(e) => tracing::warn!("instance_args_command failed for '{instance}': {e}"),
      }
    }

    args
  }

  /// Resolve a `ConnectMode` for the given instance: static `connect_url`,
  /// then profile discovery, then `instance_discover_command`.
  #[must_use]
  pub fn resolve_instance(&self, instance: &str) -> Option<ConnectMode> {
    if let Some(ic) = self.browser.instances.get(instance) {
      if let Some(ref url) = ic.connect_url {
        return Some(ConnectMode::ConnectUrl(url.clone()));
      }
      if let Some(ref profile_template) = ic.discover_profile {
        match discover_from_profile(profile_template, instance) {
          ProfileDiscovery::Found(mode) => return Some(mode),
          ProfileDiscovery::Stale => return None,
          ProfileDiscovery::NotFound => {},
        }
      }
    }

    if let Some(ref default) = self.browser.default_instance {
      if let Some(ref profile_template) = default.discover_profile {
        match discover_from_profile(profile_template, instance) {
          ProfileDiscovery::Found(mode) => return Some(mode),
          ProfileDiscovery::Stale => return None,
          ProfileDiscovery::NotFound => {},
        }
      }
    }

    if let Some(ref cmd_template) = self.browser.instance_discover_command {
      let cmd = cmd_template.replace("${INSTANCE}", instance);
      match self.command_cache.get_or_exec(&cmd, self.cache_ttl()) {
        Ok(lines) => {
          if let Some(url) = lines.first() {
            let url = url.trim();
            if url.starts_with("ws://") || url.starts_with("wss://") {
              return Some(ConnectMode::ConnectUrl(url.to_string()));
            }
          }
        },
        Err(e) => tracing::warn!("instance_discover_command failed for '{instance}': {e}"),
      }
    }

    None
  }

  /// MCP server display name from config or the default.
  #[must_use]
  pub fn server_name(&self) -> &str {
    self.server.name.as_deref().unwrap_or(DEFAULT_SERVER_NAME)
  }

  /// Resolve final server instructions, blending defaults with config-provided
  /// overrides or extras.
  pub fn server_instructions<'a>(&'a self, defaults: &str) -> &'a str {
    self.instructions_cache.get_or_init(|| {
      if let Some(ref full) = self.server.instructions {
        return full.clone();
      }
      match &self.server.extra_instructions {
        Some(extra) => format!("{defaults}\n\n{extra}"),
        None => defaults.to_string(),
      }
    })
  }
}

/// Result of attempting to discover a browser via a Chrome profile directory.
enum ProfileDiscovery {
  Found(ConnectMode),
  Stale,
  NotFound,
}

/// Read `DevToolsActivePort` from a Chrome profile directory and return a
/// `ConnectMode` if the browser is responding.
fn discover_from_profile(profile_template: &str, instance: &str) -> ProfileDiscovery {
  let template = profile_template.replace("${INSTANCE}", instance);
  let expanded = shellexpand::tilde(&template);
  let profile_dir = Path::new(expanded.as_ref());

  let port_file = profile_dir.join("DevToolsActivePort");
  let Ok(content) = std::fs::read_to_string(&port_file) else {
    return ProfileDiscovery::NotFound;
  };
  let mut lines = content.lines();
  let Some(port) = lines.next().and_then(|l| l.parse::<u16>().ok()) else {
    return ProfileDiscovery::NotFound;
  };
  let path = lines.next().unwrap_or("/");

  let addr = format!("127.0.0.1:{port}");
  if let Ok(sock_addr) = addr.parse() {
    if TcpStream::connect_timeout(&sock_addr, DISCOVER_TCP_TIMEOUT).is_ok() {
      return ProfileDiscovery::Found(ConnectMode::ConnectUrl(format!("ws://127.0.0.1:{port}{path}")));
    }
  }

  ProfileDiscovery::Stale
}

/// TTL-based cache for external command outputs.
///
/// Same command string (after `${INSTANCE}` substitution) returns cached output
/// within the TTL window, avoiding repeated subprocess spawns.
#[derive(Debug, Default)]
struct CommandCache {
  entries: Mutex<HashMap<String, CacheEntry>>,
}

#[derive(Debug, Clone)]
struct CacheEntry {
  lines: Vec<String>,
  created: Instant,
}

impl CommandCache {
  fn get_or_exec(&self, command: &str, ttl: Duration) -> Result<Vec<String>, String> {
    {
      let cache = self.entries.lock().map_err(|e| format!("Cache lock poisoned: {e}"))?;
      if let Some(entry) = cache.get(command) {
        if entry.created.elapsed() < ttl {
          return Ok(entry.lines.clone());
        }
      }
    }

    let lines = exec_command(command)?;

    {
      let mut cache = self.entries.lock().map_err(|e| format!("Cache lock poisoned: {e}"))?;
      cache.insert(
        command.to_string(),
        CacheEntry {
          lines: lines.clone(),
          created: Instant::now(),
        },
      );
    }

    Ok(lines)
  }
}

/// Execute a shell command and return its stdout lines.
///
/// Supports two output formats:
/// - JSON array of strings: `["--flag1", "--flag2"]`
/// - Plain text: one arg per line
fn exec_command(command: &str) -> Result<Vec<String>, String> {
  let output = Command::new("sh")
    .args(["-c", command])
    .output()
    .map_err(|e| format!("Failed to execute command: {e}"))?;

  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    return Err(format!("Command failed (exit {}): {stderr}", output.status));
  }

  let stdout = String::from_utf8_lossy(&output.stdout);
  let trimmed = stdout.trim();

  if trimmed.is_empty() {
    return Ok(Vec::new());
  }

  if trimmed.starts_with('[') {
    if let Ok(arr) = serde_json::from_str::<Vec<String>>(trimmed) {
      return Ok(arr);
    }
  }

  Ok(
    trimmed
      .lines()
      .map(|l| l.trim().to_string())
      .filter(|l| !l.is_empty())
      .collect(),
  )
}

#[cfg(test)]
mod tests {
  use super::*;

  const TEST_DEFAULTS: &str = "Browser automation via the Model Context Protocol.";

  #[test]
  fn default_config_has_sane_defaults() {
    let config = McpConfig::default();
    assert_eq!(config.server_name(), "ferridriver");
    assert_eq!(config.server_instructions(TEST_DEFAULTS), TEST_DEFAULTS);
    assert!(config.chrome_args().is_empty());
    assert!(config.chrome_args_for_instance("dev").is_empty());
    assert!(config.resolve_instance("dev").is_none());
    assert_eq!(config.backend_kind(), BackendKind::CdpPipe);
    assert!(!config.headless());
  }

  #[test]
  fn instructions_override() {
    let mut config = McpConfig::default();
    config.server.instructions = Some("Custom only".into());
    config.server.extra_instructions = Some("Should be ignored".into());
    assert_eq!(config.server_instructions(TEST_DEFAULTS), "Custom only");
  }

  #[test]
  fn extra_instructions_appended() {
    let mut config = McpConfig::default();
    config.server.extra_instructions = Some("Extra context here.".into());
    let instructions = config.server_instructions(TEST_DEFAULTS);
    assert!(instructions.starts_with(TEST_DEFAULTS));
    assert!(instructions.ends_with("Extra context here."));
  }

  #[test]
  fn static_instance_args() {
    let mut config = McpConfig::default();
    config.browser.instances.insert(
      "staging".into(),
      InstanceConfig {
        chrome_args: vec!["--proxy-server=localhost:8080".into()],
        ..Default::default()
      },
    );
    assert_eq!(
      config.chrome_args_for_instance("staging"),
      vec!["--proxy-server=localhost:8080"]
    );
    assert!(config.chrome_args_for_instance("unknown").is_empty());
  }

  #[test]
  fn default_instance_fallback() {
    let mut config = McpConfig::default();
    config.browser.default_instance = Some(InstanceConfig {
      chrome_args: vec!["--default-flag".into()],
      ..Default::default()
    });
    assert_eq!(config.chrome_args_for_instance("any"), vec!["--default-flag"]);
  }

  #[test]
  fn static_connect_url() {
    let mut config = McpConfig::default();
    config.browser.instances.insert(
      "remote".into(),
      InstanceConfig {
        connect_url: Some("ws://192.168.1.50:9222/devtools/browser/abc".into()),
        ..Default::default()
      },
    );
    let mode = config.resolve_instance("remote");
    assert!(matches!(mode, Some(ConnectMode::ConnectUrl(url)) if url.contains("192.168.1.50")));
  }

  #[test]
  fn backend_parsing() {
    let mut config = McpConfig::default();
    assert_eq!(config.backend_kind(), BackendKind::CdpPipe);
    config.browser.backend = Some("cdp-raw".into());
    assert_eq!(config.backend_kind(), BackendKind::CdpRaw);
    config.browser.backend = Some("bidi".into());
    assert_eq!(config.backend_kind(), BackendKind::Bidi);
    config.browser.backend = Some("unknown".into());
    assert_eq!(config.backend_kind(), BackendKind::CdpPipe);
  }

  #[test]
  fn command_cache_returns_cached_value() {
    let cache = CommandCache::default();
    let result1 = cache.get_or_exec("echo hello", Duration::from_secs(60));
    assert_eq!(
      result1.as_ref().map(Vec::as_slice),
      Ok(["hello".to_string()].as_slice())
    );
    let result2 = cache.get_or_exec("echo hello", Duration::from_secs(60));
    assert_eq!(result1, result2);
  }

  #[test]
  fn command_json_output_parsing() {
    let result = exec_command(r#"echo '["--flag1", "--flag2"]'"#);
    assert_eq!(result, Ok(vec!["--flag1".to_string(), "--flag2".to_string()]));
  }

  #[test]
  fn command_line_output_parsing() {
    let result = exec_command("echo flag1 && echo flag2");
    assert_eq!(result, Ok(vec!["flag1".to_string(), "flag2".to_string()]));
  }

  #[test]
  fn command_empty_output() {
    let result = exec_command("echo ''");
    assert_eq!(result, Ok(Vec::new()));
  }

  #[test]
  fn instance_args_command_substitutes_instance_name() {
    let mut config = McpConfig::default();
    config.browser.instance_args_command = Some("echo '--user-agent=Test-${INSTANCE}'".into());
    let args = config.chrome_args_for_instance("staging");
    assert_eq!(args, vec!["--user-agent=Test-staging"]);
    let args2 = config.chrome_args_for_instance("production");
    assert_eq!(args2, vec!["--user-agent=Test-production"]);
  }

  #[test]
  fn instance_args_command_json_output() {
    let mut config = McpConfig::default();
    config.browser.instance_args_command = Some(r#"printf '["--dns-prefetch-disable","--tag=dev"]'"#.into());
    let args = config.chrome_args_for_instance("dev");
    assert_eq!(args, vec!["--dns-prefetch-disable", "--tag=dev"]);
  }

  #[test]
  fn instance_args_command_merges_with_static_args() {
    let mut config = McpConfig::default();
    config.browser.instances.insert(
      "staging".into(),
      InstanceConfig {
        chrome_args: vec!["--proxy-server=localhost:8080".into()],
        ..Default::default()
      },
    );
    config.browser.instance_args_command = Some("echo '--user-agent=Bot-${INSTANCE}'".into());

    let args = config.chrome_args_for_instance("staging");
    assert_eq!(args.len(), 2);
    assert_eq!(args[0], "--proxy-server=localhost:8080");
    assert_eq!(args[1], "--user-agent=Bot-staging");
  }

  #[test]
  fn instance_args_command_default_instance_plus_command() {
    let mut config = McpConfig::default();
    config.browser.default_instance = Some(InstanceConfig {
      chrome_args: vec!["--default-flag".into()],
      ..Default::default()
    });
    config.browser.instance_args_command = Some("echo '--dynamic-flag'".into());
    let args = config.chrome_args_for_instance("unknown-env");
    assert_eq!(args, vec!["--default-flag", "--dynamic-flag"]);
  }

  #[test]
  fn instance_args_command_failure_is_non_fatal() {
    let mut config = McpConfig::default();
    config.browser.instance_args_command = Some("false".into());
    config.browser.instances.insert(
      "dev".into(),
      InstanceConfig {
        chrome_args: vec!["--static-flag".into()],
        ..Default::default()
      },
    );
    let args = config.chrome_args_for_instance("dev");
    assert_eq!(args, vec!["--static-flag"]);
  }

  #[test]
  fn discover_command_returns_ws_url() {
    let mut config = McpConfig::default();
    config.browser.instance_discover_command = Some("echo 'ws://127.0.0.1:9222/devtools/browser/abc'".into());
    let mode = config.resolve_instance("any");
    assert!(matches!(
      mode,
      Some(ConnectMode::ConnectUrl(url)) if url == "ws://127.0.0.1:9222/devtools/browser/abc"
    ));
  }

  #[test]
  fn discover_command_substitutes_instance() {
    let mut config = McpConfig::default();
    config.browser.instance_discover_command = Some("echo 'ws://127.0.0.1:9222/${INSTANCE}'".into());
    let mode = config.resolve_instance("staging");
    assert!(matches!(
      mode,
      Some(ConnectMode::ConnectUrl(url)) if url == "ws://127.0.0.1:9222/staging"
    ));
  }

  #[test]
  fn discover_command_ignores_non_ws_output() {
    let mut config = McpConfig::default();
    config.browser.instance_discover_command = Some("echo 'not-a-ws-url'".into());
    assert!(config.resolve_instance("dev").is_none());
  }

  #[test]
  fn discover_command_empty_output_returns_none() {
    let mut config = McpConfig::default();
    config.browser.instance_discover_command = Some("echo ''".into());
    assert!(config.resolve_instance("dev").is_none());
  }

  #[test]
  fn discover_command_failure_returns_none() {
    let mut config = McpConfig::default();
    config.browser.instance_discover_command = Some("false".into());
    assert!(config.resolve_instance("dev").is_none());
  }

  #[test]
  fn static_connect_url_takes_priority_over_discover_command() {
    let mut config = McpConfig::default();
    config.browser.instances.insert(
      "staging".into(),
      InstanceConfig {
        connect_url: Some("ws://static-host:9222/browser".into()),
        ..Default::default()
      },
    );
    config.browser.instance_discover_command = Some("echo 'ws://dynamic-host:9222/browser'".into());
    let mode = config.resolve_instance("staging");
    assert!(matches!(
      mode,
      Some(ConnectMode::ConnectUrl(url)) if url == "ws://static-host:9222/browser"
    ));
  }

  #[test]
  fn unknown_instance_falls_through_to_discover_command() {
    let mut config = McpConfig::default();
    config.browser.instances.insert(
      "staging".into(),
      InstanceConfig {
        connect_url: Some("ws://staging-host:9222/browser".into()),
        ..Default::default()
      },
    );
    config.browser.instance_discover_command = Some("echo 'ws://discovered-host:9222/${INSTANCE}'".into());

    let staging = config.resolve_instance("staging");
    assert!(matches!(
      staging,
      Some(ConnectMode::ConnectUrl(url)) if url.contains("staging-host")
    ));

    let prod = config.resolve_instance("production");
    assert!(matches!(
      prod,
      Some(ConnectMode::ConnectUrl(url)) if url == "ws://discovered-host:9222/production"
    ));
  }

  #[test]
  fn no_discovery_returns_none_for_launch_fallback() {
    let config = McpConfig::default();
    assert!(config.resolve_instance("anything").is_none());
  }

  #[test]
  fn command_cache_ttl_respects_config() {
    let mut config = McpConfig::default();
    config.browser.command_cache_ttl = Some(60);
    assert_eq!(config.cache_ttl(), Duration::from_secs(60));

    config.browser.command_cache_ttl = None;
    assert_eq!(config.cache_ttl(), DEFAULT_CACHE_TTL);
  }

  #[test]
  fn command_cache_expires_after_ttl() {
    let cache = CommandCache::default();
    let short_ttl = Duration::from_millis(50);

    let result1 = cache.get_or_exec("echo first", short_ttl);
    assert!(result1.is_ok());

    std::thread::sleep(Duration::from_millis(100));

    let result2 = cache.get_or_exec("echo first", short_ttl);
    assert_eq!(result1, result2);

    let entries = cache.entries.lock().unwrap();
    let entry = entries.get("echo first").unwrap();
    assert!(entry.created.elapsed() < Duration::from_millis(50));
  }

  #[test]
  fn command_cache_different_commands_cached_separately() {
    let cache = CommandCache::default();
    let ttl = Duration::from_secs(60);

    let r1 = cache.get_or_exec("echo aaa", ttl).unwrap();
    let r2 = cache.get_or_exec("echo bbb", ttl).unwrap();
    assert_eq!(r1, vec!["aaa"]);
    assert_eq!(r2, vec!["bbb"]);

    let entries = cache.entries.lock().unwrap();
    assert_eq!(entries.len(), 2);
  }

  #[test]
  fn config_resolve_uses_instance_not_composite_key() {
    let mut config = McpConfig::default();
    config.browser.instances.insert(
      "staging".into(),
      InstanceConfig {
        connect_url: Some("ws://staging-browser:9222".into()),
        ..Default::default()
      },
    );
    assert!(config.resolve_instance("staging").is_some());
    assert!(config.resolve_instance("staging:admin").is_none());
  }

  #[test]
  fn instance_args_uses_instance_not_composite_key() {
    let mut config = McpConfig::default();
    config.browser.instances.insert(
      "staging".into(),
      InstanceConfig {
        chrome_args: vec!["--staging-flag".into()],
        ..Default::default()
      },
    );
    assert_eq!(config.chrome_args_for_instance("staging"), vec!["--staging-flag"]);
    assert!(config.chrome_args_for_instance("staging:admin").is_empty());
  }

  #[test]
  fn discover_profile_nonexistent_path_returns_none() {
    let result = discover_from_profile("/nonexistent/path/${INSTANCE}/profile", "dev");
    assert!(matches!(result, ProfileDiscovery::NotFound));
  }

  #[test]
  fn discover_profile_stale_port_file_returns_some_none() {
    let dir = std::env::temp_dir().join("ferridriver-config-test-stale-profile");
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("DevToolsActivePort"), "59999\n/devtools/browser/fake").unwrap();

    let result = discover_from_profile(dir.to_str().unwrap(), "dev");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(matches!(result, ProfileDiscovery::Stale));
  }

  #[test]
  fn discover_profile_instance_substitution() {
    let dir = std::env::temp_dir().join("ferridriver-config-test-inst-sub");
    let staging_dir = dir.join("staging");
    let _ = std::fs::create_dir_all(&staging_dir);
    std::fs::write(staging_dir.join("DevToolsActivePort"), "59998\n/devtools/browser/abc").unwrap();

    let template = format!("{}/${{INSTANCE}}", dir.display());
    let result = discover_from_profile(&template, "staging");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(matches!(result, ProfileDiscovery::Stale));
  }
}

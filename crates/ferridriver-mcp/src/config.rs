//! Config-file-driven `McpServerConfig` implementation.
//!
//! Loads from YAML, TOML, or JSON via the `config` crate and implements
//! `McpServerConfig` so ferridriver can run fully configured without custom Rust code.
//!
//! Dynamic per-instance Chrome args and instance discovery are supported via
//! external command execution with TTL-based caching.
//!
//! # Example (YAML)
//!
//! ```yaml
//! server:
//!   name: my-server
//!   extra_instructions: "Custom instructions appended to defaults."
//!
//! browser:
//!   chrome_args:
//!     - "--ignore-certificate-errors"
//!   instance_args_command: "my-tool chrome-args --env ${INSTANCE}"
//!   instance_discover_command: "my-tool discover --env ${INSTANCE}"
//!   instances:
//!     staging:
//!       chrome_args:
//!         - "--proxy-server=http://localhost:8080"
//!       discover_profile: "~/.my-app-chrome-${INSTANCE}"
//! ```

use std::collections::HashMap;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use ferridriver::backend::BackendKind;
use ferridriver::state::ConnectMode;
use serde::Deserialize;

use crate::server::{DEFAULT_INSTRUCTIONS, DEFAULT_SERVER_NAME, McpServerConfig};

/// Default TTL for cached command outputs (5 minutes).
const DEFAULT_CACHE_TTL: Duration = Duration::from_secs(300);

/// Timeout for verifying a browser port is responsive.
const DISCOVER_TCP_TIMEOUT: Duration = Duration::from_millis(500);

// ── Config types ────────────────────────────────────────────────────────────

/// Root configuration loaded from YAML/TOML/JSON.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct FileConfig {
  /// MCP server metadata.
  pub server: ServerConfig,
  /// Browser launch and instance configuration.
  pub browser: BrowserConfig,

  // -- runtime fields (not deserialized) --
  /// Cached command outputs (populated at runtime).
  #[serde(skip)]
  command_cache: CommandCache,
  /// Pre-built combined instructions string.
  #[serde(skip)]
  instructions_cache: std::sync::OnceLock<String>,
}

/// MCP server metadata configuration.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
  /// Server name for MCP `get_info` (default: "ferridriver").
  pub name: Option<String>,
  /// Full override of server instructions. If set, replaces the default instructions entirely.
  pub instructions: Option<String>,
  /// Additional instructions appended to the default ferridriver instructions.
  /// Ignored if `instructions` is set.
  pub extra_instructions: Option<String>,
}

/// Browser launch and per-instance configuration.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct BrowserConfig {
  /// Browser backend: "cdp-pipe" (default), "cdp-raw", "bidi".
  pub backend: Option<String>,
  /// Run browsers in headless mode.
  pub headless: Option<bool>,
  /// Path to Chrome/Chromium executable.
  pub executable_path: Option<String>,
  /// Default viewport dimensions for new pages.
  pub viewport: Option<ViewportDef>,
  /// Base Chrome arguments applied to ALL browser instances.
  pub chrome_args: Vec<String>,
  /// External command to get per-instance Chrome args.
  /// `${INSTANCE}` is replaced with the instance name.
  /// Output: one arg per line, or JSON array of strings.
  pub instance_args_command: Option<String>,
  /// External command to discover a running browser instance.
  /// `${INSTANCE}` is replaced with the instance name.
  /// Output: a `ws://` URL on the first line, or empty for "not found".
  pub instance_discover_command: Option<String>,
  /// Cache TTL in seconds for command outputs (default: 300).
  pub command_cache_ttl: Option<u64>,
  /// Static per-instance overrides (keyed by instance name).
  pub instances: HashMap<String, InstanceConfig>,
  /// Default config for instances not listed in `instances`.
  pub default_instance: Option<InstanceConfig>,
}

/// Per-instance configuration.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default)]
pub struct InstanceConfig {
  /// Additional Chrome arguments for this instance.
  pub chrome_args: Vec<String>,
  /// Explicit WebSocket URL to connect to (skip launch).
  pub connect_url: Option<String>,
  /// Path to Chrome profile directory for `DevToolsActivePort` discovery.
  /// `${INSTANCE}` is replaced with the instance name. Supports `~` expansion.
  pub discover_profile: Option<String>,
}

/// Viewport dimensions.
#[derive(Debug, Clone, Deserialize)]
pub struct ViewportDef {
  pub width: Option<i64>,
  pub height: Option<i64>,
}

// ── Loading ─────────────────────────────────────────────────────────────────

impl FileConfig {
  /// Load config from an explicit file path. Format is auto-detected by extension.
  ///
  /// # Errors
  ///
  /// Returns an error if the file cannot be read or parsed.
  pub fn load(path: &Path) -> anyhow::Result<Self> {
    let settings = config::Config::builder()
      .add_source(config::File::from(path.to_owned()))
      .build()
      .map_err(|e| anyhow::anyhow!("Failed to load config from {}: {e}", path.display()))?;
    settings
      .try_deserialize()
      .map_err(|e| anyhow::anyhow!("Failed to parse config: {e}"))
  }

  /// Search for a config file in standard locations and load it if found.
  ///
  /// Search order:
  /// 1. `./ferridriver.config.{yaml,yml,toml,json}`
  /// 2. `~/.config/ferridriver/config.{yaml,yml,toml,json}`
  #[must_use]
  pub fn load_with_search() -> Option<Self> {
    let candidates = search_paths();
    for path in candidates {
      if path.exists() {
        match Self::load(&path) {
          Ok(config) => return Some(config),
          Err(e) => {
            tracing::warn!("Failed to load config from {}: {e}", path.display());
          },
        }
      }
    }
    None
  }

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
}

/// Generate candidate config file paths.
fn search_paths() -> Vec<PathBuf> {
  let mut paths = Vec::new();
  let extensions = ["yaml", "yml", "toml", "json"];

  // Current directory
  for ext in &extensions {
    paths.push(PathBuf::from(format!("ferridriver.config.{ext}")));
  }

  // User config directory
  if let Some(config_dir) = dirs::config_dir() {
    let fd_dir = config_dir.join("ferridriver");
    for ext in &extensions {
      paths.push(fd_dir.join(format!("config.{ext}")));
    }
  }

  paths
}

// ── McpServerConfig implementation ──────────────────────────────────────────

impl McpServerConfig for FileConfig {
  fn chrome_args(&self) -> Vec<String> {
    self.browser.chrome_args.clone()
  }

  fn chrome_args_for_instance(&self, instance: &str) -> Vec<String> {
    let mut args = Vec::new();

    // 1. Static per-instance args
    if let Some(ic) = self.browser.instances.get(instance) {
      args.extend(ic.chrome_args.iter().cloned());
    } else if let Some(ref default) = self.browser.default_instance {
      args.extend(default.chrome_args.iter().cloned());
    }

    // 2. Dynamic args from external command (cached)
    if let Some(ref cmd_template) = self.browser.instance_args_command {
      let cmd = cmd_template.replace("${INSTANCE}", instance);
      match self.command_cache.get_or_exec(&cmd, self.cache_ttl()) {
        Ok(lines) => args.extend(lines),
        Err(e) => tracing::warn!("instance_args_command failed for '{instance}': {e}"),
      }
    }

    args
  }

  fn resolve_instance(&self, instance: &str) -> Option<ConnectMode> {
    // 1. Static connect_url
    if let Some(ic) = self.browser.instances.get(instance) {
      if let Some(ref url) = ic.connect_url {
        return Some(ConnectMode::ConnectUrl(url.clone()));
      }
      // 2. Profile-based discovery (built-in DevToolsActivePort reader)
      if let Some(ref profile_template) = ic.discover_profile {
        match discover_from_profile(profile_template, instance) {
          ProfileDiscovery::Found(mode) => return Some(mode),
          ProfileDiscovery::Stale => return None,
          ProfileDiscovery::NotFound => {},
        }
      }
    }

    // 3. Default instance profile discovery
    if let Some(ref default) = self.browser.default_instance {
      if let Some(ref profile_template) = default.discover_profile {
        match discover_from_profile(profile_template, instance) {
          ProfileDiscovery::Found(mode) => return Some(mode),
          ProfileDiscovery::Stale => return None,
          ProfileDiscovery::NotFound => {},
        }
      }
    }

    // 4. Dynamic discovery via external command (cached)
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

  fn server_name(&self) -> &str {
    self.server.name.as_deref().unwrap_or(DEFAULT_SERVER_NAME)
  }

  fn server_instructions(&self) -> &str {
    self.instructions_cache.get_or_init(|| {
      if let Some(ref full) = self.server.instructions {
        return full.clone();
      }
      match &self.server.extra_instructions {
        Some(extra) => format!("{DEFAULT_INSTRUCTIONS}\n\n{extra}"),
        None => DEFAULT_INSTRUCTIONS.to_string(),
      }
    })
  }
}

// ── Profile discovery ───────────────────────────────────────────────────────

/// Result of attempting to discover a browser via a Chrome profile directory.
enum ProfileDiscovery {
  /// Browser found and responding at the given connect mode.
  Found(ConnectMode),
  /// Profile directory exists but browser is not responding (stale port file).
  Stale,
  /// Profile directory or port file does not exist.
  NotFound,
}

/// Read `DevToolsActivePort` from a Chrome profile directory and return
/// a `ConnectMode` if the browser is responding.
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

  // Verify browser is actually responding
  let addr = format!("127.0.0.1:{port}");
  if let Ok(sock_addr) = addr.parse() {
    if TcpStream::connect_timeout(&sock_addr, DISCOVER_TCP_TIMEOUT).is_ok() {
      return ProfileDiscovery::Found(ConnectMode::ConnectUrl(format!("ws://127.0.0.1:{port}{path}")));
    }
  }

  // Profile exists but browser not responding -- stale port file
  ProfileDiscovery::Stale
}

// ── Command cache ───────────────────────────────────────────────────────────

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
  /// Get cached output or execute the command and cache the result.
  fn get_or_exec(&self, command: &str, ttl: Duration) -> Result<Vec<String>, String> {
    // Fast path: check cache
    {
      let cache = self.entries.lock().map_err(|e| format!("Cache lock poisoned: {e}"))?;
      if let Some(entry) = cache.get(command) {
        if entry.created.elapsed() < ttl {
          return Ok(entry.lines.clone());
        }
      }
    }

    // Slow path: execute command
    let lines = exec_command(command)?;

    // Store in cache
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

  // Try JSON array first
  if trimmed.starts_with('[') {
    if let Ok(arr) = serde_json::from_str::<Vec<String>>(trimmed) {
      return Ok(arr);
    }
  }

  // Fall back to line-per-arg
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
  use std::sync::Arc;

  #[test]
  fn default_config_has_sane_defaults() {
    let config = FileConfig::default();
    assert_eq!(config.server_name(), "ferridriver");
    assert_eq!(config.server_instructions(), DEFAULT_INSTRUCTIONS);
    assert!(config.chrome_args().is_empty());
    assert!(config.chrome_args_for_instance("dev").is_empty());
    assert!(config.resolve_instance("dev").is_none());
    assert_eq!(config.backend_kind(), BackendKind::CdpPipe);
    assert!(!config.headless());
  }

  #[test]
  fn instructions_override() {
    let mut config = FileConfig::default();
    config.server.instructions = Some("Custom only".into());
    config.server.extra_instructions = Some("Should be ignored".into());
    assert_eq!(config.server_instructions(), "Custom only");
  }

  #[test]
  fn extra_instructions_appended() {
    let mut config = FileConfig::default();
    config.server.extra_instructions = Some("Extra context here.".into());
    let instructions = config.server_instructions();
    assert!(instructions.starts_with("Browser automation via"));
    assert!(instructions.ends_with("Extra context here."));
  }

  #[test]
  fn static_instance_args() {
    let mut config = FileConfig::default();
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
    let mut config = FileConfig::default();
    config.browser.default_instance = Some(InstanceConfig {
      chrome_args: vec!["--default-flag".into()],
      ..Default::default()
    });
    // Named instance not defined, falls back to default
    assert_eq!(config.chrome_args_for_instance("any"), vec!["--default-flag"]);
  }

  #[test]
  fn static_connect_url() {
    let mut config = FileConfig::default();
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
    let mut config = FileConfig::default();
    assert_eq!(config.backend_kind(), BackendKind::CdpPipe);

    config.browser.backend = Some("cdp-raw".into());
    assert_eq!(config.backend_kind(), BackendKind::CdpRaw);

    config.browser.backend = Some("bidi".into());
    assert_eq!(config.backend_kind(), BackendKind::Bidi);

    config.browser.backend = Some("unknown".into());
    assert_eq!(config.backend_kind(), BackendKind::CdpPipe); // fallback
  }

  #[test]
  fn command_cache_returns_cached_value() {
    let cache = CommandCache::default();
    // Execute a fast command
    let result1 = cache.get_or_exec("echo hello", Duration::from_secs(60));
    assert!(result1.is_ok());
    assert_eq!(
      result1.as_ref().map(Vec::as_slice),
      Ok(["hello".to_string()].as_slice())
    );

    // Second call should return cached (same result)
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
    let result = exec_command("echo 'flag1\nflag2'");
    assert!(result.is_ok());
    let lines = result.unwrap();
    assert!(!lines.is_empty()); // At least the echo output
    // Test multi-line with explicit newlines via bash $'...' syntax
    let result2 = exec_command("echo flag1 && echo flag2");
    assert_eq!(result2, Ok(vec!["flag1".to_string(), "flag2".to_string()]));
  }

  #[test]
  fn command_empty_output() {
    let result = exec_command("echo ''");
    assert_eq!(result, Ok(Vec::new()));
  }

  #[test]
  fn search_paths_include_local_and_user() {
    let paths = search_paths();
    assert!(
      paths
        .iter()
        .any(|p| p.to_string_lossy().contains("ferridriver.config.yaml"))
    );
    assert!(
      paths
        .iter()
        .any(|p| p.to_string_lossy().contains("ferridriver.config.toml"))
    );
    assert!(
      paths
        .iter()
        .any(|p| p.to_string_lossy().contains("ferridriver.config.json"))
    );
  }

  // ── Instance command substitution ─────────────────────────────────────────

  #[test]
  fn instance_args_command_substitutes_instance_name() {
    let mut config = FileConfig::default();
    config.browser.instance_args_command = Some("echo '--user-agent=Test-${INSTANCE}'".into());
    let args = config.chrome_args_for_instance("staging");
    assert_eq!(args, vec!["--user-agent=Test-staging"]);

    let args2 = config.chrome_args_for_instance("production");
    assert_eq!(args2, vec!["--user-agent=Test-production"]);
  }

  #[test]
  fn instance_args_command_json_output() {
    let mut config = FileConfig::default();
    config.browser.instance_args_command =
      Some(r#"echo '["--dns-prefetch-disable","--instance=${INSTANCE}"]'"#.replace("${INSTANCE}", "dev"));
    // Use a direct command that produces JSON
    config.browser.instance_args_command = Some(r#"printf '["--dns-prefetch-disable","--tag=dev"]'"#.into());
    let args = config.chrome_args_for_instance("dev");
    assert_eq!(args, vec!["--dns-prefetch-disable", "--tag=dev"]);
  }

  #[test]
  fn instance_args_command_merges_with_static_args() {
    let mut config = FileConfig::default();
    config.browser.instances.insert(
      "staging".into(),
      InstanceConfig {
        chrome_args: vec!["--proxy-server=localhost:8080".into()],
        ..Default::default()
      },
    );
    config.browser.instance_args_command = Some("echo '--user-agent=Bot-${INSTANCE}'".into());

    let args = config.chrome_args_for_instance("staging");
    // Static args come first, then dynamic
    assert_eq!(args.len(), 2);
    assert_eq!(args[0], "--proxy-server=localhost:8080");
    assert_eq!(args[1], "--user-agent=Bot-staging");
  }

  #[test]
  fn instance_args_command_default_instance_plus_command() {
    let mut config = FileConfig::default();
    config.browser.default_instance = Some(InstanceConfig {
      chrome_args: vec!["--default-flag".into()],
      ..Default::default()
    });
    config.browser.instance_args_command = Some("echo '--dynamic-flag'".into());

    // Unknown instance uses default + command
    let args = config.chrome_args_for_instance("unknown-env");
    assert_eq!(args, vec!["--default-flag", "--dynamic-flag"]);
  }

  #[test]
  fn instance_args_command_failure_is_non_fatal() {
    let mut config = FileConfig::default();
    config.browser.instance_args_command = Some("false".into()); // exits non-zero
    config.browser.instances.insert(
      "dev".into(),
      InstanceConfig {
        chrome_args: vec!["--static-flag".into()],
        ..Default::default()
      },
    );
    // Static args still returned even if command fails
    let args = config.chrome_args_for_instance("dev");
    assert_eq!(args, vec!["--static-flag"]);
  }

  // ── Instance discovery command ────────────────────────────────────────────

  #[test]
  fn discover_command_returns_ws_url() {
    let mut config = FileConfig::default();
    config.browser.instance_discover_command = Some("echo 'ws://127.0.0.1:9222/devtools/browser/abc'".into());

    let mode = config.resolve_instance("any");
    assert!(matches!(
      mode,
      Some(ConnectMode::ConnectUrl(url)) if url == "ws://127.0.0.1:9222/devtools/browser/abc"
    ));
  }

  #[test]
  fn discover_command_substitutes_instance() {
    let mut config = FileConfig::default();
    // The command uses ${INSTANCE} but since exec_command gets the already-substituted string,
    // we verify the substitution happens in resolve_instance
    config.browser.instance_discover_command = Some("echo 'ws://127.0.0.1:9222/${INSTANCE}'".into());

    let mode = config.resolve_instance("staging");
    assert!(matches!(
      mode,
      Some(ConnectMode::ConnectUrl(url)) if url == "ws://127.0.0.1:9222/staging"
    ));
  }

  #[test]
  fn discover_command_ignores_non_ws_output() {
    let mut config = FileConfig::default();
    config.browser.instance_discover_command = Some("echo 'not-a-ws-url'".into());

    let mode = config.resolve_instance("dev");
    assert!(mode.is_none());
  }

  #[test]
  fn discover_command_empty_output_returns_none() {
    let mut config = FileConfig::default();
    config.browser.instance_discover_command = Some("echo ''".into());

    let mode = config.resolve_instance("dev");
    assert!(mode.is_none());
  }

  #[test]
  fn discover_command_failure_returns_none() {
    let mut config = FileConfig::default();
    config.browser.instance_discover_command = Some("false".into());

    let mode = config.resolve_instance("dev");
    assert!(mode.is_none());
  }

  #[test]
  fn static_connect_url_takes_priority_over_discover_command() {
    let mut config = FileConfig::default();
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
    let mut config = FileConfig::default();
    config.browser.instances.insert(
      "staging".into(),
      InstanceConfig {
        connect_url: Some("ws://staging-host:9222/browser".into()),
        ..Default::default()
      },
    );
    config.browser.instance_discover_command = Some("echo 'ws://discovered-host:9222/${INSTANCE}'".into());

    // staging: uses static connect_url
    let staging = config.resolve_instance("staging");
    assert!(matches!(
      staging,
      Some(ConnectMode::ConnectUrl(url)) if url.contains("staging-host")
    ));

    // production: not in instances map, falls through to discover command
    let prod = config.resolve_instance("production");
    assert!(matches!(
      prod,
      Some(ConnectMode::ConnectUrl(url)) if url == "ws://discovered-host:9222/production"
    ));
  }

  #[test]
  fn no_discovery_returns_none_for_launch_fallback() {
    let config = FileConfig::default();
    // No instances, no commands -> None -> BrowserState falls back to Launch
    assert!(config.resolve_instance("anything").is_none());
  }

  // ── Cache behavior ────────────────────────────────────────────────────────

  #[test]
  fn command_cache_ttl_respects_config() {
    let mut config = FileConfig::default();
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

    // Wait for TTL to expire
    std::thread::sleep(Duration::from_millis(100));

    // Should re-execute (but same command returns same output)
    let result2 = cache.get_or_exec("echo first", short_ttl);
    assert_eq!(result1, result2);

    // Verify the entry was indeed refreshed by checking internal state
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

  // ── YAML config loading ───────────────────────────────────────────────────

  #[test]
  fn load_yaml_config() {
    let dir = std::env::temp_dir().join("ferridriver-test-yaml");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("config.yaml");
    std::fs::write(
      &path,
      r#"
server:
  name: "yaml-test"
browser:
  headless: true
  backend: "cdp-raw"
  chrome_args:
    - "--disable-gpu"
  instance_args_command: "echo '--from-yaml-${INSTANCE}'"
  instances:
    staging:
      chrome_args:
        - "--staging-flag"
      connect_url: "ws://staging:9222/browser"
    dev:
      discover_profile: "~/.chrome-profiles/${INSTANCE}"
  default_instance:
    chrome_args:
      - "--fallback-flag"
"#,
    )
    .unwrap();

    let config = FileConfig::load(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    // Server config
    assert_eq!(config.server_name(), "yaml-test");
    assert_eq!(config.backend_kind(), BackendKind::CdpRaw);
    assert!(config.headless());

    // Base args
    assert_eq!(config.chrome_args(), vec!["--disable-gpu"]);

    // Staging instance: static args + connect_url + dynamic command
    let staging_args = config.chrome_args_for_instance("staging");
    assert!(staging_args.contains(&"--staging-flag".to_string()));
    assert!(staging_args.contains(&"--from-yaml-staging".to_string()));

    let staging_mode = config.resolve_instance("staging");
    assert!(matches!(
      staging_mode,
      Some(ConnectMode::ConnectUrl(url)) if url == "ws://staging:9222/browser"
    ));

    // Unknown instance: default_instance fallback + dynamic command
    let unknown_args = config.chrome_args_for_instance("unknown");
    assert!(unknown_args.contains(&"--fallback-flag".to_string()));
    assert!(unknown_args.contains(&"--from-yaml-unknown".to_string()));
  }

  #[test]
  fn load_toml_config() {
    let dir = std::env::temp_dir().join("ferridriver-test-toml");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("config.toml");
    std::fs::write(
      &path,
      r#"
[server]
name = "toml-test"

[browser]
headless = true
chrome_args = ["--disable-extensions"]
instance_args_command = "echo '--toml-instance=${INSTANCE}'"

[browser.instances.production]
chrome_args = ["--no-sandbox"]
connect_url = "ws://prod-browser:9222/devtools"
"#,
    )
    .unwrap();

    let config = FileConfig::load(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(config.server_name(), "toml-test");
    assert_eq!(config.chrome_args(), vec!["--disable-extensions"]);

    // Production: static + dynamic
    let prod_args = config.chrome_args_for_instance("production");
    assert!(prod_args.contains(&"--no-sandbox".to_string()));
    assert!(prod_args.contains(&"--toml-instance=production".to_string()));

    let prod_mode = config.resolve_instance("production");
    assert!(matches!(
      prod_mode,
      Some(ConnectMode::ConnectUrl(url)) if url.contains("prod-browser")
    ));

    // Non-existent instance: only dynamic
    let dev_args = config.chrome_args_for_instance("dev");
    assert_eq!(dev_args, vec!["--toml-instance=dev"]);
    assert!(config.resolve_instance("dev").is_none());
  }

  #[test]
  fn load_json_config() {
    let dir = std::env::temp_dir().join("ferridriver-test-json");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("config.json");
    std::fs::write(
      &path,
      r#"{
  "server": { "name": "json-test" },
  "browser": {
    "chrome_args": ["--json-flag"],
    "instances": {
      "ci": {
        "chrome_args": ["--headless", "--no-sandbox"],
        "connect_url": "ws://ci-host:9222/browser"
      }
    }
  }
}"#,
    )
    .unwrap();

    let config = FileConfig::load(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(config.server_name(), "json-test");
    assert_eq!(config.chrome_args(), vec!["--json-flag"]);
    assert!(matches!(
      config.resolve_instance("ci"),
      Some(ConnectMode::ConnectUrl(url)) if url.contains("ci-host")
    ));
  }

  // ── McpServerConfig trait wiring ──────────────────────────────────────────

  #[test]
  fn config_trait_wires_to_browser_state() {
    use ferridriver::state::BrowserState;

    let mut config = FileConfig::default();
    config.browser.chrome_args = vec!["--base-flag".into()];
    config.browser.instance_args_command = Some("echo '--dynamic-${INSTANCE}'".into());
    config.browser.instances.insert(
      "test".into(),
      InstanceConfig {
        chrome_args: vec!["--test-flag".into()],
        connect_url: Some("ws://test-host:9222".into()),
        ..Default::default()
      },
    );

    let config: Arc<dyn McpServerConfig> = Arc::new(config);

    // Simulate what McpServer::with_options does
    let mut state = BrowserState::new(ConnectMode::Launch, BackendKind::CdpPipe);
    state.extra_args = config.chrome_args();

    let config_clone = Arc::clone(&config);
    state.set_instance_args_fn(Box::new(move |instance| {
      config_clone.chrome_args_for_instance(instance)
    }));
    let config_clone = Arc::clone(&config);
    state.set_instance_resolver_fn(Box::new(move |instance| config_clone.resolve_instance(instance)));

    // Verify base args
    assert_eq!(state.extra_args, vec!["--base-flag"]);

    // The callbacks are opaque (Box<dyn Fn>), but we can verify they produce
    // correct results by calling them directly
    assert_eq!(
      config.chrome_args_for_instance("test"),
      vec!["--test-flag", "--dynamic-test"]
    );
    assert!(matches!(
      config.resolve_instance("test"),
      Some(ConnectMode::ConnectUrl(url)) if url == "ws://test-host:9222"
    ));

    // Unknown instance: no static, just dynamic
    assert_eq!(config.chrome_args_for_instance("other"), vec!["--dynamic-other"]);
    assert!(config.resolve_instance("other").is_none()); // falls back to Launch
  }

  // ── Profile discovery ─────────────────────────────────────────────────────

  // ── Connect tool session key handling ───────────────────────────────────

  #[test]
  fn session_key_parsing_for_connect() {
    // Verify SessionKey::parse extracts instance correctly for connect tool
    let key = ferridriver::state::SessionKey::parse("staging:admin");
    assert_eq!(&*key.instance, "staging");
    assert_eq!(&*key.context, "admin");

    let key = ferridriver::state::SessionKey::parse("default");
    assert_eq!(&*key.instance, "default");
    assert_eq!(&*key.context, "default");

    let key = ferridriver::state::SessionKey::parse("myctx");
    assert_eq!(&*key.instance, "default");
    assert_eq!(&*key.context, "myctx");
  }

  #[test]
  fn config_resolve_uses_instance_not_composite_key() {
    let mut config = FileConfig::default();
    config.browser.instances.insert(
      "staging".into(),
      InstanceConfig {
        connect_url: Some("ws://staging-browser:9222".into()),
        ..Default::default()
      },
    );

    // resolve_instance should find "staging" (instance name)
    assert!(config.resolve_instance("staging").is_some());
    // It should NOT find the composite key "staging:admin"
    assert!(config.resolve_instance("staging:admin").is_none());
  }

  #[test]
  fn instance_args_uses_instance_not_composite_key() {
    let mut config = FileConfig::default();
    config.browser.instances.insert(
      "staging".into(),
      InstanceConfig {
        chrome_args: vec!["--staging-flag".into()],
        ..Default::default()
      },
    );

    // chrome_args_for_instance should find "staging"
    assert_eq!(config.chrome_args_for_instance("staging"), vec!["--staging-flag"]);
    // Composite key should NOT match the static instance
    assert!(config.chrome_args_for_instance("staging:admin").is_empty());
  }

  // ── Profile discovery ─────────────────────────────────────────────────────

  #[test]
  fn discover_profile_nonexistent_path_returns_none() {
    let result = discover_from_profile("/nonexistent/path/${INSTANCE}/profile", "dev");
    assert!(matches!(result, ProfileDiscovery::NotFound));
  }

  #[test]
  fn discover_profile_stale_port_file_returns_some_none() {
    // Create a fake DevToolsActivePort file with a port that isn't listening
    let dir = std::env::temp_dir().join("ferridriver-test-stale-profile");
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("DevToolsActivePort"), "59999\n/devtools/browser/fake").unwrap();

    let result = discover_from_profile(dir.to_str().unwrap(), "dev");
    let _ = std::fs::remove_dir_all(&dir);

    // Profile exists but port 59999 isn't listening -> Stale
    assert!(matches!(result, ProfileDiscovery::Stale));
  }

  #[test]
  fn discover_profile_instance_substitution() {
    // Verify ${INSTANCE} substitution in profile path
    let dir = std::env::temp_dir().join("ferridriver-test-inst-sub");
    let staging_dir = dir.join("staging");
    let _ = std::fs::create_dir_all(&staging_dir);
    std::fs::write(staging_dir.join("DevToolsActivePort"), "59998\n/devtools/browser/abc").unwrap();

    let template = format!("{}/$${{INSTANCE}}", dir.display()).replace("$$", "$");
    let result = discover_from_profile(&template, "staging");
    let _ = std::fs::remove_dir_all(&dir);

    // Port 59998 isn't listening -> Stale, but proves path substitution worked
    assert!(matches!(result, ProfileDiscovery::Stale));
  }
}

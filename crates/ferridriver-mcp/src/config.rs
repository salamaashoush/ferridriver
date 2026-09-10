//! `McpServerConfig` impl for the unified config's `McpConfig` section.
//!
//! Type definitions live in `ferridriver-config`. This module wires those
//! types into the runtime `McpServerConfig` trait so the MCP server can be
//! driven directly from a `ferridriver.toml` file with no custom Rust code.

pub use ferridriver_config::browser::InstanceConfig;
pub use ferridriver_config::mcp::{
  BrowserConfig, DEFAULT_CACHE_TTL, DEFAULT_SERVER_NAME, DISCOVER_TCP_TIMEOUT, McpConfig, ServerConfig,
};

/// Backwards-compatible alias. Prefer `McpConfig`.
pub type FileConfig = McpConfig;

use crate::server::{DEFAULT_INSTRUCTIONS, McpServerConfig};
use ferridriver::state::ConnectMode;

impl McpServerConfig for McpConfig {
  fn chrome_args(&self) -> Vec<String> {
    McpConfig::chrome_args(self)
  }

  fn settle_timeout_ms(&self) -> u64 {
    self
      .server
      .settle_timeout_ms
      .unwrap_or(crate::server::DEFAULT_SETTLE_TIMEOUT_MS)
  }

  fn instance_overrides(&self, instance: &str) -> Result<ferridriver::options::InstanceOverrides, String> {
    McpConfig::instance_overrides(self, instance)
  }

  fn instance_names(&self) -> Vec<String> {
    McpConfig::instance_names(self)
  }

  fn base_overrides(&self) -> ferridriver::options::InstanceOverrides {
    // A credentialed section proxy is reported by `instance_health` on
    // the first launch; the base plan just goes without it.
    McpConfig::base_overrides(self, "default").unwrap_or_default()
  }

  fn default_viewport(&self) -> Option<ferridriver::options::ViewportConfig> {
    let viewport = McpConfig::viewport(self)?;
    Some(ferridriver::options::ViewportConfig {
      width: viewport.width,
      height: viewport.height,
      ..Default::default()
    })
  }

  fn resolve_instance(&self, instance: &str) -> Option<ConnectMode> {
    McpConfig::resolve_instance(self, instance)
  }

  fn instance_health(&self, instance: &str) -> Result<(), String> {
    McpConfig::instance_health(self, instance)
  }

  fn server_name(&self) -> &str {
    McpConfig::server_name(self)
  }

  fn server_instructions(&self) -> &str {
    McpConfig::server_instructions(self, DEFAULT_INSTRUCTIONS)
  }
}

/// The whole config document as the server's configuration.
///
/// `[mcp]` alone cannot answer for `scriptRoot`, `artifactsRoot` or
/// `[engine]`: those are top-level because the test, BDD and `run`
/// hosts read them too. Implementing the trait for the document is what
/// makes those keys take effect at all — they were previously
/// Rust-only trait defaults, so an operator could not move the sandbox
/// or change a session-VM limit from a config file.
impl McpServerConfig for ferridriver_config::FerridriverConfig {
  fn script_root(&self) -> std::path::PathBuf {
    ferridriver_config::FerridriverConfig::script_root(self)
  }

  fn settle_timeout_ms(&self) -> u64 {
    self
      .mcp
      .server
      .settle_timeout_ms
      .unwrap_or(crate::server::DEFAULT_SETTLE_TIMEOUT_MS)
  }

  fn artifacts_root(&self) -> std::path::PathBuf {
    ferridriver_config::FerridriverConfig::artifacts_root(self)
  }

  fn artifacts_max_bytes(&self) -> Option<u64> {
    self.artifacts_max_bytes
  }

  /// A secrets source that cannot be read is reported and treated as empty:
  /// the server has already started, and refusing every tool call is a worse
  /// answer than a logged failure the operator can act on. The CLI resolves
  /// the same config before starting and fails there instead.
  fn secrets(&self) -> ferridriver::response::Secrets {
    match self.secrets.resolve() {
      Ok(pairs) => ferridriver::response::Secrets::new(pairs),
      Err(e) => {
        tracing::error!(error = %e, "secrets unavailable; responses will NOT be redacted");
        ferridriver::response::Secrets::default()
      },
    }
  }

  fn script_engine_config(&self) -> ferridriver_script::ScriptEngineConfig {
    let mut cfg = ferridriver_script::ScriptEngineConfig {
      secrets: McpServerConfig::secrets(self),
      artifacts_budget: self.artifacts_max_bytes.map(ferridriver::response::OutputBudget::new),
      ..Default::default()
    };
    let engine = &self.engine;
    if let Some(ms) = engine.timeout_ms {
      cfg.default_timeout = std::time::Duration::from_millis(ms);
    }
    if let Some(bytes) = engine.max_memory_bytes {
      cfg.default_memory_limit = bytes;
    }
    if let Some(bytes) = engine.max_console_bytes {
      cfg.max_console_bytes = bytes;
    }
    if let Some(bytes) = engine.max_console_entry_bytes {
      cfg.max_console_entry_bytes = bytes;
    }
    if let Some(vms) = engine.max_session_vms {
      cfg.max_session_vms = vms;
    }
    if let Some(secs) = engine.session_idle_ttl_secs {
      // `0` means "never reap": an operator keeping long-lived sessions
      // needs their extension state and `vars` to survive idle gaps.
      cfg.session_idle_ttl = (secs > 0).then(|| std::time::Duration::from_secs(secs));
    }
    cfg
  }

  fn chrome_args(&self) -> Vec<String> {
    self.mcp.chrome_args()
  }

  fn instance_overrides(&self, instance: &str) -> Result<ferridriver::options::InstanceOverrides, String> {
    self.mcp.instance_overrides(instance)
  }

  fn instance_names(&self) -> Vec<String> {
    self.mcp.instance_names()
  }

  fn base_overrides(&self) -> ferridriver::options::InstanceOverrides {
    McpServerConfig::base_overrides(&self.mcp)
  }

  fn default_viewport(&self) -> Option<ferridriver::options::ViewportConfig> {
    McpServerConfig::default_viewport(&self.mcp)
  }

  fn resolve_instance(&self, instance: &str) -> Option<ConnectMode> {
    self.mcp.resolve_instance(instance)
  }

  fn instance_health(&self, instance: &str) -> Result<(), String> {
    self.mcp.instance_health(instance)
  }

  fn server_name(&self) -> &str {
    self.mcp.server_name()
  }

  fn server_instructions(&self) -> &str {
    self.mcp.server_instructions(DEFAULT_INSTRUCTIONS)
  }
}

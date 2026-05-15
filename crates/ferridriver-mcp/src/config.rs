//! `McpServerConfig` impl for the unified config's `McpConfig` section.
//!
//! Type definitions live in `ferridriver-config`. This module wires those
//! types into the runtime `McpServerConfig` trait so the MCP server can be
//! driven directly from a `ferridriver.toml` file with no custom Rust code.

pub use ferridriver_config::mcp::{
  BrowserConfig, DEFAULT_CACHE_TTL, DEFAULT_SERVER_NAME, DISCOVER_TCP_TIMEOUT, InstanceConfig, McpConfig, ServerConfig,
  ViewportDef,
};

/// Backwards-compatible alias. Prefer `McpConfig`.
pub type FileConfig = McpConfig;

use crate::server::{DEFAULT_INSTRUCTIONS, McpServerConfig};
use ferridriver::state::ConnectMode;

impl McpServerConfig for McpConfig {
  fn chrome_args(&self) -> Vec<String> {
    McpConfig::chrome_args(self)
  }

  fn chrome_args_for_instance(&self, instance: &str) -> Vec<String> {
    McpConfig::chrome_args_for_instance(self, instance)
  }

  fn resolve_instance(&self, instance: &str) -> Option<ConnectMode> {
    McpConfig::resolve_instance(self, instance)
  }

  fn server_name(&self) -> &str {
    McpConfig::server_name(self)
  }

  fn server_instructions(&self) -> &str {
    McpConfig::server_instructions(self, DEFAULT_INSTRUCTIONS)
  }

  fn plugin_paths(&self) -> Vec<std::path::PathBuf> {
    self.plugins.iter().map(std::path::PathBuf::from).collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::server::DEFAULT_INSTRUCTIONS;
  use ferridriver::backend::BackendKind;
  use std::sync::Arc;

  #[test]
  fn trait_delegates_to_inherent_methods() {
    let mut config = McpConfig::default();
    config.browser.chrome_args = vec!["--base-flag".into()];
    config.browser.instances.insert(
      "staging".into(),
      InstanceConfig {
        chrome_args: vec!["--staging-flag".into()],
        connect_url: Some("ws://staging-host:9222".into()),
        ..Default::default()
      },
    );

    let trait_obj: Arc<dyn McpServerConfig> = Arc::new(config);
    assert_eq!(trait_obj.chrome_args(), vec!["--base-flag"]);
    assert_eq!(trait_obj.chrome_args_for_instance("staging"), vec!["--staging-flag"]);
    assert!(matches!(
      trait_obj.resolve_instance("staging"),
      Some(ConnectMode::ConnectUrl(url)) if url.contains("staging-host")
    ));
    assert_eq!(trait_obj.server_name(), DEFAULT_SERVER_NAME);
    assert_eq!(trait_obj.server_instructions(), DEFAULT_INSTRUCTIONS);
  }

  #[test]
  fn backend_parsing_via_helper() {
    let mut config = McpConfig::default();
    assert_eq!(config.backend_kind(), BackendKind::CdpPipe);
    config.browser.backend = Some("cdp-raw".into());
    assert_eq!(config.backend_kind(), BackendKind::CdpRaw);
  }
}

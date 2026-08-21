//! MCP server configuration types.
//!
//! Loaded from the `[mcp]` table of the unified `ferridriver.toml`.
//! Provides data fields plus pure helper methods. The `McpServerConfig`
//! trait implementation that wires this into the live MCP server lives
//! in `ferridriver-mcp::config` (where the trait is defined).
//!
//! Instance routing (per-instance launch settings and the external
//! args/discover commands) lives in [`crate::browser`] and is shared
//! with `[test.browser]`, so a suite and an MCP session can target the
//! same environment through the same declarations.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use ferridriver::backend::BackendKind;
use ferridriver::options::InstanceOverrides;
use ferridriver::state::ConnectMode;
use serde::{Deserialize, Serialize};

use crate::browser::{
  CommandCache, IgnoreDefaultArgsConfig, InstanceConfig, ProxyConfig, RoutingView, instance_overrides_from,
};
use crate::command_spec::CommandSpec;

pub use crate::browser::{DEFAULT_CACHE_TTL, DISCOVER_TCP_TIMEOUT, ws_endpoint_is_live};

/// Default MCP server name returned by `get_info`.
pub const DEFAULT_SERVER_NAME: &str = "ferridriver";

/// Root MCP-section configuration loaded from a unified
/// `ferridriver.{toml,yaml,json}` file under the `[mcp]` table.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct McpConfig {
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

/// Which browser backend drives an instance.
///
/// A typed enum, not a string: a misspelled backend used to fall
/// through to `cdp-pipe` silently, so a config asking for Firefox
/// quietly drove Chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendChoice {
  CdpPipe,
  CdpRaw,
  Bidi,
  #[serde(rename = "webkit")]
  WebKit,
}

impl BackendChoice {
  /// The wire spelling, for diagnostics and for handing back to
  /// consumers that still take a string.
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::CdpPipe => "cdp-pipe",
      Self::CdpRaw => "cdp-raw",
      Self::Bidi => "bidi",
      Self::WebKit => "webkit",
    }
  }

  /// Every accepted spelling, for error messages.
  pub const ALL: &'static [&'static str] = &["cdp-pipe", "cdp-raw", "bidi", "webkit"];

  /// Parse a wire spelling.
  ///
  /// # Errors
  ///
  /// Returns an error naming the bad value and listing the valid ones.
  /// Callers must not fall back to a default: picking `cdp-pipe` for a
  /// typo is how a run silently drives the wrong engine.
  pub fn parse(value: &str) -> anyhow::Result<Self> {
    match value {
      "cdp-pipe" => Ok(Self::CdpPipe),
      "cdp-raw" => Ok(Self::CdpRaw),
      "bidi" => Ok(Self::Bidi),
      "webkit" => Ok(Self::WebKit),
      other => anyhow::bail!("unknown backend {other:?} (expected one of {})", Self::ALL.join(", ")),
    }
  }

  /// The engine-level backend this choice selects.
  #[must_use]
  pub fn kind(self) -> BackendKind {
    match self {
      Self::CdpPipe => BackendKind::CdpPipe,
      Self::CdpRaw => BackendKind::CdpRaw,
      Self::Bidi => BackendKind::Bidi,
      Self::WebKit => BackendKind::WebKit,
    }
  }
}

/// MCP server metadata configuration.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ServerConfig {
  /// Server name for MCP `get_info` (default: "ferridriver").
  pub name: Option<String>,
  /// Full override of server instructions. If set, replaces the default instructions entirely.
  pub instructions: Option<String>,
  /// Additional instructions appended to the default ferridriver instructions.
  /// Ignored if `instructions` is set.
  #[serde(alias = "extra_instructions")]
  pub extra_instructions: Option<String>,
}

/// Browser launch and per-instance configuration.
///
/// Keys are camelCase on the wire, matching every other section; the
/// `snake_case` spellings this section used to require are accepted as
/// aliases so existing files keep working.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserConfig {
  /// Browser backend (default `cdp-pipe`).
  pub backend: Option<BackendChoice>,
  /// Run browsers in headless mode.
  pub headless: Option<bool>,
  /// Path to the browser executable.
  #[serde(alias = "executable_path")]
  pub executable_path: Option<String>,
  /// Viewport emulated on pages this server opens.
  ///
  /// Falls back to the top-level `[browser].viewport`, then to
  /// Playwright's 1280x720. `null` opts out of viewport emulation
  /// entirely.
  #[serde(default, deserialize_with = "crate::browser::written_viewport")]
  pub viewport: Option<crate::browser::ViewportOverride>,
  /// Base browser arguments applied to ALL instances.
  #[serde(alias = "chrome_args")]
  pub chrome_args: Vec<String>,
  /// Profile directory every instance launches with unless it names its
  /// own. Without one, each launch gets a throwaway profile, so logins
  /// do not survive a server restart and an external browser manager
  /// can never find the process again.
  #[serde(alias = "user_data_dir")]
  pub user_data_dir: Option<String>,
  /// Environment variables for every browser process.
  pub env: BTreeMap<String, String>,
  /// Proxy applied to every instance.
  pub proxy: Option<ProxyConfig>,
  /// Built-in switches to drop for every instance.
  #[serde(alias = "ignore_default_args")]
  pub ignore_default_args: Option<IgnoreDefaultArgsConfig>,
  /// Command producing per-instance browser args. A bare string is a
  /// shell line (with `${INSTANCE}` safely quoted); an argv array or a
  /// full spec object avoids the shell entirely and can set `timeoutMs`
  /// / `output`.
  #[serde(alias = "instance_args_command")]
  pub instance_args_command: Option<CommandSpec>,
  /// Command discovering a running browser for an instance. Output: a
  /// `ws(s)://` URL (plain text, a JSON array, or an object with
  /// `wsEndpoint` / `webSocketDebuggerUrl` / `url`).
  #[serde(alias = "instance_discover_command")]
  pub instance_discover_command: Option<CommandSpec>,
  /// Cache TTL in seconds for command outputs (default: 300).
  #[serde(alias = "command_cache_ttl")]
  pub command_cache_ttl: Option<u64>,
  /// Static per-instance overrides (keyed by instance name).
  pub instances: HashMap<String, InstanceConfig>,
  /// Defaults for instances not listed in `instances`.
  #[serde(alias = "default_instance")]
  pub default_instance: Option<InstanceConfig>,
  /// The top-level `[browser]` registry, copied in when the document is
  /// resolved so this section can fall back to it. Not a user-writable key
  /// here: it is declared once at the top level.
  #[serde(skip)]
  pub global_browser: Option<crate::browser::BrowserSectionConfig>,
}

impl McpConfig {
  /// The viewport pages opened by this server are emulated with.
  ///
  /// `[mcp.browser].viewport` first, then the top-level
  /// `[browser].viewport`, then Playwright's default. `None` is
  /// returned only for an explicit `viewport: null`, which is the one
  /// way to ask for no emulation at all.
  #[must_use]
  pub fn viewport(&self) -> Option<crate::browser::ViewportConfig> {
    self
      .browser
      .viewport
      .as_ref()
      .or_else(|| self.browser.global_browser.as_ref().and_then(|g| g.viewport.as_ref()))
      .map_or_else(
        || Some(crate::browser::ViewportConfig::default()),
        crate::browser::ViewportOverride::size,
      )
  }

  /// Resolve the `BackendKind` from config (defaults to `CdpPipe`).
  ///
  /// No platform gate: the `WebKit` backend drives Playwright's
  /// cross-platform build over `pw_run.sh`, so it is selectable on
  /// Linux as well as macOS. Gating it to macOS turned `backend =
  /// "webkit"` into a silent `cdp-pipe` run everywhere else.
  #[must_use]
  pub fn backend_kind(&self) -> BackendKind {
    self
      .browser
      .backend
      .or_else(|| self.browser.global_browser.as_ref().and_then(|g| g.backend))
      .map_or(BackendKind::CdpPipe, BackendChoice::kind)
  }

  /// Whether headless mode is enabled (defaults to false).
  #[must_use]
  pub fn headless(&self) -> bool {
    self
      .browser
      .headless
      .or_else(|| self.browser.global_browser.as_ref().and_then(|g| g.headless))
      .unwrap_or(false)
  }

  /// Cache TTL for command outputs.
  fn cache_ttl(&self) -> Duration {
    self
      .browser
      .command_cache_ttl
      .map_or(DEFAULT_CACHE_TTL, Duration::from_secs)
  }

  /// Base browser args applied to every instance.
  #[must_use]
  pub fn chrome_args(&self) -> Vec<String> {
    self.browser.chrome_args.clone()
  }

  /// The section-wide launch settings, before per-instance overrides.
  ///
  /// `instance` is the name `${INSTANCE}` expands to in the section's own
  /// paths. It must be the instance actually being launched, not a fixed
  /// `"default"`: a section-level `userDataDir = "~/p/${INSTANCE}"` then
  /// resolved to `~/p/default` for every instance, so two instances
  /// launched Chrome against ONE profile directory — which Chrome
  /// refuses, and which loses whichever instance's cookies got there
  /// first.
  ///
  /// # Errors
  ///
  /// Returns an error when the section-level proxy declares credentials.
  pub fn base_overrides(&self, instance: &str) -> Result<InstanceOverrides, String> {
    instance_overrides_from(
      &InstanceConfig {
        args: self.browser.chrome_args.clone(),
        user_data_dir: self.browser.user_data_dir.clone(),
        executable_path: self.browser.executable_path.clone(),
        headless: self
          .browser
          .headless
          .or_else(|| self.browser.global_browser.as_ref().and_then(|g| g.headless)),
        backend: self
          .browser
          .backend
          .or_else(|| self.browser.global_browser.as_ref().and_then(|g| g.backend)),
        env: self.browser.env.clone(),
        proxy: self.browser.proxy.clone(),
        ignore_default_args: self.browser.ignore_default_args.clone(),
        ..Default::default()
      },
      instance,
      self.backend_kind(),
    )
  }

  /// Instance names this config defines, for session-key resolution and
  /// diagnostics.
  #[must_use]
  pub fn instance_names(&self) -> Vec<String> {
    let mut names: Vec<String> = self.browser.instances.keys().cloned().collect();
    names.sort();
    names
  }

  fn routing(&self) -> RoutingView<'_> {
    RoutingView {
      global: self.browser.global_browser.as_ref(),
      instances: &self.browser.instances,
      default_instance: self.browser.default_instance.as_ref(),
      args_command: self.browser.instance_args_command.as_ref(),
      discover_command: self.browser.instance_discover_command.as_ref(),
      cache: &self.command_cache,
      cache_ttl: self.cache_ttl(),
      backend: self.backend_kind(),
    }
  }

  /// Every launch setting for `instance`: section defaults, the
  /// instance's own overrides, then whatever the args command adds.
  ///
  /// # Errors
  ///
  /// Returns an error when the instance name is unusable, its args
  /// command hard-fails, or a proxy declares credentials. The launch is
  /// aborted rather than started against an unconfigured environment.
  pub fn instance_overrides(&self, instance: &str) -> Result<InstanceOverrides, String> {
    let mut merged = self.base_overrides(instance)?;
    let per_instance = self.routing().overrides_for(instance)?;

    merged.args.extend(per_instance.args);
    if per_instance.user_data_dir.is_some() {
      merged.user_data_dir = per_instance.user_data_dir;
    }
    if per_instance.executable_path.is_some() {
      merged.executable_path = per_instance.executable_path;
    }
    if per_instance.headless.is_some() {
      merged.headless = per_instance.headless;
    }
    if per_instance.backend.is_some() {
      merged.backend = per_instance.backend;
    }
    if per_instance.ignore_default_args.is_some() {
      merged.ignore_default_args = per_instance.ignore_default_args;
    }
    merged.env.extend(per_instance.env);
    Ok(merged)
  }

  /// Check that an instance can be started, before a browser is
  /// launched for it.
  ///
  /// # Errors
  ///
  /// Returns an actionable message when the name is not a usable
  /// instance.
  pub fn instance_health(&self, instance: &str) -> Result<(), String> {
    self.routing().health(instance)
  }

  /// Resolve a `ConnectMode` for `instance`: static `connect_url`, then
  /// profile discovery, then the discover command.
  #[must_use]
  pub fn resolve_instance(&self, instance: &str) -> Option<ConnectMode> {
    self.routing().resolve_connect(instance)
  }

  /// Drop every cached instance-command result. For an operator whose
  /// environment changed (new DNS mapping, restarted browser) and who
  /// should not have to wait out the TTL.
  pub fn flush_command_cache(&self) {
    self.command_cache.flush();
  }

  /// MCP server display name from config or the default.
  #[must_use]
  pub fn server_name(&self) -> &str {
    self.server.name.as_deref().unwrap_or(DEFAULT_SERVER_NAME)
  }

  /// Resolve final server instructions, blending defaults with
  /// config-provided overrides or extras.
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

#[cfg(test)]
mod tests {
  use super::*;

  const TEST_DEFAULTS: &str = "Browser automation via the Model Context Protocol.";

  /// See `browser::tests::port_guard`: ephemeral-port reuse across
  /// parallel tests makes a "this port is dead" assertion flaky.
  static PORT_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

  fn port_guard() -> std::sync::MutexGuard<'static, ()> {
    PORT_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  fn spec(json: &str) -> CommandSpec {
    serde_json::from_str(json).expect("spec")
  }

  #[test]
  fn default_config_has_sane_defaults() {
    let config = McpConfig::default();
    assert_eq!(config.server_name(), "ferridriver");
    assert_eq!(config.server_instructions(TEST_DEFAULTS), TEST_DEFAULTS);
    assert!(config.chrome_args().is_empty());
    assert!(config.instance_overrides("dev").expect("overrides").args.is_empty());
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
  fn backend_parsing() {
    let mut config = McpConfig::default();
    assert_eq!(config.backend_kind(), BackendKind::CdpPipe);
    config.browser.backend = Some(BackendChoice::CdpRaw);
    assert_eq!(config.backend_kind(), BackendKind::CdpRaw);
    config.browser.backend = Some(BackendChoice::Bidi);
    assert_eq!(config.backend_kind(), BackendKind::Bidi);
  }

  #[test]
  fn webkit_backend_is_selectable_on_every_platform() {
    let mut config = McpConfig::default();
    config.browser.backend = Some(BackendChoice::WebKit);
    // Playwright's WebKit build is cross-platform; the old macOS-only
    // arm turned this into a silent cdp-pipe run on Linux.
    assert_eq!(config.backend_kind(), BackendKind::WebKit);
  }

  #[test]
  fn unknown_backend_is_rejected_not_defaulted() {
    let err = BackendChoice::parse("chrom-pipe").expect_err("must reject");
    let msg = err.to_string();
    assert!(msg.contains("chrom-pipe"), "names the bad value: {msg}");
    assert!(msg.contains("cdp-pipe"), "lists valid values: {msg}");
  }

  #[test]
  fn backend_wire_spellings_round_trip() {
    for name in BackendChoice::ALL {
      let parsed = BackendChoice::parse(name).expect("parse");
      assert_eq!(parsed.as_str(), *name);
      let json = serde_json::to_string(&parsed).expect("serialize");
      assert_eq!(json, format!("\"{name}\""), "serde spelling must match parse");
    }
  }

  #[test]
  fn section_defaults_flow_into_every_instance() {
    let mut config = McpConfig::default();
    config.browser.chrome_args = vec!["--base".into()];
    config.browser.user_data_dir = Some("/profiles/shared".into());
    config.browser.executable_path = Some("/bin/chrome".into());
    config.browser.headless = Some(true);
    config.browser.env.insert("SHARED".into(), "1".into());

    let o = config.instance_overrides("anything").expect("overrides");
    assert_eq!(o.args, ["--base"]);
    assert_eq!(o.user_data_dir.as_deref(), Some("/profiles/shared"));
    assert_eq!(o.executable_path.as_deref(), Some("/bin/chrome"));
    assert_eq!(o.headless, Some(true));
    assert_eq!(o.env.get("SHARED").map(String::as_str), Some("1"));
  }

  #[test]
  fn instance_overrides_beat_section_defaults() {
    let mut config = McpConfig::default();
    config.browser.chrome_args = vec!["--base".into()];
    config.browser.headless = Some(true);
    config.browser.user_data_dir = Some("/profiles/shared".into());
    config.browser.instances.insert(
      "staging".into(),
      InstanceConfig {
        args: vec!["--staging".into()],
        headless: Some(false),
        user_data_dir: Some("/profiles/${INSTANCE}".into()),
        backend: Some(BackendChoice::CdpRaw),
        env: BTreeMap::from([("APP_ENV".to_string(), "staging".to_string())]),
        ..Default::default()
      },
    );

    let o = config.instance_overrides("staging").expect("overrides");
    assert_eq!(o.args, ["--base", "--staging"], "section args come first");
    assert_eq!(o.headless, Some(false));
    assert_eq!(o.user_data_dir.as_deref(), Some("/profiles/staging"));
    assert_eq!(o.backend, Some(BackendKind::CdpRaw));
    assert_eq!(o.env.get("APP_ENV").map(String::as_str), Some("staging"));
  }

  #[test]
  fn default_instance_applies_to_unlisted_names() {
    let mut config = McpConfig::default();
    config.browser.default_instance = Some(InstanceConfig {
      args: vec!["--default-flag".into()],
      ..Default::default()
    });
    assert_eq!(
      config.instance_overrides("whatever").expect("overrides").args,
      ["--default-flag"]
    );
  }

  #[test]
  fn args_command_output_is_appended() {
    let mut config = McpConfig::default();
    config.browser.chrome_args = vec!["--base".into()];
    config.browser.instance_args_command = Some(spec(r#""echo --user-agent=Bot-${INSTANCE}""#));
    let o = config.instance_overrides("staging").expect("overrides");
    assert_eq!(o.args, ["--base", "--user-agent=Bot-staging"]);
  }

  #[test]
  fn args_command_failure_aborts_instead_of_launching_unconfigured() {
    let mut config = McpConfig::default();
    config.browser.instance_args_command = Some(spec(r#""echo bad >&2; exit 2""#));
    // Previously a warn-only path: the browser launched with no
    // environment mapping and the caller never knew.
    assert!(config.instance_overrides("default").is_err());
    let err = config.instance_health("default").expect_err("must fail");
    assert!(err.contains("<env>:<context>"), "{err}");
  }

  #[test]
  fn instance_name_from_a_session_key_cannot_inject_a_command() {
    let mut config = McpConfig::default();
    config.browser.instance_args_command = Some(spec(r#""echo --env ${INSTANCE}""#));
    // The name arrives from a caller-supplied session key.
    let err = config
      .instance_overrides("staging; touch /tmp/ferridriver-pwned")
      .expect_err("must reject");
    assert!(err.contains("only letters"), "{err}");
    assert!(
      !std::path::Path::new("/tmp/ferridriver-pwned").exists(),
      "command must not have run"
    );
  }

  #[test]
  fn instance_names_are_reported_sorted() {
    let mut config = McpConfig::default();
    config
      .browser
      .instances
      .insert("staging".into(), InstanceConfig::default());
    config.browser.instances.insert("dev".into(), InstanceConfig::default());
    assert_eq!(config.instance_names(), ["dev", "staging"]);
  }

  #[test]
  fn static_connect_url_is_returned() {
    let mut config = McpConfig::default();
    config.browser.instances.insert(
      "remote".into(),
      InstanceConfig {
        connect_url: Some("ws://192.168.1.50:9222/devtools/browser/abc".into()),
        ..Default::default()
      },
    );
    assert!(matches!(
      config.resolve_instance("remote"),
      Some(ConnectMode::ConnectUrl(url)) if url.contains("192.168.1.50")
    ));
  }

  #[test]
  fn discover_command_returns_a_live_endpoint() {
    let _net = port_guard();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let mut config = McpConfig::default();
    config.browser.instance_discover_command =
      Some(spec(&format!(r#""echo ws://127.0.0.1:{port}/devtools/browser/abc""#)));
    assert!(matches!(
      config.resolve_instance("any"),
      Some(ConnectMode::ConnectUrl(url)) if url == format!("ws://127.0.0.1:{port}/devtools/browser/abc")
    ));
  }

  #[test]
  fn discover_command_rejects_a_dead_endpoint() {
    let _net = port_guard();
    let port = {
      let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
      l.local_addr().expect("addr").port()
    };
    let mut config = McpConfig::default();
    config.browser.instance_discover_command = Some(spec(&format!(r#""echo ws://127.0.0.1:{port}/x""#)));
    assert!(config.resolve_instance("any").is_none());
  }

  #[test]
  fn discover_command_ignores_non_ws_output() {
    let mut config = McpConfig::default();
    config.browser.instance_discover_command = Some(spec(r#""echo not-a-ws-url""#));
    assert!(config.resolve_instance("dev").is_none());
  }

  #[test]
  fn discover_command_failure_returns_none() {
    let mut config = McpConfig::default();
    config.browser.instance_discover_command = Some(spec(r#""false""#));
    assert!(config.resolve_instance("dev").is_none());
  }

  #[test]
  fn command_cache_ttl_respects_config() {
    let mut config = McpConfig::default();
    config.browser.command_cache_ttl = Some(60);
    assert_eq!(config.cache_ttl(), Duration::from_mins(1));
    config.browser.command_cache_ttl = None;
    assert_eq!(config.cache_ttl(), DEFAULT_CACHE_TTL);
  }

  #[test]
  fn resolution_uses_the_instance_not_the_composite_key() {
    let mut config = McpConfig::default();
    config.browser.instances.insert(
      "staging".into(),
      InstanceConfig {
        connect_url: Some("ws://staging-browser:9222".into()),
        args: vec!["--staging-flag".into()],
        ..Default::default()
      },
    );
    assert!(config.resolve_instance("staging").is_some());
    // A composite key is not an instance name (and `:` is not even a
    // legal character in one).
    assert!(config.resolve_instance("staging:admin").is_none());
    assert!(config.instance_overrides("staging:admin").is_err());
  }

  /// A config that says nothing about the viewport gets Playwright's,
  /// not "no viewport at all".
  ///
  /// The difference is invisible on a throwaway profile and decisive on
  /// a persistent one: with no emulation the page inherits the window
  /// Chrome restored from the profile's last run, so whatever size that
  /// browser was left at silently becomes every session's viewport.
  #[test]
  fn an_unwritten_viewport_is_playwrights_default() {
    let config = McpConfig::default();
    let viewport = config.viewport().expect("a default viewport");
    assert_eq!((viewport.width, viewport.height), (1280, 720));
  }

  #[test]
  fn the_top_level_browser_section_supplies_the_viewport() {
    let mut config = McpConfig::default();
    config.browser.global_browser = Some(crate::browser::BrowserSectionConfig {
      viewport: Some(crate::browser::ViewportOverride::Size(crate::browser::ViewportConfig {
        width: 1600,
        height: 900,
      })),
      ..Default::default()
    });
    let viewport = config.viewport().expect("the section's viewport");
    assert_eq!((viewport.width, viewport.height), (1600, 900));
  }

  #[test]
  fn the_mcp_section_wins_over_the_top_level_one() {
    let mut config = McpConfig::default();
    config.browser.global_browser = Some(crate::browser::BrowserSectionConfig {
      viewport: Some(crate::browser::ViewportOverride::Size(crate::browser::ViewportConfig {
        width: 1600,
        height: 900,
      })),
      ..Default::default()
    });
    config.browser.viewport = Some(crate::browser::ViewportOverride::Size(crate::browser::ViewportConfig {
      width: 800,
      height: 600,
    }));
    let viewport = config.viewport().expect("the mcp section's viewport");
    assert_eq!((viewport.width, viewport.height), (800, 600));
  }

  /// `viewport: null` is the one way to ask for no emulation, and it has
  /// to survive at either spelling — absent and explicitly-null are
  /// different answers.
  #[test]
  fn an_explicit_null_viewport_disables_emulation() {
    let mut config = McpConfig::default();
    config.browser.viewport = Some(crate::browser::ViewportOverride::Disabled);
    assert!(config.viewport().is_none());

    let mut config = McpConfig::default();
    config.browser.global_browser = Some(crate::browser::BrowserSectionConfig {
      viewport: Some(crate::browser::ViewportOverride::Disabled),
      ..Default::default()
    });
    assert!(config.viewport().is_none());
  }

  #[test]
  fn a_written_null_viewport_parses_as_disabled_not_absent() {
    let config: McpConfig = serde_json::from_str(r#"{"browser": {"viewport": null}}"#).expect("parse");
    assert!(
      matches!(
        config.browser.viewport,
        Some(crate::browser::ViewportOverride::Disabled)
      ),
      "null must reach the field, not collapse into None"
    );
    assert!(config.viewport().is_none());
  }
}

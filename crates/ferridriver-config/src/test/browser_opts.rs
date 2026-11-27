//! Browser launch and per-context `use` configuration types.

use super::media::{ScreenshotOption, TraceOption, VideoOption};
use super::project::ReporterConfig;
use crate::browser::{ViewportConfig, ViewportOverride, written_viewport};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserConfig {
  pub browser: String,
  #[serde(alias = "protocol")]
  pub backend: String,
  /// CDP wire transport (`pipe` or `ws`).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub transport: Option<ferridriver::options::ChromiumTransport>,
  pub channel: Option<String>,
  pub headless: bool,
  pub executable_path: Option<String>,
  pub args: Vec<String>,
  #[serde(
    default,
    deserialize_with = "written_viewport",
    skip_serializing_if = "Option::is_none"
  )]
  pub viewport: Option<ViewportOverride>,
  pub slow_mo: Option<u64>,
  /// Playwright `launchOptions.firefoxUserPrefs`: `about:config`
  /// preferences for a Firefox launch, ignored by other browsers.
  #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
  pub firefox_user_prefs: std::collections::BTreeMap<String, serde_json::Value>,
  /// Playwright `use` block: per-project context defaults.
  #[serde(default, rename = "use")]
  pub use_options: ContextConfig,

  // ── Instance routing (shared schema with `[mcp.browser]`) ──
  /// Which named instance this browser launches as. A project may
  /// override it, so one suite can run against several environments.
  ///
  /// Instances give the test runner the environment selection the MCP
  /// server always had: before this, a suite could only hard-code an
  /// environment's flags into `args`.
  #[serde(default)]
  pub instance: Option<String>,
  /// A running browser to connect to instead of launching one, from
  /// `--connect` / `--auto-connect`. Wins over `instance`'s connection.
  #[serde(skip)]
  pub connect: Option<ferridriver::state::ConnectMode>,
  /// Per-instance launch settings, keyed by instance name.
  #[serde(default)]
  pub instances: rustc_hash::FxHashMap<String, crate::browser::InstanceConfig>,
  /// Defaults for instances not listed in `instances`.
  #[serde(default, alias = "default_instance")]
  pub default_instance: Option<crate::browser::InstanceConfig>,
  /// Command producing per-instance browser args (`${INSTANCE}`).
  #[serde(default, alias = "instance_args_command")]
  pub instance_args_command: Option<crate::command_spec::CommandSpec>,
  /// Command discovering a running browser for an instance.
  #[serde(default, alias = "instance_discover_command")]
  pub instance_discover_command: Option<crate::command_spec::CommandSpec>,
  /// Cache TTL in seconds for instance-command output.
  #[serde(default, alias = "command_cache_ttl")]
  pub command_cache_ttl: Option<u64>,
  /// Runtime cache for the instance commands.
  #[serde(skip)]
  pub command_cache: std::sync::Arc<crate::browser::CommandCache>,
  /// The top-level `[browser]` registry, copied in when the document is
  /// resolved so this section can fall back to it. Not a user-writable key
  /// here: it is declared once at the top level.
  #[serde(skip)]
  pub global_browser: Option<crate::browser::BrowserSectionConfig>,
  /// Whether [`Self::normalize`] chose the engine because nobody named
  /// one. The root config is normalized before its projects are merged
  /// from it, and an engine chosen that way must still give way to the
  /// device a project spreads into `use`, as an unset `browser` would.
  #[serde(skip)]
  pub engine_defaulted: bool,
}

/// Pre-seed a `use` block from `device = "<name>"`.
///
/// Only keys the block does not already carry are written, so a `use`
/// key beside the device wins — and where this runs per layer, before
/// the fold, a HIGHER layer's key wins too, by the same rule every other
/// key follows. An unknown device name is left alone: deserialization
/// reports it, where the error can name the file.
pub fn expand_device_keys(block: &mut serde_json::Map<String, serde_json::Value>) {
  use serde_json::Value;

  let Some(name) = block.get("device").and_then(Value::as_str) else {
    return;
  };
  let Some(device) = ferridriver::devices::get(name) else {
    return;
  };

  let size = |s: ferridriver::devices::DeviceSize| {
    let mut m = serde_json::Map::new();
    m.insert("width".to_string(), Value::from(s.width));
    m.insert("height".to_string(), Value::from(s.height));
    Value::Object(m)
  };
  let mut seed: Vec<(&str, Value)> = vec![
    ("userAgent", Value::from(device.user_agent)),
    ("viewport", size(device.viewport)),
    ("isMobile", Value::from(device.is_mobile)),
    ("hasTouch", Value::from(device.has_touch)),
    ("deviceScaleFactor", Value::from(device.device_scale_factor)),
    ("defaultBrowserType", Value::from(device.default_browser_type.as_str())),
  ];
  if let Some(screen) = device.screen {
    seed.push(("screen", size(screen)));
  }
  for (key, value) in seed {
    block.entry(key.to_string()).or_insert(value);
  }
}

#[expect(
  clippy::struct_excessive_bools,
  reason = "each bool is an independent key of Playwright's `use` options; enums would change the wire format"
)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ContextConfig {
  /// A name from Playwright's device registry, pre-seeding every key a
  /// descriptor carries. Expanded where the document is folded, so a
  /// `use` key written beside it — in this layer or a higher one — wins
  /// over what the device would have set.
  pub device: Option<String>,
  pub is_mobile: bool,
  pub has_touch: bool,
  /// `null` is not "unset": Playwright's `viewport: null` disables the
  /// fixed viewport, so absent and explicitly-null have to stay apart.
  /// Present here it wins over `[test.browser].viewport`, the spelling
  /// that predates the `use` bag.
  #[serde(default, deserialize_with = "written_viewport")]
  pub viewport: Option<ViewportOverride>,
  /// The window size the page reports, independent of the viewport.
  /// Absent from Playwright's `DeviceDescriptor` TYPE but present in its
  /// data, and carried by a `...devices[name]` spread.
  pub screen: Option<ViewportConfig>,
  /// Playwright's `use: { browserName }` — the engine this project runs.
  pub browser_name: Option<String>,
  /// Playwright spells `baseURL` inside `use`; `[test].baseUrl` is the
  /// older top-level spelling and loses to it.
  #[serde(rename = "baseURL", alias = "baseUrl")]
  pub base_url: Option<String>,
  /// Default timeout for actions (`click`, `fill`, …), in ms.
  pub action_timeout: Option<u64>,
  /// Default timeout for navigations (`goto`, `waitForURL`, …), in ms.
  pub navigation_timeout: Option<u64>,
  /// `mode | { mode, screenshots?, snapshots?, sources?, attachments? }`.
  pub trace: Option<TraceOption>,
  /// `mode | { mode, size? }`.
  pub video: Option<VideoOption>,
  /// `mode | { mode, fullPage?, omitBackground? }`.
  pub screenshot: Option<ScreenshotOption>,
  /// What a device descriptor names. `browserName` overrides it, and it
  /// applies only while the engine is still at its default, mirroring
  /// Playwright's `browserName` defaulting from `defaultBrowserType`.
  pub default_browser_type: Option<String>,
  #[serde(
    default,
    deserialize_with = "written_color_scheme",
    skip_serializing_if = "Option::is_none"
  )]
  pub color_scheme: Option<ColorSchemeSetting>,
  pub locale: Option<String>,
  pub device_scale_factor: Option<f64>,
  pub offline: bool,
  pub java_script_enabled: bool,
  pub bypass_csp: bool,
  pub accept_downloads: bool,
  pub user_agent: Option<String>,
  pub timezone_id: Option<String>,
  pub geolocation: Option<GeolocationConfig>,
  #[serde(default)]
  pub permissions: Vec<String>,
  #[serde(default)]
  pub extra_http_headers: BTreeMap<String, String>,
  /// One set of credentials or a list, as Playwright's `httpCredentials`.
  #[serde(default, deserialize_with = "one_or_many_credentials")]
  pub http_credentials: Vec<HttpCredentialsConfig>,
  pub ignore_https_errors: bool,
  /// A preset (`slow-3g`, `slow-4g`, `fast-4g`) or `{ latency,
  /// downloadThroughput, uploadThroughput }`. Chromium only.
  pub network_conditions: Option<ferridriver::options::NetworkConditions>,
  /// CPU slowdown factor, `4` for four times slower. Chromium only.
  pub cpu_throttling: Option<f64>,
  pub proxy: Option<ProxyConfig>,
  pub service_workers: Option<String>,
  pub storage_state: Option<String>,
  pub reduced_motion: Option<String>,
  pub forced_colors: Option<String>,
  /// Attribute `getByTestId` reads. Playwright's `use.testIdAttribute`;
  /// a comma-separated list matches any of the named attributes.
  /// `None` = `data-testid`.
  pub test_id_attribute: Option<String>,
  /// Every `use` key no field above claims, kept as raw JSON.
  ///
  /// Playwright's `use` block is open: a key that matches no built-in
  /// option is the value of a user `{ option: true }` fixture. Dropping
  /// it here as "unknown" would make the whole option-fixture feature
  /// unreachable from a config file, so the decision about what a key
  /// means moves to where fixtures are known — after collection.
  #[serde(flatten)]
  pub extra: BTreeMap<String, serde_json::Value>,
}

/// A written `use.colorScheme`. `null` asks for no emulation, so the page
/// follows the browser's own appearance; leaving the key out is different
/// again, and gets `light`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColorSchemeSetting {
  Scheme(String),
  Null,
}

impl ContextConfig {
  /// The `colorScheme` a test sees as its fixture value: the one written,
  /// `light` when none is, and `None` for `null`.
  #[must_use]
  pub fn color_scheme_fixture(&self) -> Option<&str> {
    match &self.color_scheme {
      None => Some("light"),
      Some(ColorSchemeSetting::Scheme(scheme)) => Some(scheme),
      Some(ColorSchemeSetting::Null) => None,
    }
  }
}

impl ColorSchemeSetting {
  #[must_use]
  pub fn media_override(&self) -> ferridriver::options::MediaOverride {
    match self {
      Self::Scheme(scheme) => ferridriver::options::MediaOverride::Set(scheme.clone()),
      Self::Null => ferridriver::options::MediaOverride::Disabled,
    }
  }
}

impl<'de> Deserialize<'de> for ColorSchemeSetting {
  fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
    Ok(Option::<String>::deserialize(de)?.map_or(Self::Null, Self::Scheme))
  }
}

impl Serialize for ColorSchemeSetting {
  fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
    match self {
      Self::Scheme(scheme) => ser.serialize_str(scheme),
      Self::Null => ser.serialize_none(),
    }
  }
}

/// Keep a written `colorScheme: null` apart from an absent key, which
/// `Option`'s own impl would fold together.
fn written_color_scheme<'de, D: serde::Deserializer<'de>>(de: D) -> Result<Option<ColorSchemeSetting>, D::Error> {
  ColorSchemeSetting::deserialize(de).map(Some)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpCredentialsConfig {
  pub username: String,
  pub password: String,
  pub origin: Option<String>,
}

fn one_or_many_credentials<'de, D: serde::Deserializer<'de>>(
  deserializer: D,
) -> Result<Vec<HttpCredentialsConfig>, D::Error> {
  #[derive(Deserialize)]
  #[serde(untagged)]
  enum OneOrMany {
    Many(Vec<HttpCredentialsConfig>),
    One(HttpCredentialsConfig),
  }
  Ok(match Option::<OneOrMany>::deserialize(deserializer)? {
    Some(OneOrMany::Many(list)) => list,
    Some(OneOrMany::One(credentials)) => vec![credentials],
    None => Vec::new(),
  })
}

/// Playwright's `testMatch` / `testIgnore`: one glob, or a list of them.
pub(super) fn one_or_many_patterns<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
  Ok(optional_patterns(deserializer)?.unwrap_or_default())
}

pub(super) fn optional_patterns<'de, D: serde::Deserializer<'de>>(
  deserializer: D,
) -> Result<Option<Vec<String>>, D::Error> {
  #[derive(Deserialize)]
  #[serde(untagged)]
  enum OneOrMany {
    One(String),
    Many(Vec<String>),
  }
  Ok(
    Option::<OneOrMany>::deserialize(deserializer)?.map(|patterns| match patterns {
      OneOrMany::One(pattern) => vec![pattern],
      OneOrMany::Many(patterns) => patterns,
    }),
  )
}

/// Playwright's `reporter`: one reporter's name, or a list of entries.
pub(super) fn one_or_many_reporters<'de, D: serde::Deserializer<'de>>(
  deserializer: D,
) -> Result<Vec<ReporterConfig>, D::Error> {
  #[derive(Deserialize)]
  #[serde(untagged)]
  enum OneOrMany {
    One(String),
    Many(Vec<ReporterConfig>),
  }
  Ok(match Option::<OneOrMany>::deserialize(deserializer)? {
    Some(OneOrMany::One(name)) => vec![ReporterConfig {
      name,
      options: BTreeMap::new(),
    }],
    Some(OneOrMany::Many(reporters)) => reporters,
    None => Vec::new(),
  })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfig {
  pub server: String,
  pub bypass: Option<String>,
  pub username: Option<String>,
  pub password: Option<String>,
}

impl ProxyConfig {
  /// The engine's proxy settings, credentials included.
  #[must_use]
  pub fn to_engine(&self) -> ferridriver::options::ProxyConfig {
    ferridriver::options::ProxyConfig {
      server: self.server.clone(),
      bypass: self.bypass.clone(),
      username: self.username.clone(),
      password: self.password.clone(),
    }
  }
}

impl Default for ContextConfig {
  fn default() -> Self {
    Self {
      device: None,
      is_mobile: false,
      has_touch: false,
      viewport: None,
      screen: None,
      browser_name: None,
      default_browser_type: None,
      base_url: None,
      action_timeout: None,
      navigation_timeout: None,
      trace: None,
      video: None,
      screenshot: None,
      test_id_attribute: None,
      color_scheme: None,
      locale: None,
      device_scale_factor: None,
      offline: false,
      java_script_enabled: true,
      bypass_csp: false,
      accept_downloads: true,
      user_agent: None,
      timezone_id: None,
      geolocation: None,
      permissions: Vec::new(),
      extra_http_headers: BTreeMap::new(),
      http_credentials: Vec::new(),
      ignore_https_errors: false,
      network_conditions: None,
      cpu_throttling: None,
      proxy: None,
      service_workers: None,
      storage_state: None,
      reduced_motion: None,
      forced_colors: None,
      extra: BTreeMap::new(),
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeolocationConfig {
  pub latitude: f64,
  pub longitude: f64,
  pub accuracy: Option<f64>,
}

impl BrowserConfig {
  /// Take the engine from the `use` bag.
  ///
  /// Playwright makes `browserName` an option fixture that defaults from
  /// the `defaultBrowserType` a device descriptor carries, so an
  /// explicit `browserName` always wins while a descriptor only decides
  /// the engine nobody else named. "Nobody else named" is the default
  /// pair — `chromium` over `cdp-pipe` — the same reading a project
  /// browser block already gets.
  pub fn apply_use_engine(&mut self) {
    let unnamed = self.browser == "auto" || self.engine_defaulted;
    if let Some(ref name) = self.use_options.browser_name {
      self.browser.clone_from(name);
    } else if let Some(ref name) = self.use_options.default_browser_type
      && unnamed
    {
      self.browser.clone_from(name);
      if self.engine_defaulted {
        // The backend was filled in alongside the default engine; the
        // device's engine picks its own.
        "auto".clone_into(&mut self.backend);
      }
    }
  }

  /// Normalize browser↔backend consistency after all overrides are applied.
  ///
  /// Ensures `browser` and `backend` agree -- like Playwright where `browserName`
  /// is the single source of truth and the protocol is implicit.
  ///
  /// Rules:
  /// - `backend = "bidi"` implies `browser = "firefox"` (`BiDi` is Firefox-only)
  /// - `browser = "firefox"` implies `backend = "bidi"` (Firefox only speaks `BiDi`)
  /// - `browser = "webkit"` implies `backend = "webkit"`
  /// - Everything else defaults to `browser = "chromium"`, `backend = "cdp-pipe"`
  pub fn normalize(&mut self) {
    if self.browser == "auto" && self.backend == "auto" {
      self.engine_defaulted = true;
    }
    if let Ok((backend, browser)) = self.resolve_kinds() {
      backend.name().clone_into(&mut self.backend);
      browser.name().clone_into(&mut self.browser);
    }
  }

  /// Every accepted `browser` spelling.
  pub const BROWSERS: &'static [&'static str] = &["chromium", "firefox", "webkit", "safari"];

  /// Reject an unknown `browser` or `backend` spelling.
  ///
  /// # Errors
  ///
  /// Returns an error naming the bad value and the valid ones. Without
  /// this, a typo fell through every `match` to Chromium over
  /// `cdp-pipe` and the run looked successful on the wrong engine.
  pub fn validate(&self) -> anyhow::Result<()> {
    self.resolve_kinds()?;
    Ok(())
  }

  /// Instance-routing view over this section, so `[test.browser]` and
  /// `[mcp.browser]` resolve instances through one implementation.
  ///
  /// # Errors
  /// Rejects incompatible browser and protocol choices.
  pub fn routing(&self) -> ferridriver::error::Result<crate::browser::RoutingView<'_>> {
    Ok(crate::browser::RoutingView {
      browser: Some(self.resolve_kinds()?.1),
      global: self.global_browser.as_ref(),
      instances: &self.instances,
      default_instance: self.default_instance.as_ref(),
      args_command: self.instance_args_command.as_ref(),
      discover_command: self.instance_discover_command.as_ref(),
      cache: &self.command_cache,
      cache_ttl: self
        .command_cache_ttl
        .map_or(crate::browser::DEFAULT_CACHE_TTL, std::time::Duration::from_secs),
      backend: self.resolve_kinds()?.0,
    })
  }

  /// Launch overrides for the instance this config selects, or the
  /// section defaults when it names none.
  ///
  /// # Errors
  ///
  /// Propagates an unusable instance name or a failing args command,
  /// which must abort the run rather than silently launch a browser
  /// against the wrong environment.
  pub fn instance_overrides(&self) -> Result<ferridriver::options::InstanceOverrides, String> {
    let Some(instance) = self.instance.as_deref() else {
      return Ok(ferridriver::options::InstanceOverrides::default());
    };
    let routing = self.routing().map_err(|error| error.to_string())?;
    routing.health(instance)?;
    routing.overrides_for(instance)
  }

  /// How to reach the selected instance, when it is already running.
  ///
  /// # Errors
  /// Rejects incompatible browser and protocol choices.
  pub fn resolve_instance(&self) -> ferridriver::error::Result<Option<ferridriver::state::ConnectMode>> {
    if let Some(connect) = &self.connect {
      return Ok(Some(connect.clone()));
    }
    let Some(instance) = self.instance.as_deref() else {
      return Ok(None);
    };
    Ok(self.routing()?.resolve_connect(instance))
  }

  /// The engine-level backend and browser this config selects.
  ///
  /// The single mapping every host reads: the test runner previously
  /// carried two independent copies of this `match` (`runner.rs` and
  /// `fixture.rs`), each silently defaulting an unrecognised value to
  /// Chromium over `cdp-pipe`.
  ///
  /// # Errors
  /// Rejects unknown names and incompatible browser and protocol choices.
  pub fn resolve_kinds(
    &self,
  ) -> ferridriver::error::Result<(ferridriver::backend::BackendKind, ferridriver::options::BrowserKind)> {
    let browser = (self.browser != "auto").then(|| self.browser.parse()).transpose()?;
    let backend = (self.backend != "auto")
      .then(|| ferridriver::backend::BackendKind::parse(&self.backend))
      .transpose()?;
    let selection = ferridriver::options::BrowserSelection::resolve_with_transport(browser, backend, self.transport)?;
    Ok((selection.backend, selection.browser))
  }
}

impl Default for BrowserConfig {
  fn default() -> Self {
    Self {
      browser: "auto".into(),
      backend: "auto".into(),
      transport: None,
      channel: None,
      global_browser: None,
      // Headless unless asked, as Playwright runs: `--headed` (or
      // `headless = false`) opens windows to watch a run.
      headless: true,
      executable_path: None,
      args: Vec::new(),
      viewport: None,
      slow_mo: None,
      firefox_user_prefs: std::collections::BTreeMap::new(),
      use_options: ContextConfig::default(),
      instance: None,
      connect: None,
      instances: rustc_hash::FxHashMap::default(),
      default_instance: None,
      instance_args_command: None,
      instance_discover_command: None,
      command_cache_ttl: None,
      command_cache: std::sync::Arc::default(),
      engine_defaulted: false,
    }
  }
}

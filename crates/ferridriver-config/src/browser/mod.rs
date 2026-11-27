//! Browser instance routing, shared by `[mcp.browser]` and
//! `[test.browser]`.
//!
//! An "instance" is one browser process with its own flags, profile and
//! environment. The MCP server picks one per session key
//! (`<instance>:<context>`); the test runner picks one per project. Both
//! used to be served by completely different config: the MCP section had
//! instances and external resolution commands but no way to set a
//! profile directory, an executable, a proxy or an environment, and the
//! test section had none of the instance concept at all — so a suite
//! could not target the same environment the MCP server was driving.
//!
//! Everything here is declared once and read by both sections.
//!
//! # External commands
//!
//! `instanceArgsCommand` / `instanceDiscoverCommand` are
//! [`CommandSpec`]s, the same schema an extension's `allow.commands`
//! uses. That buys three things the previous bare shell strings could
//! not have:
//!
//! - **argv form** (`["devgate", "browser", "args", "--env", "${INSTANCE}"]`),
//!   which never involves a shell at all;
//! - **safe substitution** — in shell form the instance name is
//!   single-quoted, so a name carrying `;` or `$(...)` cannot break out
//!   of the command (the instance name arrives from a caller-supplied
//!   session key, which made the old `format!`-style interpolation a
//!   command-injection hole);
//! - **a timeout**, so a discover command that polls for a browser
//!   cannot stall a tool call indefinitely.
//!
//! A bare string still deserializes to the shell form, so existing
//! configs keep working.

mod command;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use ferridriver::backend::BackendKind;
use ferridriver::options::InstanceOverrides;
use ferridriver::state::ConnectMode;

use crate::command_spec::{CommandSpec, ResolvedCommand};
use crate::mcp::BackendChoice;

pub use command::{CommandCache, InstanceLaunchSpec, ws_endpoint_is_live};
use command::{apply_command_result, discover_from_profile, first_ws_url, parse_command_result};

/// Default TTL for cached command outputs (5 minutes).
pub const DEFAULT_CACHE_TTL: Duration = Duration::from_mins(5);

/// Timeout for verifying a browser port is responsive.
pub const DISCOVER_TCP_TIMEOUT: Duration = Duration::from_millis(500);

/// Hard ceiling for an instance command that declares no `timeoutMs`.
///
/// Discover commands routinely poll (`devgate browser discover
/// --wait 10`), and the previous implementation had no bound at all: a
/// polling command ran to completion on the blocking pool while the
/// caller waited, twice, because a miss retried.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);

/// Longest accepted instance name.
///
/// The name comes from a caller-supplied session key and is substituted
/// into external commands and into profile-directory paths, so both its
/// length and its characters are validated before it reaches either.
const NAME_MAX: usize = 64;

/// Reject an instance name that must not be substituted anywhere.
///
/// # Errors
///
/// Returns a message naming the offending input.
pub fn validate_instance_name(name: &str) -> Result<(), String> {
  if name.is_empty() {
    return Err("instance name is empty".to_string());
  }
  if name.len() > NAME_MAX {
    return Err(format!("instance name is longer than {NAME_MAX} characters"));
  }
  if let Some(bad) = name
    .chars()
    .find(|c| !(c.is_ascii_alphanumeric() || *c == '.' || *c == '_' || *c == '-'))
  {
    return Err(format!(
      "instance name {name:?} contains {bad:?}; only letters, digits, '.', '_' and '-' are allowed"
    ));
  }
  Ok(())
}

/// Proxy settings for a launched instance.
#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ProxyConfig {
  /// `http://host:port` / `socks5://host:port`.
  pub server: String,
  /// Comma-separated hosts that bypass the proxy.
  pub bypass: Option<String>,
  /// Proxy credentials. No browser takes them as a flag: they answer the
  /// proxy's `407` challenge for every context of the instance.
  pub username: Option<String>,
  pub password: Option<String>,
}

/// `ignoreDefaultArgs`: `true` drops every built-in switch, a list drops
/// just the named ones.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum IgnoreDefaultArgsConfig {
  All(bool),
  Only(Vec<String>),
}

impl IgnoreDefaultArgsConfig {
  /// Lower to the engine's type. `false` means "ignore nothing", which
  /// is the absence of the option.
  #[must_use]
  pub fn lower(&self) -> Option<ferridriver::options::IgnoreDefaultArgs> {
    match self {
      Self::All(true) => Some(ferridriver::options::IgnoreDefaultArgs::All),
      Self::All(false) => None,
      Self::Only(list) => Some(ferridriver::options::IgnoreDefaultArgs::Some(list.clone())),
    }
  }
}

/// Per-instance launch settings.
///
/// Every field overrides the section-level value for this instance only.
#[derive(Debug, Default, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct InstanceConfig {
  pub browser: Option<ferridriver::options::BrowserKind>,
  pub device: Option<ferridriver::device::DeviceTarget>,
  /// Extra browser arguments for this instance.
  #[serde(alias = "chrome_args", alias = "chromeArgs")]
  pub args: Vec<String>,
  /// Command producing this instance's browser args, replacing the
  /// section-level `instanceArgsCommand`.
  ///
  /// A section command is one template for every instance, which cannot serve
  /// instances that need different commands — a Chromium instance asking for
  /// DNS rules and a `WebKit` one asking for proxy flags, say.
  #[serde(alias = "args_command")]
  pub args_command: Option<CommandSpec>,
  /// Command discovering an already-running browser for this instance,
  /// replacing the section-level `instanceDiscoverCommand`.
  ///
  /// The section command is one template for every instance, so it can only
  /// address them by the session-key label. Declaring it here lets an instance
  /// name a browser process without that label having to double as whatever
  /// the command's arguments mean.
  #[serde(alias = "discover_command")]
  pub discover_command: Option<CommandSpec>,
  /// Explicit WebSocket URL to connect to (skips launch entirely).
  #[serde(alias = "connect_url")]
  pub connect_url: Option<String>,
  pub connect_options: Option<ferridriver::options::ConnectOptions>,
  /// Profile directory to read `DevToolsActivePort` from when
  /// discovering an already-running browser. `${INSTANCE}` and `~` are
  /// expanded.
  #[serde(alias = "discover_profile")]
  pub discover_profile: Option<String>,
  /// Profile directory to LAUNCH with. Without this every launch gets a
  /// throwaway profile, so cookies and logins vanish on restart and an
  /// external manager can never find the browser again.
  #[serde(alias = "user_data_dir")]
  pub user_data_dir: Option<String>,
  /// Browser binary for this instance.
  #[serde(alias = "executable_path")]
  pub executable_path: Option<String>,
  /// Headless override for this instance.
  pub headless: Option<bool>,
  /// Backend protocol override for this instance (`cdp`, `bidi`, `webkit`, `webdriver`,
  /// or legacy `cdp-pipe` / `cdp-ws`).
  #[serde(alias = "protocol")]
  pub backend: Option<BackendChoice>,
  /// CDP wire transport override (`pipe` or `ws`) for this instance.
  pub transport: Option<ferridriver::options::ChromiumTransport>,
  /// Extra environment variables for the browser process.
  pub env: BTreeMap<String, String>,
  /// Proxy for this instance, lowered into launch flags.
  pub proxy: Option<ProxyConfig>,
  /// Built-in switches to drop for this instance.
  #[serde(alias = "ignore_default_args")]
  pub ignore_default_args: Option<IgnoreDefaultArgsConfig>,
}

/// Lower one [`InstanceConfig`] to the engine's launch-override type,
/// expanding `${INSTANCE}` in its paths.
///
/// `section_backend` is the backend the instance runs on when it names none of
/// its own; the proxy flags differ per backend, so lowering without it picks
/// the wrong switch.
///
/// # Errors
///
/// Returns an error when the instance's browser and backend disagree.
pub fn instance_overrides_from(
  cfg: &InstanceConfig,
  instance: &str,
  section_backend: BackendKind,
) -> Result<InstanceOverrides, String> {
  let selection = ferridriver::options::BrowserSelection::resolve_with_transport(
    cfg.browser,
    cfg
      .backend
      .map(BackendChoice::kind)
      .or_else(|| cfg.browser.is_none().then_some(section_backend)),
    cfg.transport,
  )
  .map_err(|error| error.to_string())?;
  let backend = selection.backend;
  let mut args = cfg.args.clone();
  if let Some(proxy) = &cfg.proxy {
    args.extend(proxy_launch_flags(&proxy.server, proxy.bypass.as_deref(), backend));
  }
  Ok(InstanceOverrides {
    kind: cfg.browser.map(|_| selection.browser),
    args,
    user_data_dir: expand_instance_path(cfg.user_data_dir.as_deref(), instance),
    executable_path: cfg.executable_path.clone(),
    headless: cfg
      .headless
      .or_else(|| (cfg.browser == Some(ferridriver::options::BrowserKind::Safari)).then_some(false)),
    backend: (cfg.backend.is_some() || cfg.transport.is_some() || cfg.browser.is_some()).then_some(backend),
    env: cfg.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    ignore_default_args: cfg
      .ignore_default_args
      .as_ref()
      .and_then(IgnoreDefaultArgsConfig::lower),
    launched_proxy: cfg.proxy.as_ref().map(ProxyConfig::to_engine),
  })
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewportConfig {
  pub width: i64,
  pub height: i64,
}

impl Default for ViewportConfig {
  fn default() -> Self {
    Self {
      width: ferridriver::state::DEFAULT_VIEWPORT_WIDTH,
      height: ferridriver::state::DEFAULT_VIEWPORT_HEIGHT,
    }
  }
}

/// A written `viewport` key, wherever it is spelled.
///
/// Three states have to stay apart: absent (the Playwright default
/// 1280x720 applies), a size, and Playwright's `viewport: null`, which
/// asks for NO fixed viewport at all. Absent is the field's `Option`;
/// this enum is the other two.
#[derive(Debug, Clone)]
pub enum ViewportOverride {
  Size(ViewportConfig),
  /// `viewport: null` — the window is not resized and the page gets
  /// whatever the browser window gives it.
  Disabled,
}

impl ViewportOverride {
  /// The size, or `None` for `viewport: null`.
  #[must_use]
  pub fn size(&self) -> Option<ViewportConfig> {
    match self {
      Self::Size(v) => Some(v.clone()),
      Self::Disabled => None,
    }
  }
}

// Hand-written rather than `#[serde(untagged)]`: an untagged enum
// collapses every field error into "did not match any variant", so a
// viewport missing `height` would stop naming `height`.
impl<'de> Deserialize<'de> for ViewportOverride {
  fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
    Ok(Option::<ViewportConfig>::deserialize(de)?.map_or(Self::Disabled, Self::Size))
  }
}

impl Serialize for ViewportOverride {
  fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
    match self {
      Self::Size(v) => v.serialize(ser),
      Self::Disabled => ser.serialize_none(),
    }
  }
}

/// Keep a written `viewport` — `null` included — apart from an absent
/// one. `Option`'s own impl swallows `null` into `None`, which is the
/// state that means "nobody said".
pub(crate) fn written_viewport<'de, D>(de: D) -> Result<Option<ViewportOverride>, D::Error>
where
  D: serde::Deserializer<'de>,
{
  ViewportOverride::deserialize(de).map(Some)
}

/// The instance registry, declared once for every host.
///
/// An instance names a browser process and how to reach it; that fact does not
/// change with who is asking. `[mcp.browser]` and `[test.browser]` shared the
/// instance SCHEMA already, but each kept its own map, so a set usable by both
/// had to be written twice — and a section that simply forgot would silently
/// launch an unconfigured browser instead of naming the instance it could not
/// find.
///
/// A section may still declare its own instances; those merge OVER these, key
/// by key, so a host can override one entry without restating the rest.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserSectionConfig {
  pub browser: Option<ferridriver::options::BrowserKind>,
  /// Protocol/backend every host launches over unless its section says otherwise.
  #[serde(alias = "protocol")]
  pub backend: Option<BackendChoice>,
  /// CDP wire transport (`pipe` or `ws`) every host uses unless its section says otherwise.
  pub transport: Option<ferridriver::options::ChromiumTransport>,
  /// Headed/headless default every host inherits unless its section says
  /// otherwise.
  pub headless: Option<bool>,
  /// Viewport every host emulates on the pages it opens, unless its
  /// section says otherwise.
  ///
  /// Absent means Playwright's default 1280x720 — NOT "no viewport".
  /// A browser reached through a persistent profile restores the window
  /// bounds its last run left behind, so a host that emulates nothing
  /// silently inherits whatever size someone resized that window to.
  /// `viewport: null` is how a config asks for that on purpose.
  #[serde(default, deserialize_with = "written_viewport")]
  pub viewport: Option<ViewportOverride>,
  /// Instances every host can select by name.
  pub instances: rustc_hash::FxHashMap<String, InstanceConfig>,
  /// Settings for an instance no entry claims.
  pub default_instance: Option<InstanceConfig>,
  /// Args command for instances that declare none of their own.
  pub instance_args_command: Option<CommandSpec>,
  /// Discover command for instances that declare none of their own.
  pub instance_discover_command: Option<CommandSpec>,
  /// Cache TTL in seconds for command outputs (default: 300).
  pub command_cache_ttl: Option<u64>,
}

/// Borrowed view of a section's instance-routing config, so one
/// implementation serves `[mcp.browser]` and `[test.browser]`.
pub struct RoutingView<'a> {
  pub browser: Option<ferridriver::options::BrowserKind>,
  pub instances: &'a rustc_hash::FxHashMap<String, InstanceConfig>,
  /// The top-level `[browser]` registry, consulted when the section does not
  /// claim a name. A section entry of the same name wins outright.
  pub global: Option<&'a BrowserSectionConfig>,
  pub default_instance: Option<&'a InstanceConfig>,
  pub args_command: Option<&'a CommandSpec>,
  pub discover_command: Option<&'a CommandSpec>,
  pub cache: &'a CommandCache,
  pub cache_ttl: Duration,
  /// Backend an instance runs on unless it names its own.
  pub backend: BackendKind,
}

impl RoutingView<'_> {
  /// The entry that claims `instance`: the section's own first, then the
  /// top-level registry, then whichever default is declared.
  pub(crate) fn config_for(&self, instance: &str) -> Option<&InstanceConfig> {
    self
      .instances
      .get(instance)
      .or_else(|| self.global.and_then(|g| g.instances.get(instance)))
      .or(self.default_instance)
      .or_else(|| self.global.and_then(|g| g.default_instance.as_ref()))
  }

  /// Every instance name any layer declares, for diagnostics and for deciding
  /// whether a name is unknown.
  fn declared_names(&self) -> Vec<&str> {
    let mut names: Vec<&str> = self.instances.keys().map(String::as_str).collect();
    if let Some(g) = self.global {
      let extra: Vec<&str> = g
        .instances
        .keys()
        .map(String::as_str)
        .filter(|n| !names.contains(n))
        .collect();
      names.extend(extra);
    }
    names.sort_unstable();
    names
  }

  /// Whether any layer declares an instance at all. With none, a name cannot
  /// be "unknown" — the section command answers for whatever it is given.
  fn has_declared_instances(&self) -> bool {
    !self.instances.is_empty() || self.global.is_some_and(|g| !g.instances.is_empty())
  }

  /// The section's args command, falling back to the top-level one.
  fn section_args_command(&self) -> Option<&CommandSpec> {
    self
      .args_command
      .or_else(|| self.global.and_then(|g| g.instance_args_command.as_ref()))
  }

  /// The section's discover command, falling back to the top-level one.
  fn section_discover_command(&self) -> Option<&CommandSpec> {
    self
      .discover_command
      .or_else(|| self.global.and_then(|g| g.instance_discover_command.as_ref()))
  }

  /// Resolve every launch setting for `instance`.
  ///
  /// # Errors
  ///
  /// Returns an error when the instance name is not substitutable, when
  /// the args command fails (a hard failure means the name is wrong —
  /// almost always a session key with no `:` that landed on `default` —
  /// and launching anyway puts the caller on an unconfigured browser).
  /// Reject a name no configured instance claims, naming the ones that exist.
  ///
  /// This has to run BEFORE the args command. Feeding an undeclared name to a
  /// template like `--env ${INSTANCE}` surfaces the failure as that command's
  /// own error — a clap usage message from a binary the caller may never have
  /// heard of — while the configured set, which is the answer, goes unmentioned.
  pub(crate) fn reject_unknown_instance(&self, instance: &str) -> Result<(), String> {
    if !self.has_declared_instances() || self.config_for(instance).is_some() {
      return Ok(());
    }
    let known = self.declared_names();
    Err(format!(
      "unknown instance '{instance}'; configured instances are: {}. \
       The part before ':' in a session key names a configured browser instance, \
       not a free-form label — add '{instance}' under [browser.instances] to give \
       it its own browser process.",
      known.join(", ")
    ))
  }

  /// Resolve every launch setting for `instance`.
  ///
  /// # Errors
  ///
  /// Returns an error when the instance name is not substitutable, when no
  /// configured instance claims it, when the args command fails or returns a
  /// result carrying no launch settings (launching anyway puts the caller on an
  /// unconfigured browser).
  pub fn overrides_for(&self, instance: &str) -> Result<InstanceOverrides, String> {
    validate_instance_name(instance)?;
    // Before anything else, and whether or not a command exists: a declared
    // set that does not contain this name means the caller asked for a browser
    // nothing describes, and the configured set is the answer they need.
    self.reject_unknown_instance(instance)?;
    self.validate_connection(instance)?;

    if let Some(cfg) = self.config_for(instance)
      && let Some(device) = &cfg.device
    {
      return Ok(InstanceOverrides {
        kind: Some(device.browser()),
        backend: Some(device.backend()),
        args: cfg.args.clone(),
        ..InstanceOverrides::default()
      });
    }
    let mut out = match self.config_for(instance) {
      Some(cfg) => instance_overrides_from(cfg, instance, self.backend)?,
      None => InstanceOverrides::default(),
    };

    // The instance's own command replaces the section's rather than adding to
    // it: two commands would each contribute a full set of launch args.
    let command = self
      .config_for(instance)
      .and_then(|cfg| cfg.args_command.as_ref())
      .or_else(|| self.section_args_command());

    if let Some(spec) = command {
      let resolved = resolve_for_instance(spec, instance)?;
      let value = self.cache.get_or_exec(&resolved, self.cache_ttl)?;
      let parsed = parse_command_result(&value).map_err(|e| format!("cannot start instance '{instance}': {e}"))?;
      apply_command_result(&mut out, parsed, self.backend);
    }

    Ok(out)
  }

  /// Check that `instance` can be started before a browser is launched.
  ///
  /// # Errors
  ///
  /// Returns an actionable error when the name is invalid or its args
  /// command hard-fails.
  pub fn health(&self, instance: &str) -> Result<(), String> {
    validate_instance_name(instance).map_err(|e| {
      format!(
        "cannot start instance '{instance}': {e}. If you meant an environment, \
         set the session to '<env>:<context>' (e.g. 'staging:admin')."
      )
    })?;
    // Nothing can supply settings for a name no entry claims, with or without
    // an args command: launching would produce a browser with none of the
    // configuration the caller asked for.
    self.reject_unknown_instance(instance)?;
    self.validate_connection(instance)?;
    if self.config_for(instance).is_some_and(|cfg| cfg.device.is_some()) {
      return Ok(());
    }
    let Some(spec) = self.section_args_command() else {
      return Ok(());
    };
    let resolved = resolve_for_instance(spec, instance)?;
    self
      .cache
      .get_or_exec(&resolved, self.cache_ttl)
      .map(|_| ())
      .map_err(|e| {
        format!(
          "cannot start instance '{instance}': its args command failed ({e}). \
         If you meant an environment, set the session to '<env>:<context>' \
         (e.g. 'staging:admin') — a session with no ':' selects the 'default' \
         instance, which has no environment mapping."
        )
      })
  }

  fn validate_connection(&self, instance: &str) -> Result<(), String> {
    if let Some(cfg) = self.config_for(instance)
      && let Some(device) = &cfg.device
      && (cfg.connect_url.is_some()
        || cfg.connect_options.is_some()
        || cfg.discover_command.is_some()
        || cfg.discover_profile.is_some()
        || cfg.user_data_dir.is_some()
        || cfg.executable_path.is_some()
        || (!cfg.args.is_empty() && !matches!(device, ferridriver::device::DeviceTarget::Android(_)))
        || cfg.args_command.is_some()
        || !cfg.env.is_empty()
        || cfg.proxy.is_some()
        || cfg.ignore_default_args.is_some()
        || cfg.browser.is_some_and(|browser| browser != device.browser())
        || cfg.backend.is_some_and(|backend| backend.kind() != device.backend()))
    {
      return Err(format!(
        "instance '{instance}': a managed device cannot specify a remote endpoint, desktop launch options, discovery command, or conflicting browser/backend"
      ));
    }
    if let Some(cfg) = self.config_for(instance)
      && cfg.connect_options.is_some()
      && cfg.connect_url.is_none()
    {
      return Err(format!("instance '{instance}': connectOptions requires connectUrl"));
    }
    Ok(())
  }

  /// Resolve how to reach `instance`: an explicit URL, a discovered
  /// profile, or a discover command. `None` means "launch a new one".
  #[must_use]
  pub fn resolve_connect(&self, instance: &str) -> Option<ConnectMode> {
    if validate_instance_name(instance).is_err() {
      return None;
    }

    let cfg = self.config_for(instance);
    if let Some(cfg) = cfg
      && let Some(target) = &cfg.device
    {
      return Some(ConnectMode::Device {
        target: target.clone(),
        headless: cfg
          .headless
          .or_else(|| self.global.and_then(|g| g.headless))
          .unwrap_or(false),
      });
    }

    if let Some(cfg) = cfg
      && let Some(url) = &cfg.connect_url
    {
      // A WebSocket URL is the browser's own CDP or BiDi endpoint, never a
      // WebDriver server, so `connectOptions` only adds its headers and
      // timeout to the handshake.
      if url.starts_with("ws://") || url.starts_with("wss://") {
        return Some(match &cfg.connect_options {
          Some(options) => ConnectMode::ConnectUrlWith {
            url: url.clone(),
            headers: options.headers.clone(),
            timeout: options.timeout,
          },
          None => ConnectMode::ConnectUrl(url.clone()),
        });
      }
      let inherited_browser = self.browser.or_else(|| self.global.and_then(|global| global.browser));
      let browser = cfg
        .browser
        .or_else(|| inherited_browser.filter(|_| cfg.backend.is_none()));
      if cfg.connect_options.is_some()
        || ((matches!(
          cfg.backend.or_else(|| cfg.browser.is_none().then_some(self.backend)),
          Some(BackendKind::WebDriver | BackendKind::Bidi)
        ) || matches!(
          browser,
          Some(
            ferridriver::options::BrowserKind::Safari
              | ferridriver::options::BrowserKind::Firefox
              | ferridriver::options::BrowserKind::Ladybird
          )
        )) && (url.starts_with("http://") || url.starts_with("https://")))
      {
        let options = cfg.connect_options.clone().unwrap_or_default();
        let browser_name = options
          .capabilities
          .as_ref()
          .and_then(|caps| caps.get("browserName"))
          .and_then(serde_json::Value::as_str)
          .unwrap_or_else(|| match browser {
            Some(ferridriver::options::BrowserKind::Safari | ferridriver::options::BrowserKind::WebKit) => "safari",
            Some(ferridriver::options::BrowserKind::Firefox) => "firefox",
            Some(ferridriver::options::BrowserKind::Ladybird) => "ladybird",
            Some(ferridriver::options::BrowserKind::Lightpanda) => "lightpanda",
            Some(ferridriver::options::BrowserKind::Chromium) => "chrome",
            None => match cfg.backend.map_or(self.backend, BackendChoice::kind) {
              BackendKind::WebKit => "safari",
              BackendKind::Bidi => "firefox",
              _ => "chrome",
            },
          })
          .to_owned();
        return Some(ConnectMode::WebDriver {
          endpoint: url.clone(),
          browser_name,
          protocol: ferridriver::backend::webdriver::WebDriverProtocol::from_backend(
            cfg
              .backend
              .map(BackendChoice::kind)
              .or_else(|| cfg.browser.is_none().then_some(self.backend)),
          ),
          capabilities: options.capabilities.clone(),
          headers: options.headers.clone(),
          timeout: options.timeout,
        });
      }
      return Some(ConnectMode::ConnectUrl(url.clone()));
    }

    // Discovery answers "is there a browser already running I can attach to",
    // and both answers it knows -- a `DevToolsActivePort` file and a `ws://`
    // endpoint -- are CDP. A WebKit browser is driven over an inspector pipe
    // its launcher owns, with no endpoint for anyone else to attach to, so
    // running the section's discover command for a WebKit instance can only
    // waste its timeout before every launch.
    let backend = cfg.and_then(|c| c.backend).map_or(self.backend, BackendChoice::kind);
    if backend == BackendKind::WebKit {
      return None;
    }

    if let Some(cfg) = cfg {
      // A stale profile means "the browser this profile described is
      // gone", NOT "stop looking" — the previous code returned early
      // here and never tried the discover command.
      if let Some(template) = &cfg.discover_profile
        && let Some(mode) = discover_from_profile(template, instance)
      {
        return Some(mode);
      }
    }

    // The instance's own command replaces the section's, exactly as its
    // `argsCommand` does.
    let spec = self
      .config_for(instance)
      .and_then(|cfg| cfg.discover_command.as_ref())
      .or_else(|| self.section_discover_command())?;
    let resolved = resolve_for_instance(spec, instance).ok()?;
    self.discover_via_command(&resolved)
  }

  /// Run a discover command and return a LIVE endpoint.
  ///
  /// The happy path is cached, but a cached URL is always TCP-probed
  /// because a browser can restart on a new port inside the TTL. A miss
  /// evicts the entry so the next call rediscovers — and, unlike
  /// before, does NOT immediately re-run the command: the retry doubled
  /// the wall-clock cost of every cold start (two full `--wait 10`
  /// polls) to cover a race the caller retries anyway.
  fn discover_via_command(&self, resolved: &ResolvedCommand) -> Option<ConnectMode> {
    let value = self.cache.get_or_exec(resolved, self.cache_ttl).ok()?;
    let url = first_ws_url(&value)?;
    if ws_endpoint_is_live(&url) {
      return Some(ConnectMode::ConnectUrl(url));
    }
    self.cache.evict(resolved);
    None
  }
}

/// Substitute `${INSTANCE}` in a command spec.
fn resolve_for_instance(spec: &CommandSpec, instance: &str) -> Result<ResolvedCommand, String> {
  let mut vars = BTreeMap::new();
  vars.insert("INSTANCE".to_string(), serde_json::Value::String(instance.to_string()));
  spec.resolve(&vars)
}

/// Expand `${INSTANCE}` and `~` in a per-instance path.
pub(super) fn expand_instance_path(template: Option<&str>, instance: &str) -> Option<String> {
  let template = template?;
  let substituted = template.replace("${INSTANCE}", instance);
  Some(shellexpand::tilde(&substituted).into_owned())
}

/// Spell the proxy launch flags the way `backend`'s binary takes them.
///
/// Thin wrapper over the engine's own lowering so a config-declared proxy and
/// a `launch({ proxy })` proxy can never disagree about the switch name.
#[must_use]
pub fn proxy_launch_flags(server: &str, bypass: Option<&str>, backend: BackendKind) -> Vec<String> {
  ferridriver::options::ProxyConfig {
    server: server.to_string(),
    bypass: bypass.map(ToString::to_string),
    username: None,
    password: None,
  }
  .launch_flags(backend)
}

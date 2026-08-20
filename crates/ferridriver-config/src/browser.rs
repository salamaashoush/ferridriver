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

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use ferridriver::options::InstanceOverrides;
use ferridriver::state::ConnectMode;

use crate::command_spec::{CommandOutput, CommandSpec, ResolvedCommand, ResolvedExec};
use crate::mcp::BackendChoice;
use ferridriver::backend::BackendKind;

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
  /// Proxy credentials are NOT a launch flag; Chrome asks for them over
  /// the wire. Declaring them here is an error rather than a silent
  /// drop — use the context-level proxy (`[test.browser.use].proxy`).
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
  /// Backend override for this instance.
  pub backend: Option<BackendChoice>,
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
/// Returns an error when the proxy declares credentials (not a launch
/// flag) — surfaced rather than silently dropped.
pub fn instance_overrides_from(
  cfg: &InstanceConfig,
  instance: &str,
  section_backend: BackendKind,
) -> Result<InstanceOverrides, String> {
  let backend = cfg.backend.map_or(section_backend, BackendChoice::kind);
  let mut args = cfg.args.clone();
  if let Some(proxy) = &cfg.proxy {
    args.extend(proxy_args(proxy, backend)?);
  }
  Ok(InstanceOverrides {
    args,
    user_data_dir: expand_instance_path(cfg.user_data_dir.as_deref(), instance),
    executable_path: cfg.executable_path.clone(),
    headless: cfg.headless,
    backend: cfg.backend.map(BackendChoice::kind),
    env: cfg.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
    ignore_default_args: cfg
      .ignore_default_args
      .as_ref()
      .and_then(IgnoreDefaultArgsConfig::lower),
  })
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
  /// Transport every host launches over unless its section says otherwise.
  pub backend: Option<BackendChoice>,
  /// Headed/headless default every host inherits unless its section says
  /// otherwise.
  pub headless: Option<bool>,
  /// Instances every host can select by name.
  pub instances: std::collections::HashMap<String, InstanceConfig>,
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
  pub instances: &'a std::collections::HashMap<String, InstanceConfig>,
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
  fn config_for(&self, instance: &str) -> Option<&InstanceConfig> {
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
  /// and launching anyway puts the caller on an unconfigured browser),
  /// or when the instance declares proxy credentials.
  /// Reject a name no configured instance claims, naming the ones that exist.
  ///
  /// This has to run BEFORE the args command. Feeding an undeclared name to a
  /// template like `--env ${INSTANCE}` surfaces the failure as that command's
  /// own error — a clap usage message from a binary the caller may never have
  /// heard of — while the configured set, which is the answer, goes unmentioned.
  fn reject_unknown_instance(&self, instance: &str) -> Result<(), String> {
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
  /// unconfigured browser), or when the instance declares proxy credentials.
  pub fn overrides_for(&self, instance: &str) -> Result<InstanceOverrides, String> {
    validate_instance_name(instance)?;
    // Before anything else, and whether or not a command exists: a declared
    // set that does not contain this name means the caller asked for a browser
    // nothing describes, and the configured set is the answer they need.
    self.reject_unknown_instance(instance)?;

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

  /// Resolve how to reach `instance`: an explicit URL, a discovered
  /// profile, or a discover command. `None` means "launch a new one".
  #[must_use]
  pub fn resolve_connect(&self, instance: &str) -> Option<ConnectMode> {
    if validate_instance_name(instance).is_err() {
      return None;
    }

    let cfg = self.config_for(instance);

    if let Some(cfg) = cfg
      && let Some(url) = &cfg.connect_url
    {
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
fn expand_instance_path(template: Option<&str>, instance: &str) -> Option<String> {
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

/// Lower instance proxy settings into launch flags for `backend`.
///
/// # Errors
///
/// Returns an error when the proxy declares credentials, which are not a
/// launch flag.
pub fn proxy_args(proxy: &ProxyConfig, backend: BackendKind) -> Result<Vec<String>, String> {
  if proxy.username.is_some() || proxy.password.is_some() {
    return Err(
      "instance proxy credentials are not launch flags; set them on the context proxy \
       (`[test.browser.use].proxy`) instead of the instance"
        .to_string(),
    );
  }

  Ok(proxy_launch_flags(&proxy.server, proxy.bypass.as_deref(), backend))
}

/// Shape a command's output into browser arguments.
///
/// Accepts a JSON array of strings, a JSON object with an `args` array
/// (what `devgate browser args --json` emits), or plain text with
/// one argument per line.
/// What ferridriver asks a browser args command to return.
///
/// This is OUR contract, not any particular launcher's. A command knows things
/// the config cannot state ahead of time — where a managed profile lives, which
/// browser build is usable, what proxy is up — so it supplies the whole launch
/// shape rather than only flags. Any program that emits this shape can drive an
/// instance; nothing here is specific to the tool that happens to be configured.
///
/// Unknown keys are IGNORED rather than rejected: the command is a separate
/// program on its own release cycle, and a launcher adding a field it thinks we
/// might want must not break every instance that uses it. Known keys are typed
/// strictly, so a wrong type is an error naming the key instead of a silently
/// dropped setting.
#[derive(Debug, Default, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceLaunchSpec {
  /// Browser flags, appended to whatever the instance already declares.
  #[serde(default)]
  pub args: Vec<String>,
  /// Profile directory to launch with. `profileDir` is the spelling a launcher
  /// uses for a directory it manages; `userDataDir` is Chrome's own.
  #[serde(default, alias = "profileDir")]
  pub user_data_dir: Option<String>,
  /// Browser binary to launch.
  #[serde(default)]
  pub executable_path: Option<String>,
  /// Environment variables for the browser process.
  #[serde(default)]
  pub env: BTreeMap<String, String>,
  /// Proxy the browser should route through, lowered into launch flags.
  #[serde(default)]
  pub proxy_url: Option<String>,
  /// Hosts that bypass `proxyUrl`, in Chrome's comma-separated spelling.
  #[serde(default)]
  pub proxy_bypass: Option<String>,
  /// Headless override.
  #[serde(default)]
  pub headless: Option<bool>,
}

/// Read a command's stdout into [`InstanceLaunchSpec`].
///
/// Three legacy shapes stay supported because they are already documented: a
/// JSON array of flags, and newline-delimited text. Anything else that is not
/// an object is an error rather than an empty result — a command returning a
/// number or a bare `true` has failed, and silently launching an unconfigured
/// browser is the failure mode this whole path exists to prevent.
fn parse_command_result(value: &serde_json::Value) -> Result<InstanceLaunchSpec, String> {
  match value {
    serde_json::Value::Object(_) => serde_json::from_value(value.clone()).map_err(|e| {
      format!(
        "its args command returned an unusable result: {e}. Expected an object with any of \
         args, userDataDir (or profileDir), executablePath, env, proxyUrl, proxyBypass, headless"
      )
    }),
    serde_json::Value::Array(_) | serde_json::Value::String(_) => Ok(InstanceLaunchSpec {
      args: value_to_args(value),
      ..InstanceLaunchSpec::default()
    }),
    other => Err(format!(
      "its args command returned {}, which carries no launch settings; \
       expected an object, an array of flags, or newline-separated flags",
      match other {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        _ => "a number",
      }
    )),
  }
}

/// Fold a parsed command result into the launch settings.
///
/// Explicit config wins: a value the operator wrote down is never replaced by a
/// derived one. The command fills only what config left unset.
fn apply_command_result(out: &mut InstanceOverrides, spec: InstanceLaunchSpec, backend: BackendKind) {
  out.args.extend(spec.args);

  if let Some(url) = spec.proxy_url.filter(|u| !u.is_empty()) {
    out
      .args
      .extend(proxy_launch_flags(&url, spec.proxy_bypass.as_deref(), backend));
  }
  if out.user_data_dir.is_none() {
    out.user_data_dir = spec.user_data_dir.filter(|d| !d.is_empty());
  }
  if out.executable_path.is_none() {
    out.executable_path = spec.executable_path.filter(|p| !p.is_empty());
  }
  if out.headless.is_none() {
    out.headless = spec.headless;
  }
  for (key, val) in spec.env {
    // Config-set variables stay put; the command only adds.
    out.env.entry(key).or_insert(val);
  }
}

fn value_to_args(value: &serde_json::Value) -> Vec<String> {
  match value {
    serde_json::Value::Array(items) => items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
    serde_json::Value::Object(map) => map
      .get("args")
      .and_then(serde_json::Value::as_array)
      .map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
      .unwrap_or_default(),
    serde_json::Value::String(text) => text
      .lines()
      .map(str::trim)
      .filter(|l| !l.is_empty())
      .map(str::to_string)
      .collect(),
    _ => Vec::new(),
  }
}

/// First `ws(s)://` URL in a command's output.
fn first_ws_url(value: &serde_json::Value) -> Option<String> {
  let candidates: Vec<String> = match value {
    serde_json::Value::String(text) => text.lines().map(|l| l.trim().to_string()).collect(),
    serde_json::Value::Array(items) => items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
    // Both spellings: a discover command written in Rust/Go tends to emit
    // snake_case, and a JSON object whose ws URL we cannot find looks
    // exactly like "no browser running" — the most confusing possible
    // failure for something whose whole job is finding one.
    serde_json::Value::Object(map) => ["wsEndpoint", "webSocketDebuggerUrl", "ws_url", "wsUrl", "url"]
      .iter()
      .filter_map(|k| map.get(*k).and_then(serde_json::Value::as_str).map(str::to_string))
      .collect(),
    _ => Vec::new(),
  };
  candidates
    .into_iter()
    .find(|c| c.starts_with("ws://") || c.starts_with("wss://"))
}

/// Read `DevToolsActivePort` from a profile directory, returning a
/// connect mode only when the browser there is actually answering.
fn discover_from_profile(template: &str, instance: &str) -> Option<ConnectMode> {
  let expanded = expand_instance_path(Some(template), instance)?;
  let profile_dir = Path::new(&expanded);
  let content = std::fs::read_to_string(profile_dir.join("DevToolsActivePort")).ok()?;
  let mut lines = content.lines();
  let port = lines.next()?.parse::<u16>().ok()?;
  let path = lines.next().unwrap_or("/");

  let addr = format!("127.0.0.1:{port}").parse().ok()?;
  std::net::TcpStream::connect_timeout(&addr, DISCOVER_TCP_TIMEOUT)
    .ok()
    .map(|_| ConnectMode::ConnectUrl(format!("ws://127.0.0.1:{port}{path}")))
}

/// TCP-probe a `ws(s)://host:port/...` URL. An unparseable or portless
/// authority counts as not live, so a bogus endpoint is refused rather
/// than hung on.
#[must_use]
pub fn ws_endpoint_is_live(url: &str) -> bool {
  use std::net::ToSocketAddrs;

  let Some(rest) = url.strip_prefix("ws://").or_else(|| url.strip_prefix("wss://")) else {
    return false;
  };
  let authority = rest.split('/').next().unwrap_or("");
  if authority.is_empty() {
    return false;
  }
  match authority.to_socket_addrs() {
    Ok(addrs) => addrs
      .into_iter()
      .any(|addr| std::net::TcpStream::connect_timeout(&addr, DISCOVER_TCP_TIMEOUT).is_ok()),
    Err(_) => false,
  }
}

/// TTL cache for instance-command output, keyed by the RESOLVED command
/// so two instances never share an entry.
#[derive(Debug, Default)]
pub struct CommandCache {
  entries: Mutex<std::collections::HashMap<String, CacheEntry>>,
  /// One lock per key, so concurrent misses run the command ONCE.
  ///
  /// Without it every cold session spawned its own copy: a discover
  /// command that polls (`--wait 10`) then cost N × 10s of wall clock and
  /// N browser probes to answer a question with one answer.
  inflight: Mutex<std::collections::HashMap<String, Arc<Mutex<()>>>>,
}

#[derive(Debug, Clone)]
struct CacheEntry {
  value: serde_json::Value,
  created: Instant,
}

impl CommandCache {
  /// Cache identity of a resolved command.
  ///
  /// Includes `cwd`: the same command line run from two directories is
  /// two different commands (a repo-relative helper script resolves
  /// differently), and sharing one entry served the wrong answer to
  /// whichever caller arrived second.
  fn key(resolved: &ResolvedCommand) -> String {
    let cwd = resolved.cwd.as_deref().unwrap_or_default();
    match &resolved.exec {
      ResolvedExec::Shell(line) => format!("{cwd}\u{1}sh:{line}"),
      ResolvedExec::Argv(argv) => format!("{cwd}\u{1}argv:{}", argv.join("\u{1}")),
    }
  }

  fn fresh(&self, key: &str, ttl: Duration) -> Option<serde_json::Value> {
    let cache = self.entries.lock().ok()?;
    let entry = cache.get(key)?;
    (entry.created.elapsed() < ttl).then(|| entry.value.clone())
  }

  /// Cached output, or run the command and cache it.
  ///
  /// # Errors
  ///
  /// Returns the command's failure message.
  pub fn get_or_exec(&self, resolved: &ResolvedCommand, ttl: Duration) -> Result<serde_json::Value, String> {
    let key = Self::key(resolved);
    if let Some(value) = self.fresh(&key, ttl) {
      return Ok(value);
    }

    let gate = {
      let Ok(mut inflight) = self.inflight.lock() else {
        // A poisoned gate must not turn into "never run the command".
        return execute(resolved);
      };
      Arc::clone(inflight.entry(key.clone()).or_default())
    };
    let _run = gate.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

    // The winner of the gate filled the entry while we waited.
    if let Some(value) = self.fresh(&key, ttl) {
      return Ok(value);
    }

    let value = execute(resolved)?;

    if let Ok(mut cache) = self.entries.lock() {
      cache.insert(
        key,
        CacheEntry {
          value: value.clone(),
          created: Instant::now(),
        },
      );
    }
    Ok(value)
  }

  /// Drop a cached entry so the next call re-runs the command.
  pub fn evict(&self, resolved: &ResolvedCommand) {
    if let Ok(mut cache) = self.entries.lock() {
      cache.remove(&Self::key(resolved));
    }
  }

  /// Drop every cached entry. For an operator whose environment changed
  /// (new DNS mapping, restarted browser) and who should not have to
  /// wait out the TTL.
  pub fn flush(&self) {
    if let Ok(mut cache) = self.entries.lock() {
      cache.clear();
    }
  }
}

/// Run a resolved command to completion under its timeout and shape the
/// output per its declared mode.
///
/// Blocking by design: every caller already runs it on the blocking
/// pool. A separate executor from `ferridriver-script`'s
/// `session_procs` because this crate sits BELOW it in the dependency
/// graph; this one only needs one-shot execution with a bound.
fn execute(resolved: &ResolvedCommand) -> Result<serde_json::Value, String> {
  let mut command = match &resolved.exec {
    ResolvedExec::Shell(line) => {
      let mut c = Command::new("sh");
      c.args(["-c", line]);
      c
    },
    ResolvedExec::Argv(argv) => {
      let (program, rest) = argv.split_first().ok_or("empty argv")?;
      let mut c = Command::new(program);
      c.args(rest);
      c
    },
  };
  if let Some(cwd) = &resolved.cwd {
    command.current_dir(cwd);
  }
  command
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

  let timeout = resolved
    .timeout_ms
    .map_or(DEFAULT_COMMAND_TIMEOUT, Duration::from_millis);

  let mut child = command.spawn().map_err(|e| format!("failed to execute command: {e}"))?;

  // Both pipes are drained on their own threads for the child's whole
  // life. Reading them only AFTER `try_wait` reports exit deadlocks the
  // child the moment it writes more than a pipe buffer (~64KB): it blocks
  // in `write(2)`, never exits, and the loop below kills it at the
  // timeout — reported as "timed out" for a command that was merely
  // chatty. Same failure mode as an undrained browser stderr.
  let stdout_reader = child.stdout.take().map(drain_pipe);
  let stderr_reader = child.stderr.take().map(drain_pipe);

  let started = Instant::now();
  let status = loop {
    match child.try_wait() {
      Err(e) => return Err(format!("failed to wait for command: {e}")),
      Ok(Some(status)) => break status,
      Ok(None) => {
        if started.elapsed() >= timeout {
          let _ = child.kill();
          let _ = child.wait();
          return Err(format!("command timed out after {}ms", timeout.as_millis()));
        }
        std::thread::sleep(Duration::from_millis(20));
      },
    }
  };

  let stdout = stdout_reader.map(join_pipe).unwrap_or_default();
  let stderr = stderr_reader.map(join_pipe).unwrap_or_default();
  if !status.success() {
    return Err(format!("command failed (exit {status}): {}", stderr.trim()));
  }
  shape(&stdout, resolved.output)
}

/// Read one child pipe to EOF on its own thread.
fn drain_pipe<R: std::io::Read + Send + 'static>(mut pipe: R) -> std::thread::JoinHandle<String> {
  std::thread::spawn(move || {
    let mut buf = Vec::new();
    let _ = pipe.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
  })
}

/// Whatever a drain thread collected. A panicked reader yields the empty
/// string rather than poisoning the command's result.
fn join_pipe(handle: std::thread::JoinHandle<String>) -> String {
  handle.join().unwrap_or_default()
}

/// Shape stdout per the declared output mode. `Text` still probes for
/// JSON so the documented `devgate browser args --json` shape works
/// without every config having to declare `output = "json"`.
fn shape(stdout: &str, mode: CommandOutput) -> Result<serde_json::Value, String> {
  let trimmed = stdout.trim();
  match mode {
    CommandOutput::Json => serde_json::from_str(trimmed).map_err(|e| format!("command output is not JSON: {e}")),
    CommandOutput::Lines => Ok(serde_json::Value::Array(
      trimmed
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::Value::String(l.to_string()))
        .collect(),
    )),
    CommandOutput::Text => {
      if (trimmed.starts_with('[') || trimmed.starts_with('{'))
        && let Ok(value) = serde_json::from_str(trimmed)
      {
        return Ok(value);
      }
      Ok(serde_json::Value::String(trimmed.to_string()))
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Tests that need a port to be DEAD must not run while a sibling is
  /// binding `:0` — the kernel readily hands out the port one test just
  /// freed, and the "dead" assertion then fails for an unrelated reason.
  static PORT_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

  fn port_guard() -> std::sync::MutexGuard<'static, ()> {
    PORT_GUARD.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  fn spec(json: &str) -> CommandSpec {
    serde_json::from_str(json).expect("spec")
  }

  fn view<'a>(
    instances: &'a std::collections::HashMap<String, InstanceConfig>,
    args: Option<&'a CommandSpec>,
    discover: Option<&'a CommandSpec>,
    cache: &'a CommandCache,
  ) -> RoutingView<'a> {
    RoutingView {
      instances,
      global: None,
      default_instance: None,
      args_command: args,
      discover_command: discover,
      cache,
      cache_ttl: DEFAULT_CACHE_TTL,
      backend: BackendKind::CdpPipe,
    }
  }

  /// The failure an operator actually hits: a name no instance claims used to
  /// reach the args command, so the error was that command's usage message
  /// instead of the configured set — the one piece of information that answers it.
  #[test]
  fn an_unknown_instance_names_the_configured_ones_instead_of_running_the_command() {
    let mut instances = std::collections::HashMap::new();
    instances.insert("staging".to_string(), InstanceConfig::default());
    instances.insert("dev".to_string(), InstanceConfig::default());
    // Would exit non-zero with its own message if it were ever reached.
    let args = spec(r#""echo boom >&2; exit 2""#);
    let cache = CommandCache::default();
    let v = view(&instances, Some(&args), None, &cache);

    for err in [
      v.health("desktop").expect_err("health must refuse"),
      v.overrides_for("desktop").expect_err("overrides must refuse"),
    ] {
      assert!(err.contains("unknown instance 'desktop'"), "{err}");
      assert!(err.contains("dev, staging"), "must name the configured set: {err}");
      assert!(!err.contains("boom"), "the args command must not have run: {err}");
    }

    // A declared name reaches the command, so it fails as the COMMAND failing —
    // which is what proves the gate did not swallow the declared case too.
    let ran = v.health("staging").expect_err("the failing command still runs");
    assert!(ran.contains("its args command failed"), "{ran}");
    assert!(!ran.contains("unknown instance"), "{ran}");
  }

  /// A launcher that manages the profile, the binary and the proxy should be
  /// able to say so — otherwise the operator hand-copies a managed path into
  /// config, and the copy goes stale the moment the launcher moves it.
  #[test]
  fn an_args_command_supplies_the_whole_launch_shape() {
    let mut instances = std::collections::HashMap::new();
    instances.insert("staging".to_string(), InstanceConfig::default());
    let args = spec(
      r#""printf '{\"args\":[\"--flag\"],\"profileDir\":\"/managed/staging\",\"executablePath\":\"/bin/cft\",\"env\":{\"FROM_CMD\":\"1\"}}'""#,
    );
    let cache = CommandCache::default();
    let v = view(&instances, Some(&args), None, &cache);

    let out = v.overrides_for("staging").expect("overrides");
    assert_eq!(out.args, vec!["--flag".to_string()]);
    assert_eq!(out.user_data_dir.as_deref(), Some("/managed/staging"));
    assert_eq!(out.executable_path.as_deref(), Some("/bin/cft"));
    assert_eq!(out.env.get("FROM_CMD").map(String::as_str), Some("1"));
  }

  /// A value the operator wrote down is never replaced by a derived one.
  #[test]
  fn explicit_config_outranks_the_command_result() {
    let mut instances = std::collections::HashMap::new();
    instances.insert(
      "staging".to_string(),
      InstanceConfig {
        user_data_dir: Some("/chosen".to_string()),
        executable_path: Some("/chosen/chrome".to_string()),
        env: [("FROM_CMD".to_string(), "config".to_string())].into_iter().collect(),
        ..InstanceConfig::default()
      },
    );
    let args = spec(
      r#""printf '{\"args\":[],\"profileDir\":\"/managed\",\"executablePath\":\"/managed/chrome\",\"env\":{\"FROM_CMD\":\"command\"}}'""#,
    );
    let cache = CommandCache::default();
    let v = view(&instances, Some(&args), None, &cache);

    let out = v.overrides_for("staging").expect("overrides");
    assert_eq!(out.user_data_dir.as_deref(), Some("/chosen"));
    assert_eq!(out.executable_path.as_deref(), Some("/chosen/chrome"));
    assert_eq!(out.env.get("FROM_CMD").map(String::as_str), Some("config"));
  }

  /// The schema is ours, so it must accept the shapes we document and refuse
  /// the ones that carry no settings — never silently launch an unconfigured
  /// browser because a command returned something unexpected.
  #[test]
  fn the_launch_spec_schema_accepts_what_we_document() {
    // Legacy shapes stay supported.
    let arr = parse_command_result(&serde_json::json!(["--a", "--b"])).expect("array");
    assert_eq!(arr.args, vec!["--a".to_string(), "--b".to_string()]);
    let text = parse_command_result(&serde_json::json!("--a\n--b\n")).expect("text");
    assert_eq!(text.args, vec!["--a".to_string(), "--b".to_string()]);

    // Both spellings of the profile directory.
    for key in ["userDataDir", "profileDir"] {
      let v = serde_json::json!({ key: "/managed" });
      let spec = parse_command_result(&v).expect(key);
      assert_eq!(spec.user_data_dir.as_deref(), Some("/managed"), "for {key}");
    }

    // A field we do not know must not break a launcher that adds one.
    let fwd = parse_command_result(&serde_json::json!({ "args": [], "environment": "staging", "somethingNew": 7 }))
      .expect("unknown keys are ignored");
    assert!(fwd.args.is_empty());

    // A wrong type is an error naming the key, not a dropped setting.
    let err = parse_command_result(&serde_json::json!({ "args": "not-a-list" })).expect_err("typed");
    assert!(err.contains("args"), "{err}");

    // Shapes carrying no settings at all are refused outright.
    for v in [serde_json::json!(null), serde_json::json!(true), serde_json::json!(42)] {
      assert!(parse_command_result(&v).is_err(), "must refuse {v}");
    }
  }

  /// A proxy the launcher reports has to reach the browser as flags, or the
  /// instance silently bypasses it.
  #[test]
  fn a_reported_proxy_is_lowered_into_launch_flags() {
    let mut out = InstanceOverrides::default();
    let spec = InstanceLaunchSpec {
      proxy_url: Some("http://127.0.0.1:3128".to_string()),
      proxy_bypass: Some("localhost".to_string()),
      ..InstanceLaunchSpec::default()
    };
    apply_command_result(&mut out, spec, BackendKind::CdpPipe);
    assert!(
      out.args.iter().any(|a| a.contains("127.0.0.1:3128")),
      "proxy must reach the browser: {:?}",
      out.args
    );
  }

  /// An instance must be able to describe both halves of its own routing.
  /// Without this, discovery can only address an instance by its session-key
  /// label, which is the coupling `argsCommand` exists to remove.
  #[test]
  fn an_instance_discover_command_replaces_the_section_one() {
    let _guard = port_guard();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();

    let mut instances = std::collections::HashMap::new();
    instances.insert(
      "desktop".to_string(),
      InstanceConfig {
        discover_command: Some(spec(&format!(
          r#""echo ws://127.0.0.1:{port}/devtools/browser/from-instance""#
        ))),
        ..InstanceConfig::default()
      },
    );
    let section = spec(r#""echo ws://127.0.0.1:1/devtools/browser/from-section""#);
    let cache = CommandCache::default();
    let v = view(&instances, None, Some(&section), &cache);

    match v.resolve_connect("desktop") {
      Some(ConnectMode::ConnectUrl(url)) => assert!(url.contains("from-instance"), "{url}"),
      other => panic!("instance command must win outright, got {other:?}"),
    }
  }

  /// An instance names a browser process, and that fact does not change with
  /// who is asking. Declared once at the top level, both `[mcp.browser]` and
  /// `[test.browser]` must resolve it -- and a section that declares the same
  /// name must win, so a host can override one entry without restating the set.
  #[test]
  fn a_top_level_instance_serves_every_section_and_a_section_may_override_it() {
    let mut global = BrowserSectionConfig::default();
    global.instances.insert(
      "staging".to_string(),
      InstanceConfig {
        args: vec!["--from-global".to_string()],
        ..InstanceConfig::default()
      },
    );
    global.instances.insert("prod".to_string(), InstanceConfig::default());

    // This section overrides `staging` and says nothing about `prod`.
    let mut section = std::collections::HashMap::new();
    section.insert(
      "staging".to_string(),
      InstanceConfig {
        args: vec!["--from-section".to_string()],
        ..InstanceConfig::default()
      },
    );

    let cache = CommandCache::default();
    let mut v = view(&section, None, None, &cache);
    v.global = Some(&global);

    assert_eq!(
      v.overrides_for("staging").expect("staging").args,
      vec!["--from-section".to_string()],
      "a section entry must win over the top-level one"
    );
    assert_eq!(
      v.overrides_for("prod").expect("prod").args,
      Vec::<String>::new(),
      "a top-level instance the section never mentions must still resolve"
    );

    // And an unknown name is measured against BOTH layers.
    let err = v.overrides_for("nosuch").expect_err("unknown");
    assert!(err.contains("prod, staging"), "must name every declared layer: {err}");
  }

  #[test]
  fn instance_names_are_validated() {
    assert!(validate_instance_name("staging").is_ok());
    assert!(validate_instance_name("dev-2.eu_west").is_ok());
    assert!(validate_instance_name("").is_err());
    assert!(validate_instance_name(&"x".repeat(65)).is_err());
    // The name is caller-supplied and reaches a shell command.
    assert!(validate_instance_name("staging; rm -rf ~").is_err());
    assert!(validate_instance_name("$(whoami)").is_err());
    assert!(validate_instance_name("a/b").is_err());
  }

  #[test]
  fn shell_form_single_quotes_the_instance_name() {
    // Even if a name slipped past validation, substitution is quoted.
    let s = spec(r#""echo --env ${INSTANCE}""#);
    let r = resolve_for_instance(&s, "sta'ging").expect("resolve");
    assert_eq!(r.exec, ResolvedExec::Shell(r"echo --env 'sta'\''ging'".to_string()));
  }

  #[test]
  fn argv_form_never_involves_a_shell() {
    let s = spec(r#"{"run":["devgate","browser","args","--env","${INSTANCE}"]}"#);
    let r = resolve_for_instance(&s, "staging").expect("resolve");
    assert_eq!(
      r.exec,
      ResolvedExec::Argv(vec![
        "devgate".into(),
        "browser".into(),
        "args".into(),
        "--env".into(),
        "staging".into()
      ])
    );
  }

  #[test]
  fn args_command_json_object_shape_is_accepted() {
    let instances = std::collections::HashMap::new();
    let cache = CommandCache::default();
    let s = spec(r#""printf '{\"environment\":\"staging\",\"args\":[\"--a\",\"--b\"]}'""#);
    let v = view(&instances, Some(&s), None, &cache);
    assert_eq!(v.overrides_for("staging").expect("overrides").args, ["--a", "--b"]);
  }

  #[test]
  fn args_command_line_output_is_accepted() {
    let instances = std::collections::HashMap::new();
    let cache = CommandCache::default();
    let s = spec(r#""echo --one && echo --two""#);
    let v = view(&instances, Some(&s), None, &cache);
    assert_eq!(v.overrides_for("dev").expect("overrides").args, ["--one", "--two"]);
  }

  #[test]
  fn args_command_failure_is_fatal_not_a_warning() {
    let instances = std::collections::HashMap::new();
    let cache = CommandCache::default();
    let s = spec(r#""echo bad env >&2; exit 2""#);
    let v = view(&instances, Some(&s), None, &cache);
    // Silently launching a browser with no environment mapping is how a
    // caller ends up on the wrong environment.
    let err = v.overrides_for("nope").expect_err("must fail");
    assert!(err.contains("exit"), "{err}");
    let health = v.health("nope").expect_err("must fail");
    assert!(health.contains("<env>:<context>"), "{health}");
  }

  #[test]
  fn command_timeout_is_bounded() {
    let instances = std::collections::HashMap::new();
    let cache = CommandCache::default();
    let s = spec(r#"{"run":"sleep 5","timeoutMs":150}"#);
    let v = view(&instances, Some(&s), None, &cache);
    let started = Instant::now();
    let err = v.overrides_for("slow").expect_err("must time out");
    assert!(err.contains("timed out"), "{err}");
    assert!(
      started.elapsed() < Duration::from_secs(2),
      "must not wait out the sleep"
    );
  }

  #[test]
  fn per_instance_settings_are_resolved() {
    let mut instances = std::collections::HashMap::new();
    instances.insert(
      "staging".to_string(),
      InstanceConfig {
        args: vec!["--static".into()],
        user_data_dir: Some("/profiles/${INSTANCE}".into()),
        executable_path: Some("/bin/chrome".into()),
        headless: Some(true),
        env: BTreeMap::from([("APP_ENV".to_string(), "staging".to_string())]),
        proxy: Some(ProxyConfig {
          server: "http://localhost:3003".into(),
          bypass: Some("localhost".into()),
          ..Default::default()
        }),
        ignore_default_args: Some(IgnoreDefaultArgsConfig::Only(vec!["--no-sandbox".into()])),
        ..Default::default()
      },
    );
    let cache = CommandCache::default();
    let v = view(&instances, None, None, &cache);
    let o = v.overrides_for("staging").expect("overrides");

    assert_eq!(o.user_data_dir.as_deref(), Some("/profiles/staging"));
    assert_eq!(o.executable_path.as_deref(), Some("/bin/chrome"));
    assert_eq!(o.headless, Some(true));
    assert_eq!(o.env.get("APP_ENV").map(String::as_str), Some("staging"));
    assert!(o.args.contains(&"--static".to_string()));
    assert!(o.args.contains(&"--proxy-server=http://localhost:3003".to_string()));
    assert!(o.args.contains(&"--proxy-bypass-list=localhost".to_string()));
    assert_eq!(
      o.ignore_default_args,
      Some(ferridriver::options::IgnoreDefaultArgs::Some(vec![
        "--no-sandbox".into()
      ])),
      "ignoreDefaultArgs must reach the launch path"
    );
  }

  #[test]
  fn an_instance_args_command_replaces_the_section_one() {
    let cache = CommandCache::default();
    let mut instances = std::collections::HashMap::new();
    instances.insert(
      "webkit".to_string(),
      InstanceConfig {
        args_command: Some(spec(r#""echo --from-instance""#)),
        ..Default::default()
      },
    );

    // Declared, but with no command of its own: it falls back to the section's.
    instances.insert("chrome".to_string(), InstanceConfig::default());

    let section = spec(r#""echo --from-section""#);
    let view = view(&instances, Some(&section), None, &cache);
    let out = view.overrides_for("webkit").expect("overrides");

    assert_eq!(
      out.args,
      vec!["--from-instance".to_string()],
      "instance command must win outright"
    );

    let other = view.overrides_for("chrome").expect("overrides");
    assert_eq!(other.args, vec!["--from-section".to_string()]);
  }

  #[test]
  fn a_webkit_instance_is_never_discovered() {
    let cache = CommandCache::default();
    let mut instances = std::collections::HashMap::new();
    instances.insert(
      "webkit".to_string(),
      InstanceConfig {
        backend: Some(BackendChoice::WebKit),
        ..Default::default()
      },
    );

    // The command leaves a trace, so the test observes whether it RAN rather
    // than whether its endpoint was accepted (a dead one is rejected either way).
    let tmp = tempfile::tempdir().expect("tempdir");
    let marker = tmp.path().join("${INSTANCE}.ran");
    let discover = spec(&format!(
      r#""touch {} && echo ws://127.0.0.1:9222/devtools/browser/x""#,
      marker.display()
    ));
    let view = view(&instances, None, Some(&discover), &cache);

    let _ = view.resolve_connect("webkit");
    assert!(
      !tmp.path().join("webkit.ran").exists(),
      "a WebKit browser exposes no endpoint to attach to, so discovery must not run"
    );

    let _ = view.resolve_connect("chrome");
    assert!(
      tmp.path().join("chrome.ran").exists(),
      "the section's discover command still serves CDP instances"
    );
  }

  #[test]
  fn webkit_takes_its_own_proxy_switch() {
    let proxy = ProxyConfig {
      server: "http://127.0.0.1:3052".into(),
      bypass: Some("127.0.0.1,localhost".into()),
      username: None,
      password: None,
    };

    let chromium = proxy_args(&proxy, BackendKind::CdpPipe).expect("chromium flags");
    assert!(chromium.contains(&"--proxy-server=http://127.0.0.1:3052".to_string()));

    // `--proxy-server` is not a WebKit switch: it is accepted and ignored, so
    // lowering it the Chromium way left WebKit talking to the network direct.
    let webkit = proxy_args(&proxy, BackendKind::WebKit).expect("webkit flags");
    assert!(webkit.contains(&"--proxy=http://127.0.0.1:3052".to_string()));
    assert!(!webkit.iter().any(|arg| arg.starts_with("--proxy-server=")));

    if cfg!(target_os = "linux") {
      assert!(webkit.contains(&"--ignore-host=127.0.0.1".to_string()));
      assert!(webkit.contains(&"--ignore-host=localhost".to_string()));
    } else {
      assert!(webkit.contains(&"--proxy-bypass-list=127.0.0.1,localhost".to_string()));
    }
  }

  #[test]
  fn an_instance_backend_overrides_the_sections_for_proxy_flags() {
    let cfg = InstanceConfig {
      backend: Some(BackendChoice::WebKit),
      proxy: Some(ProxyConfig {
        server: "http://127.0.0.1:3052".into(),
        bypass: None,
        username: None,
        password: None,
      }),
      ..Default::default()
    };

    let overrides = instance_overrides_from(&cfg, "staging", BackendKind::CdpPipe).expect("overrides");
    assert!(overrides.args.contains(&"--proxy=http://127.0.0.1:3052".to_string()));
  }

  #[test]
  fn proxy_credentials_are_rejected_not_dropped() {
    let mut instances = std::collections::HashMap::new();
    instances.insert(
      "p".to_string(),
      InstanceConfig {
        proxy: Some(ProxyConfig {
          server: "http://p:1".into(),
          username: Some("u".into()),
          ..Default::default()
        }),
        ..Default::default()
      },
    );
    let cache = CommandCache::default();
    let v = view(&instances, None, None, &cache);
    let err = v.overrides_for("p").expect_err("must fail");
    assert!(err.contains("credentials"), "{err}");
  }

  #[test]
  fn connect_url_wins_over_discovery() {
    let mut instances = std::collections::HashMap::new();
    instances.insert(
      "remote".to_string(),
      InstanceConfig {
        connect_url: Some("ws://192.168.1.50:9222/devtools/browser/abc".into()),
        ..Default::default()
      },
    );
    let cache = CommandCache::default();
    let discover = spec(r#""echo ws://127.0.0.1:1/x""#);
    let v = view(&instances, None, Some(&discover), &cache);
    assert!(matches!(
      v.resolve_connect("remote"),
      Some(ConnectMode::ConnectUrl(url)) if url.contains("192.168.1.50")
    ));
  }

  #[test]
  fn stale_profile_falls_through_to_the_discover_command() {
    let _net = port_guard();
    let dir = tempfile::tempdir().expect("tempdir");
    // A port nothing is listening on: the profile is stale.
    let dead = {
      let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
      l.local_addr().expect("addr").port()
    };
    std::fs::write(
      dir.path().join("DevToolsActivePort"),
      format!("{dead}\n/devtools/browser/gone"),
    )
    .expect("write");

    let live = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let live_port = live.local_addr().expect("addr").port();

    let mut instances = std::collections::HashMap::new();
    instances.insert(
      "staging".to_string(),
      InstanceConfig {
        discover_profile: Some(dir.path().to_string_lossy().into_owned()),
        ..Default::default()
      },
    );
    let cache = CommandCache::default();
    let discover = spec(&format!(r#""echo ws://127.0.0.1:{live_port}/devtools/browser/new""#));
    let v = view(&instances, None, Some(&discover), &cache);

    // The old implementation returned None here and never ran the
    // discover command, so a restarted browser was unreachable.
    assert!(matches!(
      v.resolve_connect("staging"),
      Some(ConnectMode::ConnectUrl(url)) if url.contains(&live_port.to_string())
    ));
  }

  #[test]
  fn dead_discovered_endpoint_is_rejected_and_evicted() {
    let _net = port_guard();
    let dead = {
      let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
      l.local_addr().expect("addr").port()
    };
    let instances = std::collections::HashMap::new();
    let cache = CommandCache::default();
    let discover = spec(&format!(r#""echo ws://127.0.0.1:{dead}/x""#));
    let v = view(&instances, None, Some(&discover), &cache);
    assert!(v.resolve_connect("any").is_none());
    // Evicted, so a browser that comes up later is found rather than
    // being masked for the whole TTL.
    let resolved = resolve_for_instance(&discover, "any").expect("resolve");
    assert!(
      cache.get_or_exec(&resolved, Duration::from_secs(0)).is_ok(),
      "entry must not be a cached failure"
    );
  }

  /// A command writing more than a pipe buffer used to block in
  /// `write(2)` forever and be killed at the timeout, so a chatty
  /// discover/args command looked like a hung one.
  #[test]
  fn output_larger_than_a_pipe_buffer_does_not_time_out() {
    let instances = std::collections::HashMap::new();
    let cache = CommandCache::default();
    // ~256KB of args, four times the usual 64KB pipe buffer.
    let s = spec(r#"{"run":"for i in $(seq 1 16000); do echo --flag-$i; done","timeoutMs":20000}"#);
    let v = view(&instances, Some(&s), None, &cache);
    let args = v.overrides_for("chatty").expect("must not time out").args;
    assert_eq!(args.len(), 16000, "every line survives");
    assert_eq!(args[15999], "--flag-16000");
  }

  /// The same command line from two directories is two commands: a
  /// repo-relative helper resolves differently in each.
  #[test]
  fn cache_entries_are_scoped_to_the_command_cwd() {
    let a = tempfile::tempdir().expect("tempdir");
    let b = tempfile::tempdir().expect("tempdir");
    std::fs::write(a.path().join("marker"), "A").expect("write");
    std::fs::write(b.path().join("marker"), "B").expect("write");

    let cache = CommandCache::default();
    let make = |dir: &Path| {
      let json = format!(r#"{{"run":"cat marker","cwd":"{}"}}"#, dir.display());
      resolve_for_instance(&spec(&json), "x").expect("resolve")
    };
    let first = cache.get_or_exec(&make(a.path()), DEFAULT_CACHE_TTL).expect("a");
    let second = cache.get_or_exec(&make(b.path()), DEFAULT_CACHE_TTL).expect("b");
    assert_eq!(first, serde_json::Value::String("A".into()));
    assert_eq!(second, serde_json::Value::String("B".into()), "cwd is part of identity");
  }

  /// Concurrent cold callers must not each spawn the command: a discover
  /// command that polls would cost one full poll per caller.
  #[test]
  fn concurrent_misses_run_the_command_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let counter = dir.path().join("runs");
    let cache = Arc::new(CommandCache::default());
    let s = spec(&format!(
      r#""echo x >> {0}; wc -l < {0}""#,
      counter.display().to_string().replace('"', "")
    ));
    let resolved = resolve_for_instance(&s, "x").expect("resolve");

    std::thread::scope(|scope| {
      for _ in 0..8 {
        let cache = Arc::clone(&cache);
        let resolved = resolved.clone();
        scope.spawn(move || {
          cache.get_or_exec(&resolved, DEFAULT_CACHE_TTL).expect("exec");
        });
      }
    });

    let runs = std::fs::read_to_string(&counter).expect("counter").lines().count();
    assert_eq!(runs, 1, "eight concurrent misses, one execution");
  }

  #[test]
  fn cache_serves_within_ttl_and_flush_clears_it() {
    let cache = CommandCache::default();
    let s = spec(r#""date +%s%N""#);
    let resolved = resolve_for_instance(&s, "x").expect("resolve");
    let first = cache.get_or_exec(&resolved, DEFAULT_CACHE_TTL).expect("first");
    let second = cache.get_or_exec(&resolved, DEFAULT_CACHE_TTL).expect("second");
    assert_eq!(first, second, "served from cache");
    cache.flush();
    let third = cache.get_or_exec(&resolved, DEFAULT_CACHE_TTL).expect("third");
    assert_ne!(first, third, "flush must force a re-run");
  }

  #[test]
  fn discover_command_output_shapes() {
    assert_eq!(
      first_ws_url(&serde_json::json!("ws://a:1/x")).as_deref(),
      Some("ws://a:1/x")
    );
    assert_eq!(
      first_ws_url(&serde_json::json!({"wsEndpoint": "wss://b:2/y"})).as_deref(),
      Some("wss://b:2/y")
    );
    assert_eq!(
      first_ws_url(&serde_json::json!(["http://c", "ws://d:3/z"])).as_deref(),
      Some("ws://d:3/z")
    );
    // A snake_case key is what a Rust/Go discover command emits by default;
    // missing it made a found browser look like no browser.
    assert_eq!(
      first_ws_url(&serde_json::json!({"env": "staging", "ws_url": "ws://e:4/w", "port": 4})).as_deref(),
      Some("ws://e:4/w")
    );
    assert_eq!(
      first_ws_url(&serde_json::json!({"wsUrl": "ws://f:5/v"})).as_deref(),
      Some("ws://f:5/v")
    );
    assert!(first_ws_url(&serde_json::json!("not-a-url")).is_none());
    assert!(
      first_ws_url(&serde_json::json!({"port": 9222})).is_none(),
      "a payload with no ws URL is still a miss"
    );
  }
}

//! External instance command execution, output shaping, TTL caching, and endpoint discovery.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferridriver::backend::BackendKind;
use ferridriver::options::InstanceOverrides;
use ferridriver::state::ConnectMode;

use crate::command_spec::{CommandOutput, ResolvedCommand, ResolvedExec};

use super::{DEFAULT_COMMAND_TIMEOUT, DISCOVER_TCP_TIMEOUT, expand_instance_path, proxy_launch_flags};

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
pub(super) fn parse_command_result(value: &serde_json::Value) -> Result<InstanceLaunchSpec, String> {
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
pub(super) fn apply_command_result(out: &mut InstanceOverrides, spec: InstanceLaunchSpec, backend: BackendKind) {
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
pub(super) fn first_ws_url(value: &serde_json::Value) -> Option<String> {
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
pub(super) fn discover_from_profile(template: &str, instance: &str) -> Option<ConnectMode> {
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
  entries: Mutex<rustc_hash::FxHashMap<String, CacheEntry>>,
  /// One lock per key, so concurrent misses run the command ONCE.
  ///
  /// Without it every cold session spawned its own copy: a discover
  /// command that polls (`--wait 10`) then cost N × 10s of wall clock and
  /// N browser probes to answer a question with one answer.
  inflight: Mutex<rustc_hash::FxHashMap<String, Arc<Mutex<()>>>>,
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
    ResolvedExec::Shell(line) => ferridriver::backend::process::shell_command(line),
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

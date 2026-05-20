//! Process execution for the `commands` capability.
//!
//! One-shot: [`run_oneshot`] spawns, bounds wall-clock and output, kills
//! the whole process group on timeout, and shapes stdout per the
//! declared [`CommandOutput`] mode.
//!
//! Persistent: [`SessionProcs`] keeps long-running children (a dev
//! server, a watcher) alive across VM rebuilds. It lives in the durable
//! session tier, so `Drop` (idle-TTL reap / explicit close / shutdown)
//! SIGKILLs every process group — a session can never leak a server.
//!
//! Every child is its own process group (`setsid` in `pre_exec`) so a
//! shell pipeline dies whole, not just its leader. The environment is
//! scrubbed to `PATH` plus the spec's declared passthrough names — a
//! command never inherits ambient server secrets.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::command_spec::{CommandOutput, ResolvedCommand, ResolvedExec};

/// Default hard wall-clock bound for a one-shot command that did not
/// declare `timeoutMs`. Without this a hung child (blocked on a
/// resource, an infinite loop) would pin the calling script forever —
/// the per-script interrupt-handler timeout does not fire during this
/// native await. A spec's explicit `timeoutMs` still overrides.
const DEFAULT_ONESHOT_TIMEOUT_MS: u64 = 120_000;

/// Max bytes captured per stream (one-shot result, or the tail kept for
/// a persistent process's `status`).
const OUTPUT_CAP: usize = 8 * 1024 * 1024;
const RING_CAP: usize = 64 * 1024;
/// Max concurrently-running persistent processes per session.
const MAX_PERSISTENT: usize = 16;

fn configure(cmd: &mut Command, rc: &ResolvedCommand) {
  cmd.env_clear();
  if let Some(path) = std::env::var_os("PATH") {
    cmd.env("PATH", path);
  }
  for name in &rc.env {
    if let Some(val) = std::env::var_os(name) {
      cmd.env(name, val);
    }
  }
  if let Some(dir) = &rc.cwd {
    cmd.current_dir(dir);
  }
  cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
  // New session => child is its own process-group leader (pgid == pid),
  // so `kill(-pid)` reaps the whole pipeline. SAFETY: `setsid` is
  // async-signal-safe and the only call in the pre_exec hook.
  #[allow(unsafe_code)]
  unsafe {
    cmd.pre_exec(|| {
      libc::setsid();
      Ok(())
    });
  }
}

fn build(rc: &ResolvedCommand) -> Command {
  let mut cmd = match &rc.exec {
    ResolvedExec::Shell(line) => {
      let mut c = Command::new("sh");
      c.arg("-c").arg(line);
      c
    },
    ResolvedExec::Argv(argv) => {
      // Argv is non-empty (deserialization enforces it); be defensive.
      let mut c = Command::new(argv.first().map_or("true", String::as_str));
      c.args(argv.iter().skip(1));
      c
    },
  };
  configure(&mut cmd, rc);
  cmd
}

fn pid_of(id: Option<u32>) -> i32 {
  id.and_then(|p| i32::try_from(p).ok()).unwrap_or(0)
}

/// SIGKILL the process group led by `pid` (best-effort).
fn kill_group(pid: i32) {
  if pid > 0 {
    #[allow(unsafe_code)]
    unsafe {
      libc::kill(-pid, libc::SIGKILL);
    }
  }
}

/// Read up to `cap` bytes; `Err` if the stream exceeds it (the process
/// group is killed by the caller).
async fn read_capped<R: tokio::io::AsyncRead + Unpin>(mut r: R, cap: usize) -> Result<Vec<u8>, String> {
  let mut buf = Vec::new();
  let mut chunk = [0u8; 8192];
  loop {
    let n = r
      .read(&mut chunk)
      .await
      .map_err(|e| format!("read child output: {e}"))?;
    if n == 0 {
      break;
    }
    if buf.len() + n > cap {
      return Err(format!("command output exceeded {cap} bytes"));
    }
    buf.extend_from_slice(&chunk[..n]);
  }
  Ok(buf)
}

fn shape(stdout: &[u8], mode: CommandOutput) -> Result<serde_json::Value, String> {
  let s = String::from_utf8_lossy(stdout);
  let t = s.trim();
  match mode {
    CommandOutput::Text => Ok(if t.is_empty() {
      serde_json::Value::Null
    } else {
      serde_json::Value::String(t.to_string())
    }),
    CommandOutput::Json => {
      if t.is_empty() {
        return Ok(serde_json::Value::Null);
      }
      serde_json::from_str(t).map_err(|e| format!("command output is not valid JSON: {e}"))
    },
    CommandOutput::Lines => Ok(serde_json::Value::Array(
      t.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::Value::String(l.to_string()))
        .collect(),
    )),
  }
}

/// Run a one-shot command to completion. Errors on non-zero exit
/// (message carries stderr), timeout, or output past the cap.
pub async fn run_oneshot(rc: &ResolvedCommand) -> Result<serde_json::Value, String> {
  if rc.persistent {
    return Err("this command is declared `persistent`: use commands.start/status/stop, not run".to_string());
  }
  let mut child = build(rc).spawn().map_err(|e| format!("spawn command: {e}"))?;
  let pid = pid_of(child.id());
  let out = child.stdout.take().ok_or("no stdout pipe")?;
  let err = child.stderr.take().ok_or("no stderr pipe")?;

  let work = Box::pin(async move {
    let (o, e) = tokio::join!(read_capped(out, OUTPUT_CAP), read_capped(err, OUTPUT_CAP));
    let status = child.wait().await.map_err(|e| format!("wait child: {e}"))?;
    Ok::<_, String>((o?, e?, status))
  });

  // An explicit `timeoutMs` is honoured as-is; an unset one still gets
  // a hard default so a hung one-shot can never block the session
  // indefinitely.
  let ms = rc.timeout_ms.unwrap_or(DEFAULT_ONESHOT_TIMEOUT_MS);
  let (stdout, stderr, status) = {
    let Ok(r) = tokio::time::timeout(Duration::from_millis(ms), work).await else {
      kill_group(pid);
      return Err(format!("command timed out after {ms}ms"));
    };
    r.inspect_err(|_| kill_group(pid))?
  };

  if !status.success() {
    let code = status.code().map_or_else(|| "signal".to_string(), |c| c.to_string());
    let msg = String::from_utf8_lossy(&stderr);
    return Err(format!("command failed (exit {code}): {}", msg.trim()));
  }
  shape(&stdout, rc.output)
}

/// A bounded tail of a stream — only the last [`RING_CAP`] bytes.
#[derive(Default)]
struct Ring(Vec<u8>);
impl Ring {
  fn push(&mut self, b: &[u8]) {
    self.0.extend_from_slice(b);
    if self.0.len() > RING_CAP {
      let cut = self.0.len() - RING_CAP;
      self.0.drain(..cut);
    }
  }
  fn text(&self) -> String {
    String::from_utf8_lossy(&self.0).into_owned()
  }
}

struct Proc {
  pid: i32,
  started: Instant,
  stdout: Arc<Mutex<Ring>>,
  stderr: Arc<Mutex<Ring>>,
  /// Set by the reaper task once the child exits.
  exit: Arc<Mutex<Option<i32>>>,
}

/// Per-session persistent-process registry. Owned by the durable
/// session tier; `Drop` kills every process group.
pub struct SessionProcs {
  inner: Mutex<HashMap<String, Proc>>,
}

impl Default for SessionProcs {
  fn default() -> Self {
    Self {
      inner: Mutex::new(HashMap::new()),
    }
  }
}

impl SessionProcs {
  /// Start (or no-op if already running) a persistent command. Returns
  /// the pid.
  pub fn start(&self, name: &str, rc: &ResolvedCommand) -> Result<i32, String> {
    if !rc.persistent {
      return Err("this command is not declared `persistent`: use commands.run".to_string());
    }
    let mut map = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(p) = map.get(name)
      && p
        .exit
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_none()
    {
      return Ok(p.pid); // already running — idempotent
    }
    map.retain(|_, p| {
      let alive = p
        .exit
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_none();
      if !alive {
        kill_group(p.pid);
      }
      alive
    });
    if map.len() >= MAX_PERSISTENT {
      return Err(format!(
        "too many persistent processes (max {MAX_PERSISTENT}) for this session"
      ));
    }

    let mut child = build(rc).spawn().map_err(|e| format!("spawn command: {e}"))?;
    let pid = pid_of(child.id());
    let stdout = Arc::new(Mutex::new(Ring::default()));
    let stderr = Arc::new(Mutex::new(Ring::default()));
    let exit = Arc::new(Mutex::new(None));

    if let Some(o) = child.stdout.take() {
      pump(o, stdout.clone());
    }
    if let Some(e) = child.stderr.take() {
      pump(e, stderr.clone());
    }
    let exit_w = exit.clone();
    tokio::spawn(async move {
      let code = child.wait().await.ok().and_then(|s| s.code()).unwrap_or(-1);
      *exit_w.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(code);
    });

    map.insert(
      name.to_string(),
      Proc {
        pid,
        started: Instant::now(),
        stdout,
        stderr,
        exit,
      },
    );
    Ok(pid)
  }

  pub fn status(&self, name: &str) -> Result<serde_json::Value, String> {
    let map = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let p = map
      .get(name)
      .ok_or_else(|| format!("no persistent process `{name}` started in this session"))?;
    let exit = *p.exit.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    Ok(serde_json::json!({
      "name": name,
      "pid": p.pid,
      "running": exit.is_none(),
      "exitCode": exit,
      "uptimeMs": p.started.elapsed().as_millis() as u64,
      "stdout": p.stdout.lock().unwrap_or_else(std::sync::PoisonError::into_inner).text(),
      "stderr": p.stderr.lock().unwrap_or_else(std::sync::PoisonError::into_inner).text(),
    }))
  }

  pub fn stop(&self, name: &str) -> Result<(), String> {
    let mut map = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match map.remove(name) {
      Some(p) => {
        kill_group(p.pid);
        Ok(())
      },
      None => Err(format!("no persistent process `{name}` to stop")),
    }
  }
}

impl Drop for SessionProcs {
  fn drop(&mut self) {
    if let Ok(map) = self.inner.lock() {
      for p in map.values() {
        kill_group(p.pid);
      }
    }
  }
}

fn pump<R: tokio::io::AsyncRead + Unpin + Send + 'static>(mut r: R, ring: Arc<Mutex<Ring>>) {
  tokio::spawn(async move {
    let mut chunk = [0u8; 8192];
    loop {
      match r.read(&mut chunk).await {
        Ok(0) | Err(_) => break,
        Ok(n) => ring
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .push(&chunk[..n]),
      }
    }
  });
}

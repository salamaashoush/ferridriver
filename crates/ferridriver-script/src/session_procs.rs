//! Process execution for the `commands` capability.
//!
//! One-shot: [`run_oneshot`] spawns, bounds wall-clock and output, kills
//! the whole process group on timeout, and shapes stdout per the
//! declared [`CommandOutput`] mode.
//!
//! Persistent: [`SessionProcs`] keeps long-running children (a dev
//! server, a watcher) alive across VM rebuilds. It lives in the durable
//! session tier. Explicit close awaits its process owners; dropping the
//! registry requests cleanup through those same owners.
//!
//! Every child is its own process group so a
//! shell pipeline dies whole, not just its leader. The environment is
//! scrubbed to `PATH` plus the spec's declared passthrough names — a
//! command never inherits ambient server secrets.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ferridriver::backend::process::ChildGroup;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
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
  cmd
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
  #[cfg(unix)]
  cmd.process_group(0);
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

async fn finished<T>(done: &AtomicBool, leg: impl std::future::Future<Output = T>) -> T {
  let output = leg.await;
  done.store(true, Ordering::Relaxed);
  output
}

#[cfg(target_os = "linux")]
fn running_detail(pid: Option<u32>) -> String {
  pid
    .and_then(ferridriver::backend::process::describe_process)
    .map(|detail| format!(": {detail}"))
    .unwrap_or_default()
}

#[cfg(not(target_os = "linux"))]
fn running_detail(_pid: Option<u32>) -> String {
  String::new()
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
  SessionProcs::default().run_oneshot(rc).await
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
  pub exit_code: Option<i32>,
  pub success: bool,
  pub stdout: String,
  pub stderr: String,
}

pub async fn exec_oneshot(rc: &ResolvedCommand) -> Result<CommandResult, String> {
  SessionProcs::default().exec_oneshot(rc).await
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

struct InteractiveOutput {
  reader: tokio::io::BufReader<tokio::process::ChildStdout>,
  pending: Vec<u8>,
}

struct Proc {
  pid: i32,
  input: Arc<tokio::sync::Mutex<Option<tokio::process::ChildStdin>>>,
  output: Option<Arc<tokio::sync::Mutex<InteractiveOutput>>>,
  started: Instant,
  stdout: Arc<Mutex<Ring>>,
  output_changed: tokio::sync::watch::Receiver<()>,
  stderr: Arc<Mutex<Ring>>,
  /// Set by the reaper task once the child exits.
  exit: tokio::sync::watch::Receiver<Option<i32>>,
  failure: tokio::sync::watch::Receiver<Option<String>>,
  control: tokio::sync::mpsc::UnboundedSender<StopRequest>,
}

type StopRequest = tokio::sync::oneshot::Sender<Result<(), String>>;

#[derive(Clone)]
struct Job {
  control: tokio::sync::mpsc::UnboundedSender<StopRequest>,
  exit: tokio::sync::watch::Receiver<Option<i32>>,
  failure: tokio::sync::watch::Receiver<Option<String>>,
}

struct StopOnDrop(Option<tokio::sync::mpsc::UnboundedSender<StopRequest>>);

impl Drop for StopOnDrop {
  fn drop(&mut self) {
    if let Some(control) = self.0.take() {
      let (reply, _) = tokio::sync::oneshot::channel();
      let _ = control.send(reply);
    }
  }
}

async fn wait_process(
  mut exit: tokio::sync::watch::Receiver<Option<i32>>,
  mut failure: tokio::sync::watch::Receiver<Option<String>>,
) -> Result<i32, String> {
  loop {
    if let Some(code) = *exit.borrow_and_update() {
      return Ok(code);
    }
    if let Some(error) = failure.borrow_and_update().as_ref() {
      return Err(error.clone());
    }
    tokio::select! {
      result = exit.changed() => result.map_err(|e| e.to_string())?,
      result = failure.changed() => {
        if result.is_err() && exit.borrow().is_none() {
          return Err("process owner stopped without an exit result".into());
        }
      },
    }
  }
}

async fn stop_process(
  control: &tokio::sync::mpsc::UnboundedSender<StopRequest>,
  exit: &tokio::sync::watch::Receiver<Option<i32>>,
) -> Result<(), String> {
  if exit.borrow().is_some() {
    return Ok(());
  }
  let (reply, result) = tokio::sync::oneshot::channel();
  if control.send(reply).is_ok()
    && let Ok(result) = result.await
  {
    return result;
  }
  if exit.borrow().is_some() {
    Ok(())
  } else {
    Err("process owner stopped before confirming cleanup".into())
  }
}

async fn own_process(
  mut group: ChildGroup,
  mut control: tokio::sync::mpsc::UnboundedReceiver<StopRequest>,
  pumps: Vec<tokio::task::JoinHandle<()>>,
  exit: tokio::sync::watch::Sender<Option<i32>>,
  failure: tokio::sync::watch::Sender<Option<String>>,
) {
  let mut observing = true;
  loop {
    let (result, reply, abandoned) = tokio::select! {
      result = group.wait(), if observing => (result, None, false),
      request = control.recv() => {
        let abandoned = request.is_none();
        let result = match group.terminate() {
          Ok(()) => group.wait().await,
          Err(error) => Err(error),
        };
        (result, request, abandoned)
      }
    };
    match result {
      Ok(status) => {
        for pump in pumps {
          if let Err(error) = pump.await {
            tracing::warn!(%error, "command output task failed");
          }
        }
        exit.send_replace(Some(status.code().unwrap_or(-1)));
        if let Some(reply) = reply {
          let _ = reply.send(Ok(()));
        }
        return;
      },
      Err(error) => {
        let message = format!("process cleanup: {error}");
        failure.send_replace(Some(message.clone()));
        if let Some(reply) = reply {
          let _ = reply.send(Err(message));
        }
        if abandoned {
          tracing::warn!(%error, "process cleanup failed after owner was dropped");
          return;
        }
        observing = false;
      },
    }
  }
}

/// Per-session process registry, retained independently of the script VM.
pub struct SessionProcs {
  inner: Mutex<HashMap<String, Proc>>,
  closing: std::sync::atomic::AtomicBool,
  jobs: Mutex<Vec<Job>>,
}

impl Default for SessionProcs {
  fn default() -> Self {
    Self {
      inner: Mutex::new(HashMap::new()),
      closing: std::sync::atomic::AtomicBool::new(false),
      jobs: Mutex::new(Vec::new()),
    }
  }
}

impl SessionProcs {
  pub async fn run_oneshot(&self, rc: &ResolvedCommand) -> Result<serde_json::Value, String> {
    let result = self.exec_oneshot(rc).await?;
    if !result.success {
      let code = result.exit_code.map_or_else(|| "signal".to_string(), |c| c.to_string());
      return Err(format!("command failed (exit {code}): {}", result.stderr.trim()));
    }
    shape(result.stdout.as_bytes(), rc.output)
  }

  pub async fn exec_oneshot(&self, rc: &ResolvedCommand) -> Result<CommandResult, String> {
    if rc.persistent {
      return Err("this command is declared `persistent`: use commands.start/status/stop, not run".into());
    }
    let (job, out, err, pid) = {
      let mut jobs = self.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      if self.closing.load(std::sync::atomic::Ordering::Acquire) {
        return Err("session processes are closing".into());
      }
      jobs.retain(|job| job.exit.borrow().is_none());
      let mut child = build(rc).spawn().map_err(|e| format!("spawn command: {e}"))?;
      let pid = child.id();
      let out = child.stdout.take();
      let err = child.stderr.take();
      let group = ChildGroup::unregistered(child);
      let out = out.ok_or("no stdout pipe")?;
      let err = err.ok_or("no stderr pipe")?;
      let (control, requests) = tokio::sync::mpsc::unbounded_channel();
      let (exit_w, exit) = tokio::sync::watch::channel(None);
      let (failure_w, failure) = tokio::sync::watch::channel(None);
      let job = Job { control, exit, failure };
      jobs.push(job.clone());
      tokio::spawn(own_process(group, requests, Vec::new(), exit_w, failure_w));
      (job, out, err, pid)
    };
    let mut cancel = StopOnDrop(Some(job.control.clone()));
    let legs = [
      (AtomicBool::new(false), "the process to exit"),
      (AtomicBool::new(false), "stdout to close"),
      (AtomicBool::new(false), "stderr to close"),
    ];
    let work = Box::pin(async {
      tokio::try_join!(
        finished(&legs[0].0, wait_process(job.exit.clone(), job.failure.clone())),
        finished(&legs[1].0, read_capped(out, OUTPUT_CAP)),
        finished(&legs[2].0, read_capped(err, OUTPUT_CAP)),
      )
    });
    let ms = rc.timeout_ms.unwrap_or(DEFAULT_ONESHOT_TIMEOUT_MS);
    let result = tokio::time::timeout(Duration::from_millis(ms), work)
      .await
      .unwrap_or_else(|_| {
        let pending: Vec<&str> = legs
          .iter()
          .filter(|(done, _)| !done.load(Ordering::Relaxed))
          .map(|(_, leg)| *leg)
          .collect();
        // An exit that happened but was never observed is a different bug
        // from a command that is still running; say which one this was.
        let exited = if legs[0].0.load(Ordering::Relaxed) {
          String::new()
        } else {
          match pid.map(ferridriver::backend::process::child_exited) {
            Some(Ok(true)) => " (the process had exited, but its owner never saw it)".to_string(),
            Some(Ok(false)) => format!(" (the process is still running{})", running_detail(pid)),
            _ => String::new(),
          }
        };
        Err(format!(
          "command timed out after {ms}ms waiting for {}{exited}",
          pending.join(", ")
        ))
      });
    let cleanup = stop_process(&job.control, &job.exit).await;
    cancel.0 = None;
    if cleanup.is_ok() {
      self
        .jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|entry| !entry.control.same_channel(&job.control));
    }
    let (code, stdout, stderr) = match (result, cleanup) {
      (Ok(result), Ok(())) => result,
      (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
      (Err(error), Err(cleanup)) => return Err(format!("{error}; cleanup: {cleanup}")),
    };
    Ok(CommandResult {
      exit_code: (code >= 0).then_some(code),
      success: code == 0,
      stdout: String::from_utf8_lossy(&stdout).into_owned(),
      stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
  }

  /// Start (or no-op if already running) a persistent command. Returns
  /// the pid.
  pub fn start(&self, name: &str, rc: &ResolvedCommand) -> Result<i32, String> {
    self.spawn(name, rc, false)
  }

  pub fn open(&self, name: &str, rc: &ResolvedCommand) -> Result<i32, String> {
    self.spawn(name, rc, true)
  }

  fn spawn(&self, name: &str, rc: &ResolvedCommand, interactive: bool) -> Result<i32, String> {
    if !rc.persistent {
      return Err("this command is not declared `persistent`: use commands.run".to_string());
    }
    let mut map = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if self.closing.load(std::sync::atomic::Ordering::Acquire) {
      return Err("session processes are closing".into());
    }
    if let Some(p) = map.get(name)
      && p.exit.borrow().is_none()
    {
      if let Some(error) = p.failure.borrow().as_ref() {
        return Err(error.clone());
      }
      return Ok(p.pid); // already running — idempotent
    }
    map.retain(|_, p| p.exit.borrow().is_none());
    if map.len() >= MAX_PERSISTENT {
      return Err(format!(
        "too many persistent processes (max {MAX_PERSISTENT}) for this session"
      ));
    }

    let mut cmd = build(rc);
    if interactive {
      cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().map_err(|e| format!("spawn command: {e}"))?;
    let pid = pid_of(child.id());
    let stdout = Arc::new(Mutex::new(Ring::default()));
    let stderr = Arc::new(Mutex::new(Ring::default()));
    let (exit_w, exit) = tokio::sync::watch::channel(None);
    let (failure_w, failure) = tokio::sync::watch::channel(None);
    let (control, requests) = tokio::sync::mpsc::unbounded_channel();
    let (output_w, output_changed) = tokio::sync::watch::channel(());

    let input = Arc::new(tokio::sync::Mutex::new(child.stdin.take()));
    let pipe = child.stdout.take().ok_or("no stdout pipe")?;
    let mut pumps = Vec::new();
    let output = if interactive {
      Some(Arc::new(tokio::sync::Mutex::new(InteractiveOutput {
        reader: tokio::io::BufReader::new(pipe),
        pending: Vec::new(),
      })))
    } else {
      pumps.push(pump(pipe, stdout.clone(), Some(output_w)));
      None
    };
    if let Some(e) = child.stderr.take() {
      pumps.push(pump(e, stderr.clone(), None));
    }
    tokio::spawn(own_process(
      ChildGroup::unregistered(child),
      requests,
      pumps,
      exit_w,
      failure_w,
    ));

    map.insert(
      name.to_string(),
      Proc {
        pid,
        input,
        output,
        started: Instant::now(),
        stdout,
        output_changed,
        stderr,
        exit,
        failure,
        control,
      },
    );
    Ok(pid)
  }

  pub async fn wait_for_output(&self, name: &str, text: &str) -> Result<String, String> {
    let (stdout, mut changed) = {
      let map = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      let process = map.get(name).ok_or_else(|| format!("no persistent process `{name}`"))?;
      if process.output.is_some() {
        return Err("use commands.read for interactive stdout".into());
      }
      (process.stdout.clone(), process.output_changed.clone())
    };
    loop {
      changed.borrow_and_update();
      let output = stdout.lock().unwrap_or_else(std::sync::PoisonError::into_inner).text();
      if output.contains(text) {
        return Ok(output);
      }
      changed
        .changed()
        .await
        .map_err(|_| format!("process `{name}` closed stdout before emitting {text:?}"))?;
    }
  }

  pub async fn write(&self, name: &str, data: Option<String>) -> Result<(), String> {
    let input = {
      let map = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      map
        .get(name)
        .ok_or_else(|| format!("no process `{name}`"))?
        .input
        .clone()
    };
    let mut input = input.lock().await;
    if let Some(data) = data {
      input
        .as_mut()
        .ok_or("stdin is closed")?
        .write_all(data.as_bytes())
        .await
        .map_err(|e| e.to_string())
    } else {
      input.take();
      Ok(())
    }
  }

  pub async fn read(&self, name: &str) -> Result<Option<String>, String> {
    let output = {
      let map = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      map
        .get(name)
        .ok_or_else(|| format!("no process `{name}`"))?
        .output
        .clone()
        .ok_or("use commands.open for interactive stdout")?
    };
    let mut output = output.lock().await;
    let InteractiveOutput { reader, pending } = &mut *output;
    loop {
      let chunk = reader.fill_buf().await.map_err(|e| e.to_string())?;
      if chunk.is_empty() {
        if pending.is_empty() {
          return Ok(None);
        }
        break;
      }
      let newline = chunk.iter().position(|&byte| byte == b'\n');
      let count = newline.unwrap_or(chunk.len());
      if pending.len() + count > OUTPUT_CAP {
        return Err(format!("command line exceeded {OUTPUT_CAP} bytes"));
      }
      pending.extend_from_slice(&chunk[..count]);
      reader.consume(count + usize::from(newline.is_some()));
      if newline.is_some() {
        break;
      }
    }
    String::from_utf8(std::mem::take(pending))
      .map(Some)
      .map_err(|e| e.to_string())
  }

  pub async fn wait(&self, name: &str) -> Result<i32, String> {
    let (exit, failure) = {
      let map = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      let process = map.get(name).ok_or_else(|| format!("no process `{name}`"))?;
      (process.exit.clone(), process.failure.clone())
    };
    wait_process(exit, failure).await
  }

  pub fn status(&self, name: &str) -> Result<serde_json::Value, String> {
    let map = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let p = map
      .get(name)
      .ok_or_else(|| format!("no persistent process `{name}` started in this session"))?;
    let exit = *p.exit.borrow();
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

  pub async fn stop(&self, name: &str) -> Result<(), String> {
    let (control, exit) = {
      let map = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      let process = map
        .get(name)
        .ok_or_else(|| format!("no persistent process `{name}` to stop"))?;
      (process.control.clone(), process.exit.clone())
    };
    stop_process(&control, &exit).await?;
    let mut map = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if map
      .get(name)
      .is_some_and(|process| process.control.same_channel(&control))
    {
      map.remove(name);
    }
    Ok(())
  }

  pub async fn close(&self) -> Result<(), String> {
    self.begin_close();
    let processes = {
      let map = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      map
        .iter()
        .map(|(name, process)| (name.clone(), process.control.clone(), process.exit.clone()))
        .collect::<Vec<_>>()
    };
    let mut errors = Vec::new();
    for (name, control, exit) in processes {
      match stop_process(&control, &exit).await {
        Ok(()) => {
          let mut map = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
          if map
            .get(&name)
            .is_some_and(|process| process.control.same_channel(&control))
          {
            map.remove(&name);
          }
        },
        Err(error) => errors.push(format!("{name}: {error}")),
      }
    }
    let jobs = self
      .jobs
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone();
    for job in jobs {
      match stop_process(&job.control, &job.exit).await {
        Ok(()) => self
          .jobs
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .retain(|entry| !entry.control.same_channel(&job.control)),
        Err(error) => errors.push(format!("one-shot command: {error}")),
      }
    }
    if errors.is_empty() {
      Ok(())
    } else {
      Err(errors.join("; "))
    }
  }

  pub(crate) fn begin_close(&self) {
    self.closing.store(true, std::sync::atomic::Ordering::Release);
  }
}

fn pump<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
  mut r: R,
  ring: Arc<Mutex<Ring>>,
  changed: Option<tokio::sync::watch::Sender<()>>,
) -> tokio::task::JoinHandle<()> {
  tokio::spawn(async move {
    let mut chunk = [0u8; 8192];
    loop {
      match r.read(&mut chunk).await {
        Ok(0) | Err(_) => break,
        Ok(n) => {
          ring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(&chunk[..n]);
          if let Some(changed) = &changed {
            changed.send_replace(());
          }
        },
      }
    }
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn command(source: &str, persistent: bool) -> ResolvedCommand {
    ResolvedCommand {
      exec: ResolvedExec::Shell(source.into()),
      timeout_ms: Some(2_000),
      env: Vec::new(),
      cwd: None,
      output: CommandOutput::Text,
      persistent,
    }
  }

  #[tokio::test]
  async fn natural_exit_cleans_descendants_and_preserves_output_for_all_waiters() {
    let procs = SessionProcs::default();
    procs
      .start("sashoush", &command("sleep 30 & printf ready; exit 7", true))
      .expect("start");
    let (first, second) = tokio::time::timeout(Duration::from_secs(3), async {
      tokio::join!(procs.wait("sashoush"), procs.wait("sashoush"))
    })
    .await
    .expect("descendant is cleaned before its sleep ends");
    assert_eq!(first.expect("first waiter"), 7);
    assert_eq!(second.expect("second waiter"), 7);
    assert_eq!(procs.wait("sashoush").await.expect("late waiter"), 7);
    assert_eq!(procs.status("sashoush").expect("retained output")["stdout"], "ready");
    procs.stop("sashoush").await.expect("stop completed process");
  }

  #[tokio::test]
  async fn close_waits_for_termination_and_permanently_closes_admission() {
    let procs = SessionProcs::default();
    let spec = command("printf ready; exec sleep 30", true);
    procs.start("sashoush", &spec).expect("start");
    procs.wait_for_output("sashoush", "ready").await.expect("ready");
    let exit = procs.inner.lock().expect("registry")["sashoush"].exit.clone();
    procs.close().await.expect("close");
    assert_eq!(*exit.borrow(), Some(-1));
    assert!(procs.inner.lock().expect("registry").is_empty());
    assert!(
      procs
        .start("sashoush", &spec)
        .expect_err("closed admission")
        .contains("closing")
    );
    assert!(
      procs
        .run_oneshot(&command("printf unexpected", false))
        .await
        .expect_err("closed admission")
        .contains("closing")
    );
    procs.close().await.expect("repeated close");
  }

  #[tokio::test]
  async fn cancelled_oneshot_remains_owned_until_cleanup_finishes() {
    let procs = Arc::new(SessionProcs::default());
    let worker = Arc::clone(&procs);
    let task = tokio::spawn(async move { worker.exec_oneshot(&command("exec sleep 30", false)).await });
    let job = tokio::time::timeout(Duration::from_secs(3), async {
      loop {
        if let Some(job) = procs.jobs.lock().expect("jobs").first().cloned() {
          break job;
        }
        tokio::task::yield_now().await;
      }
    })
    .await
    .expect("job registered");
    task.abort();
    assert!(task.await.err().expect("cancel caller").is_cancelled());
    tokio::time::timeout(Duration::from_secs(3), procs.close())
      .await
      .expect("bounded cleanup")
      .expect("close");
    assert_eq!(*job.exit.borrow(), Some(-1));
    assert!(procs.jobs.lock().expect("jobs").is_empty());
  }

  #[tokio::test]
  async fn oneshot_preserves_leader_status_and_terminates_remaining_descendants() {
    let procs = SessionProcs::default();
    let result = procs
      .exec_oneshot(&command("sleep 30 & printf ready; exit 7", false))
      .await
      .expect("result");
    assert_eq!(result.exit_code, Some(7));
    assert!(!result.success);
    assert_eq!(result.stdout, "ready");
    assert!(procs.jobs.lock().expect("jobs").is_empty());
  }

  #[tokio::test]
  async fn timeout_and_output_overflow_finish_cleanup_before_returning() {
    let procs = SessionProcs::default();
    let mut spec = command("exec sleep 30", false);
    spec.timeout_ms = Some(20);
    let error = procs.exec_oneshot(&spec).await.err().expect("timeout");
    assert!(
      error.contains("timed out after 20ms waiting for the process to exit"),
      "{error}"
    );
    assert!(error.contains("(the process is still running"), "{error}");
    #[cfg(target_os = "linux")]
    assert!(error.contains(" state "), "{error}");
    assert!(procs.jobs.lock().expect("jobs").is_empty());
    let error = procs
      .exec_oneshot(&command("exec yes sashoush", false))
      .await
      .err()
      .expect("output overflow");
    assert!(error.contains("output exceeded"), "{error}");
    assert!(procs.jobs.lock().expect("jobs").is_empty(), "{error}");
  }
}

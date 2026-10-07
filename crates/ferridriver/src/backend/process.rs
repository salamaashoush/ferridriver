//! Shared browser-process lifecycle helpers.
//!
//! Chrome/Firefox spawn a pool of subprocesses (renderer, GPU, utility,
//! zygote). When the parent dies via SIGKILL — as happens when a test
//! harness panics or `kill_on_drop(true)` fires — those helpers are
//! supposed to notice the parent IPC pipe closing and exit on their own.
//! In practice, on macOS this is flaky: helpers can linger for seconds
//! or get stuck, showing up as "automation Chrome zombies" in tools like
//! `devgate browser zombies` that pgrep `--remote-debugging` etc.
//!
//! Defence: every browser spawn calls `setsid()` in `pre_exec`, making
//! the parent its own session-and-process-group leader. Every helper
//! the parent forks inherits that group. On teardown we explicitly
//! `killpg(-pgid, SIGKILL)` so the whole group dies together — no
//! lingering helpers, regardless of how the parent itself died.
//!
//! Combine with `tokio::process::Command::kill_on_drop(true)`:
//! - `kill_on_drop` covers the *Rust* side (SIGKILL to the parent PID
//!   when the `Child` handle drops).
//! - `killpg` covers the *OS* side (all helpers in the same group die
//!   too, even if Chrome itself crashed or spun off sandboxed children).

pub(crate) async fn output(
  mut command: tokio::process::Command,
  input: Option<&[u8]>,
  timeout: Option<std::time::Duration>,
) -> crate::error::Result<String> {
  use std::process::Stdio;

  use tokio::io::{AsyncReadExt, AsyncWriteExt};

  use crate::error::FerriError;
  let program = command.as_std().get_program().to_string_lossy().into_owned();
  command.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
  command.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
  #[cfg(unix)]
  command.process_group(0);
  let mut child = command
    .spawn()
    .map_err(|error| FerriError::backend(format!("starting {program}: {error}")))?;
  let stdin = child.stdin.take();
  let stdout = child.stdout.take();
  let stderr = child.stderr.take();
  let mut group = ChildGroup::new(child);
  let operation = async {
    let write = async {
      if let (Some(input), Some(mut stdin)) = (input, stdin) {
        stdin.write_all(input).await?;
      }
      Ok::<_, std::io::Error>(())
    };
    let read_out = async {
      let mut bytes = Vec::new();
      if let Some(mut stdout) = stdout {
        stdout.read_to_end(&mut bytes).await?;
      }
      Ok::<_, std::io::Error>(bytes)
    };
    let read_err = async {
      let mut bytes = Vec::new();
      if let Some(mut stderr) = stderr {
        stderr.read_to_end(&mut bytes).await?;
      }
      Ok::<_, std::io::Error>(bytes)
    };
    tokio::try_join!(group.wait(), write, read_out, read_err)
  };
  let result = match timeout {
    Some(timeout) => tokio::time::timeout(timeout, operation)
      .await
      .map_err(|_| FerriError::backend(format!("{program} exceeded its command deadline")))?,
    None => operation.await,
  }?;
  let (status, (), stdout, stderr) = result;
  if !status.success() {
    return Err(FerriError::backend(format!(
      "{program} exited {status}: {}{}",
      String::from_utf8_lossy(&stdout),
      String::from_utf8_lossy(&stderr)
    )));
  }
  Ok(String::from_utf8_lossy(&stdout).trim().to_owned())
}

/// `pre_exec` closure suitable for every browser `Command` in this crate.
///
/// Runs inside the forked child before `exec`, putting the child in its
/// own session and process group. Any error is silently ignored —
/// failing `setsid` only matters when the current process is already a
/// session leader, which is fine for tests.
///
/// # Safety
///
/// `setsid()` is async-signal-safe per POSIX.1-2017, so it is safe to
/// call from `pre_exec`. No allocation, no mutex, no non-reentrant C
/// functions. The returned closure captures nothing.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn setsid_pre_exec() -> impl FnMut() -> std::io::Result<()> + Send + Sync + 'static {
  || {
    // SAFETY: `setsid` is async-signal-safe and has no side effects on
    // the parent. A return of -1 means we're already a session leader
    // (errno=EPERM) which is benign for our purposes.
    unsafe {
      libc::setsid();
    }
    Ok(())
  }
}

/// Continuously drain a spawned browser's piped stderr into tracing.
///
/// A browser launched with `Stdio::piped()` stderr MUST have that pipe
/// read for its whole life: Chrome logs every renderer console message
/// to stderr (`INFO:CONSOLE` lines), and once the 64KB kernel pipe
/// buffer fills, the browser process blocks in `write(2)` on the same
/// thread that routes `DevTools` traffic — every CDP command and event
/// freezes until the pipe is drained (observed as `Runtime.evaluate`
/// 30s timeouts after ~1000 console.log calls). Playwright drains the
/// stream unconditionally for the same reason
/// (`packages/utils/processLauncher.ts` — stderr piped into the
/// `pw:browser` debug channel).
pub fn drain_child_stderr(child: &mut tokio::process::Child) -> StderrTail {
  let tail = StderrTail::default();
  let Some(stderr) = child.stderr.take() else {
    return tail;
  };
  drain_stream(stderr, tail.clone(), "stderr");
  tail
}

pub(crate) fn drain_child_output(child: &mut tokio::process::Child) -> StderrTail {
  let tail = drain_child_stderr(child);
  if let Some(stdout) = child.stdout.take() {
    drain_stream(stdout, tail.clone(), "stdout");
  }
  tail
}

fn drain_stream(stream: impl tokio::io::AsyncRead + Unpin + Send + 'static, tail: StderrTail, name: &'static str) {
  let sink = tail;
  tokio::spawn(async move {
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(stream).lines();
    while let Ok(Some(line)) = lines.next_line().await {
      tracing::debug!(target: "ferridriver::browser::stderr", stream = name, "{line}");
      sink.record(line);
    }
  });
}

/// How many of the browser's most recent stderr lines to keep for error
/// messages. Enough to carry a policy refusal or a crash banner, small
/// enough that a chatty renderer cannot grow it without bound.
const STDERR_TAIL_LINES: usize = 20;

/// The tail of a browser child's stderr, captured while it is drained.
///
/// Draining is mandatory (see [`drain_child_stderr`]); keeping the tail is
/// what lets a launch failure quote the browser's own explanation instead of
/// reporting a bare timeout. Chrome refuses remote debugging under an
/// enterprise `RemoteDebuggingAllowed=false` policy by printing one line and
/// otherwise starting normally, which is invisible without this.
#[derive(Clone, Default)]
pub struct StderrTail(std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>);

impl StderrTail {
  /// Append one line, evicting the oldest once the window is full. A
  /// poisoned lock drops the line rather than propagating: losing a
  /// diagnostic must never take down a browser launch.
  pub(crate) fn record(&self, line: String) {
    if let Ok(mut buf) = self.0.lock() {
      if buf.len() == STDERR_TAIL_LINES {
        buf.pop_front();
      }
      buf.push_back(line);
    }
  }

  /// The captured lines, oldest first. Empty when the child had no piped
  /// stderr or has printed nothing yet.
  #[must_use]
  pub fn lines(&self) -> Vec<String> {
    self.0.lock().map(|b| b.iter().cloned().collect()).unwrap_or_default()
  }

  /// The tail rendered for an error message, or `None` when nothing was
  /// captured — so a caller never appends an empty "stderr:" section.
  #[must_use]
  pub fn as_error_context(&self) -> Option<String> {
    let lines = self.lines();
    if lines.is_empty() {
      return None;
    }
    Some(format!("browser stderr:\n  {}", lines.join("\n  ")))
  }
}

/// Send `SIGKILL` to every process in the given pid's process group.
///
/// Assumes the target was spawned with [`setsid_pre_exec`], so its
/// `pgid == pid`. Failures are ignored — ESRCH (group already dead)
/// and EPERM are the common cases.
///
/// Callers MUST only pass the pid of a child that has NOT been reaped
/// yet: the kernel keeps an unreaped pid reserved, so the group is
/// guaranteed to still be ours. A reaped pid may already be recycled
/// by an unrelated same-UID process (which `killpg` WILL kill —
/// think a parallel test run's freshly-launched Firefox dying
/// mid-startup). [`ChildGroup`] retains the unreaped child until cleanup.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn kill_process_group(pid: u32) {
  // Cast is safe: Chrome PIDs fit in i32 on every platform we target.
  #[allow(clippy::cast_possible_wrap)]
  let group_id = pid as i32;
  // SAFETY: `killpg` is async-signal-safe. `SIGKILL` is always
  // deliverable.
  unsafe {
    libc::killpg(group_id, libc::SIGKILL);
  }
}

#[cfg(not(unix))]
pub fn kill_process_group(_pid: u32) {
  // Windows: process groups are a different concept (Job Objects).
  // `tokio::process::Child::kill_on_drop` already terminates the
  // parent; subprocess cleanup on Windows is handled by Chrome itself.
}

/// `tokio::process::Child` wrapper that kills the entire process group
/// on drop. Combine with [`setsid_pre_exec`] on the `Command` so the
/// parent is its own session+group leader; every helper it forks
/// inherits the group and dies together on teardown. Without this,
/// SIGKILL to the parent leaves renderer/GPU/zygote subprocesses
/// behind on macOS — visible as "automation Chrome zombies" in
/// `devgate browser zombies`.
///
/// The inner `Child` still has `kill_on_drop(true)` set, so the parent
/// PID is also killed directly (belt + suspenders). The group kill
/// runs first because fields drop in declaration order.
pub struct ChildGroup {
  pid: u32,
  child: tokio::process::Child,
  registered: bool,
  directory_cleanup: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
  /// On-disk record of this browser, removed once the process is
  /// killed. Left behind when our own process dies without running
  /// `Drop` (SIGKILL, panic-abort) — exactly the case
  /// [`sweep_stale_browsers`] reclaims on the next start.
  record: Option<ProcRecord>,
}

impl ChildGroup {
  #[must_use]
  pub fn new(child: tokio::process::Child) -> Self {
    Self::recorded(child, None, false)
  }

  /// Own a command without adding it to the browser recovery registry.
  #[must_use]
  pub fn unregistered(child: tokio::process::Child) -> Self {
    Self {
      pid: child.id().unwrap_or(0),
      child,
      registered: false,
      directory_cleanup: None,
      record: None,
    }
  }

  /// Like [`Self::new`], but also writes a launch record so a later run
  /// can reclaim this browser if our process dies without teardown.
  /// `profile_dir` is the browser's `--user-data-dir` / `--profile`;
  /// `owns_profile_dir` marks it as ours to delete (a temp dir), false
  /// for a caller-supplied persistent directory.
  #[must_use]
  pub fn recorded(child: tokio::process::Child, profile_dir: Option<&std::path::Path>, owns_profile_dir: bool) -> Self {
    // `id()` is `None` only after the child has been polled to
    // completion; fresh children always have an id.
    let pid = child.id().unwrap_or(0);
    let record = if pid == 0 {
      None
    } else {
      // Two layers, because they cover different deaths: the record
      // survives a hard kill and is reclaimed by the next start, the
      // watchdog acts immediately but only while it is alive itself.
      super::reaper::watch(pid);
      ProcRecord::write(pid, profile_dir, owns_profile_dir)
    };
    Self {
      pid,
      child,
      registered: pid != 0,
      directory_cleanup: None,
      record,
    }
  }

  /// Add a temp directory to this launch's cleanup set. Removed when
  /// the browser is torn down, and reclaimed by
  /// [`sweep_stale_browsers`] if this process never gets to.
  pub fn own_dir(&mut self, dir: &std::path::Path) {
    if let Some(record) = self.record.as_mut() {
      record.own_dir(dir);
    }
  }

  /// Whether the browser process is still running. `false` once it has
  /// exited, so a dead-browser check never has to depend on a
  /// backend-specific transport signal.
  pub fn is_running(&mut self) -> bool {
    #[cfg(unix)]
    {
      self.child.id().is_some() && matches!(self.exit_observed(), Ok(false))
    }
    #[cfg(not(unix))]
    {
      matches!(self.child.try_wait(), Ok(None))
    }
  }

  #[cfg(unix)]
  fn exit_observed(&self) -> std::io::Result<bool> {
    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
    let pid = Pid::from_raw(i32::try_from(self.pid).map_err(std::io::Error::other)?)
      .filter(|pid| *pid != Pid::INIT)
      .ok_or_else(|| std::io::Error::other("child has no owned process id"))?;
    loop {
      match waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG,
      ) {
        Ok(status) => return Ok(status.is_some()),
        Err(rustix::io::Errno::INTR) => {},
        Err(error) => return Err(error.into()),
      }
    }
  }

  #[cfg(unix)]
  async fn observe_exit(&self) -> std::io::Result<()> {
    use std::task::Poll;
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
    std::future::poll_fn(|cx| {
      loop {
        // Register before inspecting the child, so an exit between inspection
        // and suspension cannot lose its only SIGCHLD notification.
        let pending = signal.poll_recv(cx).is_pending();
        if self.exit_observed()? {
          return Poll::Ready(Ok(()));
        }
        if pending {
          return Poll::Pending;
        }
      }
    })
    .await
  }

  fn retire_registration(&mut self) -> std::io::Result<()> {
    if self.registered {
      super::reaper::forget(self.pid);
      self.registered = false;
    }
    if let Some(record) = &mut self.record {
      record.retire()?;
    }
    Ok(())
  }

  async fn remove_owned_directories(&mut self) -> std::io::Result<()> {
    let Some(record) = &self.record else { return Ok(()) };
    let cleanup = self.directory_cleanup.get_or_insert_with(|| {
      let directories = std::sync::Arc::clone(&record.owned_dirs);
      tokio::task::spawn_blocking(move || {
        directories
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .remove()
      })
    });
    let result = cleanup.await;
    self.directory_cleanup = None;
    result.map_err(std::io::Error::other)?
  }

  /// Request termination while the child still reserves its process id.
  ///
  /// # Errors
  /// Returns an error if the operating system rejects termination.
  pub fn terminate(&mut self) -> std::io::Result<()> {
    if self.child.id().is_none() {
      return Ok(());
    }
    #[cfg(unix)]
    {
      use rustix::process::{Pid, Signal, kill_process_group};
      let pid = Pid::from_raw(i32::try_from(self.pid).map_err(std::io::Error::other)?)
        .filter(|pid| *pid != Pid::INIT)
        .ok_or_else(|| std::io::Error::other("child has no owned process id"))?;
      match kill_process_group(pid, Signal::KILL) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => {},
        #[cfg(target_os = "macos")]
        Err(rustix::io::Errno::PERM) if process_group_exiting_settled(self.pid) => {},
        Err(error) => return Err(error.into()),
      }
    }
    self.child.start_kill()
  }

  /// Wait for the leader, terminate its remaining group, then reap it.
  ///
  /// # Errors
  /// Returns observation, termination, registry or wait errors without
  /// surrendering the child to another owner.
  pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
    if self.child.id().is_some() {
      #[cfg(unix)]
      {
        self.observe_exit().await?;
        self.terminate()?;
      }
      #[cfg(not(unix))]
      self.child.wait().await?;
    }
    self.retire_registration()?;
    self.remove_owned_directories().await?;
    self.child.wait().await
  }

  /// Wait for a browser to exit by itself after a graceful close. `Err`
  /// says why it did not, so a failed flush names the cause.
  ///
  /// A browser that exits on its own has finished its shutdown, so a
  /// non-zero status is logged rather than failed, as Playwright does.
  /// Chromium on Apple Silicon CI runners exits with 2, its hang code, at
  /// the end of every persistent-profile close.
  pub(crate) async fn wait_for_exit(&mut self, timeout: std::time::Duration) -> Result<(), String> {
    match tokio::time::timeout(timeout, self.wait()).await {
      Ok(Ok(status)) => {
        if !status.success() {
          tracing::warn!(%status, "browser exited with a failure status after a graceful close");
        }
        Ok(())
      },
      Ok(Err(error)) => Err(format!("waiting for it failed: {error}")),
      Err(_) => Err(format!("it was still running after {}s", timeout.as_secs())),
    }
  }

  /// Kill the whole process group, then reap the parent. The group
  /// kill happens BEFORE reaping: an unreaped child's pid cannot be
  /// recycled by the kernel, so the `killpg` target is guaranteed to
  /// still be our group. Reaping afterwards means the enclosing
  /// runtime carries no zombie.
  ///
  /// # Errors
  /// Reports termination, directory, registry or wait failures; the owner remains retryable.
  pub async fn shutdown(&mut self) -> std::io::Result<()> {
    self.terminate()?;
    self.wait().await?;
    self.record.take();
    Ok(())
  }
}

impl Drop for ChildGroup {
  fn drop(&mut self) {
    if let Err(error) = self.terminate() {
      tracing::warn!(%error, "failed to clean up owned process group during drop");
    }
    if self.registered {
      super::reaper::forget(self.pid);
    }
    // Retire the PID record before the child field drops. An active directory
    // worker retains its separate recovery record without that PID authority.
    self.record.take();
  }
}

// ── Launch registry ─────────────────────────────────────────────────────────
//
// Killing the browser on teardown covers every path where this process
// gets to run code. It does not cover SIGKILL, `panic = "abort"`, or a
// host crash — and only the pipe-transport backends (chromium over fd
// 3/4, webkit over pw_run.sh) exit by themselves when the parent
// vanishes. A websocket-transport Chrome or a BiDi Firefox survives
// indefinitely, reparented to pid 1, holding its profile directory and
// (headed, on macOS) a dock tile with no window. So every launch drops
// a record on disk and the next process start reclaims what the
// previous one leaked.

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct BrowserRecord {
  owner_pid: u32,
  browser_pid: u32,
  /// Identifies the process at sweep time; present even when the
  /// directory belongs to the caller and must not be deleted.
  profile_dir: Option<String>,
  /// Temp directories this browser owns: its profile when we made it,
  /// plus the downloads directory.
  #[serde(default)]
  owned_dirs: Vec<String>,
  /// The browser's start time as `ps` reports it. Pids are recycled, and
  /// (pid, start time) is the pair that identifies a process across that:
  /// without it a stale record can name someone else's browser. Absent on
  /// records written by an older build, which fall back to matching the
  /// command line alone.
  #[serde(default)]
  start_time: Option<String>,
}

// Concurrent sweepers delete malformed JSON, so never expose a partial update.
fn write_record_file(path: &std::path::Path, record: &BrowserRecord) -> std::io::Result<()> {
  use std::io::Write;

  let parent = path
    .parent()
    .ok_or_else(|| std::io::Error::other("process record has no parent directory"))?;
  let bytes = serde_json::to_vec(record).map_err(std::io::Error::other)?;
  let mut file = tempfile::NamedTempFile::new_in(parent)?;
  file.write_all(&bytes)?;
  file.persist(path).map_err(|error| error.error)?;
  Ok(())
}

/// A launch-record file, deleted when the browser it describes is
/// killed through [`ChildGroup`].
///
/// It also owns the browser's temp directories, so they and the record
/// that would let a later run reclaim them disappear together. Leaving
/// removal to the handle holding the `TempDir` meant a browser that was
/// dropped rather than closed deferred the removal to a runtime that
/// was already shutting down: the record went away, the directory did
/// not, and nothing was left pointing at it.
pub struct ProcRecord {
  path: Option<std::path::PathBuf>,
  record: BrowserRecord,
  owned_dirs: std::sync::Arc<std::sync::Mutex<OwnedDirectories>>,
}

struct OwnedDirectories {
  paths: Vec<std::path::PathBuf>,
  recovery_path: Option<std::path::PathBuf>,
}

impl OwnedDirectories {
  fn remove(&mut self) -> std::io::Result<()> {
    for dir in &self.paths {
      super::async_tempdir::removal_result(dir, std::fs::remove_dir_all(dir))?;
    }
    self.paths.clear();
    if let Some(path) = &self.recovery_path {
      remove_record_file(path)?;
      self.recovery_path = None;
    }
    Ok(())
  }
}

impl Drop for OwnedDirectories {
  fn drop(&mut self) {
    if let Err(error) = self.remove() {
      tracing::warn!(%error, "process directory cleanup failed; retaining recovery record");
    }
  }
}

impl Drop for ProcRecord {
  fn drop(&mut self) {
    if let Err(error) = self.retire() {
      tracing::warn!(%error, "failed to retire process recovery record");
    }
  }
}

fn remove_record_file(path: &std::path::Path) -> std::io::Result<()> {
  match std::fs::remove_file(path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error),
  }
}

fn registry_dir() -> Option<std::path::PathBuf> {
  let dir = dirs::cache_dir()?.join("ferridriver").join("procs");
  std::fs::create_dir_all(&dir).ok()?;
  Some(dir)
}

/// Start tracking a browser the instant it is spawned, before any
/// protocol handshake.
///
/// Registration used to happen when the [`ChildGroup`] was built, which
/// is after the launcher has connected and completed its handshake. For
/// a `BiDi` Firefox that window is seconds long, and a process killed
/// inside it left a browser nothing knew about — no watchdog entry, no
/// record for the next start to sweep. Called again by
/// [`ChildGroup::recorded`], which is harmless: the record path is
/// derived from the pid, and the watchdog drops every copy of a pid
/// when it is unwatched.
pub fn track_spawned(pid: u32, profile_dir: Option<&std::path::Path>, owns_profile_dir: bool) {
  if pid == 0 {
    return;
  }
  super::reaper::watch(pid);
  let _ = ProcRecord::write_file(pid, profile_dir, owns_profile_dir);
}

impl ProcRecord {
  /// Write the record file and return its path plus the dirs it owns.
  fn write_file(
    browser_pid: u32,
    profile_dir: Option<&std::path::Path>,
    owns_profile_dir: bool,
  ) -> Option<(std::path::PathBuf, Vec<std::path::PathBuf>)> {
    let dir = registry_dir()?;
    let owner_pid = std::process::id();
    let owned_dirs: Vec<std::path::PathBuf> = if owns_profile_dir {
      profile_dir.map(std::path::Path::to_path_buf).into_iter().collect()
    } else {
      Vec::new()
    };
    let record = BrowserRecord {
      owner_pid,
      browser_pid,
      profile_dir: profile_dir.map(|p| p.to_string_lossy().into_owned()),
      owned_dirs: owned_dirs.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
      start_time: process_start_time(browser_pid),
    };
    let path = dir.join(format!("{owner_pid}-{browser_pid}.json"));
    write_record_file(&path, &record).ok()?;
    Some((path, owned_dirs))
  }

  /// Take ownership of the record for `browser_pid`, so teardown
  /// removes both the file and the temp dirs it names.
  fn write(browser_pid: u32, profile_dir: Option<&std::path::Path>, owns_profile_dir: bool) -> Option<Self> {
    let (path, owned_dirs) = Self::write_file(browser_pid, profile_dir, owns_profile_dir)?;
    let record = BrowserRecord {
      owner_pid: std::process::id(),
      browser_pid,
      profile_dir: profile_dir.map(|p| p.to_string_lossy().into_owned()),
      owned_dirs: owned_dirs.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
      start_time: process_start_time(browser_pid),
    };
    Some(Self {
      path: Some(path),
      record,
      owned_dirs: std::sync::Arc::new(std::sync::Mutex::new(OwnedDirectories {
        paths: owned_dirs,
        recovery_path: None,
      })),
    })
  }

  /// Hand another temp directory to this record, so it is removed with
  /// the browser and reclaimed by the sweep if the process is killed.
  fn own_dir(&mut self, dir: &std::path::Path) {
    self
      .owned_dirs
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .paths
      .push(dir.to_path_buf());
    self.record.owned_dirs.push(dir.to_string_lossy().into_owned());
    if let Some(path) = &self.path {
      let _ = write_record_file(path, &self.record);
    }
  }

  fn retire(&mut self) -> std::io::Result<()> {
    let Some(path) = &self.path else { return Ok(()) };
    let mut directories = self
      .owned_dirs
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !directories.paths.is_empty() && directories.recovery_path.is_none() {
      use std::io::Write as _;
      let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("process record has no parent"))?;
      let mut record = self.record.clone();
      record.browser_pid = 0;
      record.profile_dir = None;
      record.start_time = None;
      let mut file = tempfile::Builder::new().prefix("cleanup-").tempfile_in(parent)?;
      file.write_all(&serde_json::to_vec(&record).map_err(std::io::Error::other)?)?;
      let recovery_path = file.path().with_extension("json");
      file.persist_noclobber(&recovery_path).map_err(|error| error.error)?;
      directories.recovery_path = Some(recovery_path);
    }
    remove_record_file(path)?;
    self.path = None;
    Ok(())
  }
}

/// Whether `pid` names a live process. A recycled pid reads as live,
/// which keeps the sweep conservative: it never kills on a maybe.
#[cfg(unix)]
#[allow(unsafe_code)]
fn process_is_live(pid: u32) -> bool {
  #[allow(clippy::cast_possible_wrap)]
  let pid = pid as i32;
  // SAFETY: signal 0 performs the existence/permission check only and
  // delivers nothing.
  unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

#[cfg(not(unix))]
fn process_is_live(_pid: u32) -> bool {
  true
}

/// The process's start time, as `<seconds>.<microseconds>` since the
/// epoch. Paired with the pid it survives pid recycling, which a command
/// line alone does not: two automation browsers look identical.
///
/// Read from the kernel, never by running `ps`. Every browser launch
/// records a start time, so this used to fork+exec a `ps` per launch on
/// a tokio worker thread — and `Command::output()` blocks until the
/// child closes its pipes, which under a full parallel suite could
/// wedge a worker for the rest of the run.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn process_start_time(pid: u32) -> Option<String> {
  let info = proc_bsdinfo(pid)?;
  Some(format!("{}.{:06}", info.pbi_start_tvsec, info.pbi_start_tvusec))
}

/// `proc_pidinfo(PROC_PIDTBSDINFO)` for `pid`, or `None` when the
/// process is gone or not ours to inspect.
#[cfg(target_os = "macos")]
fn proc_bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
  proc_bsdinfo_including_exited(pid, false)
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn proc_bsdinfo_including_exited(pid: u32, include_exited: bool) -> Option<libc::proc_bsdinfo> {
  #[allow(clippy::cast_possible_wrap)]
  let pid = pid as i32;
  let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
  let size = i32::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
  // SAFETY: `info` is a live, correctly sized `proc_bsdinfo`; the call
  // only writes into it and reports how many bytes it wrote.
  let written = unsafe {
    libc::proc_pidinfo(
      pid,
      libc::PROC_PIDTBSDINFO,
      u64::from(include_exited),
      std::ptr::from_mut(&mut info).cast::<libc::c_void>(),
      size,
    )
  };
  (written == size).then_some(info)
}

/// How long a browser holding a caller-owned profile gets to flush it and
/// exit after a graceful close. Playwright allows its launch timeout, 30s;
/// 5s ran out on a 3-vCPU macOS runner while Chromium was still closing.
pub(crate) const PROFILE_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// XNU's `killpg` answers EPERM when no member can be signalled, which is
/// what a group whose leader is mid-exit looks like. That leader is visible
/// to neither `proc_listpgrppids` nor `proc_pidinfo` until it settles into a
/// zombie, so the snapshot taken straight after the EPERM can miss it and
/// the close failed. A few milliseconds later it is a zombie the snapshot
/// accepts; this blocks only on that error path, for at most 100ms.
#[cfg(target_os = "macos")]
fn process_group_exiting_settled(pid: u32) -> bool {
  for _ in 0..20 {
    if process_group_exiting(pid) {
      return true;
    }
    std::thread::sleep(std::time::Duration::from_millis(5));
  }
  false
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn process_group_exiting(pid: u32) -> bool {
  const PROC_FLAG_INEXIT: u32 = 4;
  let Ok(group) = i32::try_from(pid) else { return false };
  let mut members = vec![0_i32; 16];
  loop {
    let Ok(size) = i32::try_from(std::mem::size_of_val(members.as_slice())) else {
      return false;
    };
    // XNU killpg1 also loses exiting members before they become waitable
    // zombies. Require a complete group snapshot proving exit has begun;
    // an unknown or live member must preserve the permission error.
    // SAFETY: libproc writes at most `size` bytes into this initialized PID
    // buffer. Its result is a PID count, checked before indexing the buffer.
    let count = unsafe { libc::proc_listpgrppids(group, members.as_mut_ptr().cast(), size) };
    let Ok(count) = usize::try_from(count) else {
      return false;
    };
    if count == 0 {
      return false;
    }
    if count >= members.len() {
      let Some(capacity) = members.len().checked_mul(2) else {
        return false;
      };
      members.resize(capacity, 0);
      continue;
    }
    return members[..count].iter().all(|&member| {
      u32::try_from(member)
        .ok()
        .and_then(|pid| proc_bsdinfo_including_exited(pid, true))
        .is_some_and(|info| {
          info.pbi_pgid == pid && (info.pbi_status == libc::SZOMB || info.pbi_flags & PROC_FLAG_INEXIT != 0)
        })
    });
  }
}

/// `/proc/<pid>/stat` field 22 — start time in clock ticks since boot.
/// Unique per (pid, process) exactly like the macOS form; the two are
/// never compared with each other, only with a value this same function
/// produced.
#[cfg(target_os = "linux")]
fn process_start_time(pid: u32) -> Option<String> {
  let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
  // The comm field is parenthesised and may contain spaces, so fields
  // are counted from after the closing parenthesis.
  let rest = stat.rsplit_once(')')?.1;
  // After the comm field come state (3), ppid (4), ... so starttime
  // (22) is the twentieth token here.
  rest.split_whitespace().nth(19).map(ToString::to_string)
}

/// The process's full command line (arguments joined by spaces), or
/// `None` when it cannot be read.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn process_command(pid: u32) -> Option<String> {
  #[allow(clippy::cast_possible_wrap)]
  let pid_arg = pid as i32;
  let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid_arg];
  let mut len: libc::size_t = 0;
  // SAFETY: a null buffer with a live length pointer asks for the size.
  let sized = unsafe {
    libc::sysctl(
      mib.as_mut_ptr(),
      3,
      std::ptr::null_mut(),
      std::ptr::addr_of_mut!(len),
      std::ptr::null_mut(),
      0,
    )
  };
  if sized != 0 || len == 0 {
    return None;
  }
  let mut buf = vec![0u8; len];
  // SAFETY: `buf` has exactly `len` bytes and `len` is updated in place.
  let read = unsafe {
    libc::sysctl(
      mib.as_mut_ptr(),
      3,
      buf.as_mut_ptr().cast::<libc::c_void>(),
      std::ptr::addr_of_mut!(len),
      std::ptr::null_mut(),
      0,
    )
  };
  if read != 0 {
    return None;
  }
  buf.truncate(len);
  Some(parse_procargs2(&buf))
}

/// `KERN_PROCARGS2` payload: `argc` as a 32-bit int, the executable
/// path, NUL padding, then `argc` NUL-terminated arguments.
#[cfg(target_os = "macos")]
fn parse_procargs2(buf: &[u8]) -> String {
  let Some((count, rest)) = buf.split_at_checked(4) else {
    return String::new();
  };
  let argc = u32::from_ne_bytes([count[0], count[1], count[2], count[3]]) as usize;
  // Skip the executable path and the NUL padding that follows it.
  let after_path = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
  let mut cursor = &rest[after_path..];
  while cursor.first() == Some(&0) {
    cursor = &cursor[1..];
  }
  cursor
    .split(|b| *b == 0)
    .take(argc)
    .map(|arg| String::from_utf8_lossy(arg).into_owned())
    .collect::<Vec<_>>()
    .join(" ")
}

#[cfg(target_os = "linux")]
fn process_command(pid: u32) -> Option<String> {
  let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
  let joined = raw
    .split(|b| *b == 0)
    .filter(|arg| !arg.is_empty())
    .map(|arg| String::from_utf8_lossy(arg).into_owned())
    .collect::<Vec<_>>()
    .join(" ");
  (!joined.is_empty()).then_some(joined)
}

/// What a live pid from a record turned out to be.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Identity {
  /// The browser this record describes; safe to signal.
  Ours,
  /// A different process wearing a recycled pid; must not be signalled.
  NotOurs,
  /// `ps` told us nothing. Neither killing nor cleaning up is justified —
  /// the next sweep asks again.
  Unknown,
}

/// Decide whether the live process at `record.browser_pid` is the browser the
/// record describes.
///
/// A pid alone proves nothing: the kernel recycles them, and one automation
/// browser's command line looks like any other's. The start time pins the pid
/// to a single process — `(pid, start time)` is unique — and the command-line
/// check stays on top of it as a second signal, so a pid recycled within the
/// same second is still rejected.
fn identify_browser(record: &BrowserRecord) -> Identity {
  if let Some(recorded) = record.start_time.as_deref() {
    match process_start_time(record.browser_pid) {
      Some(live) if live != recorded => return Identity::NotOurs,
      Some(_) => {},
      None => return Identity::Unknown,
    }
  }
  let Some(cmd) = process_command(record.browser_pid) else {
    return Identity::Unknown;
  };
  let looks_right = match record.profile_dir {
    Some(ref dir) => cmd.contains(dir.as_str()),
    None => cmd.contains("--inspector-pipe") || cmd.contains("--remote-debugging"),
  };
  if looks_right { Identity::Ours } else { Identity::NotOurs }
}

/// Whether a recorded directory is somewhere we are willing to delete
/// recursively. Every directory we own is a temp dir we created, so anything
/// outside the temp root or our own cache directory is a corrupt or tampered
/// record, not ours to remove.
fn is_reclaimable_dir(path: &std::path::Path) -> bool {
  let Ok(target) = path.canonicalize() else {
    return false;
  };
  [
    Some(std::env::temp_dir()),
    dirs::cache_dir().map(|c| c.join("ferridriver")),
  ]
  .into_iter()
  .flatten()
  .filter_map(|root| root.canonicalize().ok())
  .any(|root| target.starts_with(&root) && target != root)
}

/// Reclaim browsers launched by ferridriver processes that are no
/// longer running: kill the process group and remove the temp profile
/// directory it was holding. Returns the number of browsers killed.
///
/// Browsers owned by a LIVE process — another MCP session, a parallel test
/// run — are never touched: the record is skipped while its owner is alive.
///
/// Safe against pid reuse: a recorded browser is signalled only once its
/// start time still matches the record and its command line still names the
/// record's profile directory. A pid we cannot identify is left alone, along
/// with the record, for the next sweep to re-examine.
#[must_use]
pub fn sweep_stale_browsers() -> usize {
  let Some(dir) = registry_dir() else {
    return 0;
  };
  let Ok(entries) = std::fs::read_dir(&dir) else {
    return 0;
  };
  let mut reclaimed = 0;
  for entry in entries.flatten() {
    let path = entry.path();
    if path.extension().and_then(|e| e.to_str()) != Some("json") {
      continue;
    }
    let Ok(bytes) = std::fs::read(&path) else { continue };
    let Ok(record) = serde_json::from_slice::<BrowserRecord>(&bytes) else {
      let _ = std::fs::remove_file(&path);
      continue;
    };
    if record.owner_pid == std::process::id() || process_is_live(record.owner_pid) {
      continue;
    }
    reclaimed += usize::from(reclaim_record(&path, &record));
  }
  reclaimed
}

fn reclaim_record(path: &std::path::Path, record: &BrowserRecord) -> bool {
  let identity = if record.browser_pid == 0 {
    Identity::NotOurs
  } else if process_is_live(record.browser_pid) {
    identify_browser(record)
  } else {
    // Already gone: nothing to signal, and its leftovers are ours to clear.
    Identity::NotOurs
  };
  if identity == Identity::Unknown {
    // Keep the record: deleting it would strand the browser it names, and
    // deleting its directories would pull the profile out from under a
    // browser that may still be running.
    return false;
  }
  if identity == Identity::Ours {
    tracing::info!(
      target: "ferridriver::process",
      browser_pid = record.browser_pid,
      owner_pid = record.owner_pid,
      "reclaiming a browser leaked by a dead ferridriver process",
    );
    kill_process_group(record.browser_pid);
  }
  let mut cleanup_complete = true;
  for dir in &record.owned_dirs {
    let dir = std::path::Path::new(dir);
    if is_reclaimable_dir(dir) {
      if let Err(error) = super::async_tempdir::removal_result(dir, std::fs::remove_dir_all(dir)) {
        tracing::warn!(%error, path = %dir.display(), "process recovery directory removal failed");
        cleanup_complete = false;
      }
    } else if !matches!(std::fs::symlink_metadata(dir), Err(error) if error.kind() == std::io::ErrorKind::NotFound) {
      cleanup_complete = false;
    }
  }
  if cleanup_complete {
    let _ = std::fs::remove_file(path);
  }
  identity == Identity::Ours
}

#[cfg(test)]
mod tests {
  use super::{STDERR_TAIL_LINES, StderrTail};

  /// The tail exists so a launch failure can quote the browser. An empty
  /// tail must stay silent rather than render an empty section.
  #[test]
  fn empty_stderr_tail_contributes_no_error_context() {
    assert!(StderrTail::default().as_error_context().is_none());
  }

  /// A chatty renderer must not grow the buffer without bound, and the
  /// lines that survive must be the MOST RECENT ones — a policy refusal is
  /// printed at startup but a crash banner is printed last.
  #[test]
  fn stderr_tail_keeps_the_last_lines_only() {
    let tail = StderrTail::default();
    for n in 0..(STDERR_TAIL_LINES * 3) {
      tail.record(format!("line {n}"));
    }
    let lines = tail.lines();
    assert_eq!(lines.len(), STDERR_TAIL_LINES);
    assert_eq!(lines.last().map(String::as_str), Some("line 59"));
    assert_eq!(lines.first().map(String::as_str), Some("line 40"));
    let context = tail.as_error_context().expect("context");
    assert!(context.starts_with("browser stderr:"));
    assert!(context.contains("line 59"));
  }

  use super::*;

  /// A pid that has exited and been reaped, so `process_is_live` is
  /// false for it. Kernel pid reuse could in principle hand it to
  /// someone else, which is exactly why the sweep also checks the
  /// command line before signalling anything.
  fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("true").spawn().expect("spawn true");
    let pid = child.id();
    let _ = child.wait();
    pid
  }

  fn write_record(owner_pid: u32, browser_pid: u32, profile: Option<&std::path::Path>, owns: bool) -> PathBuf2 {
    let dir = registry_dir().expect("registry dir");
    let record = BrowserRecord {
      owner_pid,
      browser_pid,
      profile_dir: profile.map(|p| p.to_string_lossy().into_owned()),
      owned_dirs: if owns {
        profile.map(|p| p.to_string_lossy().into_owned()).into_iter().collect()
      } else {
        Vec::new()
      },
      start_time: process_start_time(browser_pid),
    };
    let path = dir.join(format!("{owner_pid}-{browser_pid}.json"));
    write_record_file(&path, &record).expect("write record");
    path
  }

  type PathBuf2 = std::path::PathBuf;

  #[test]
  fn sweep_reclaims_the_profile_dir_of_a_dead_owner() {
    let owner = dead_pid();
    let browser = dead_pid();
    let profile = std::env::temp_dir().join(format!("ferridriver-sweep-test-{owner}"));
    std::fs::create_dir_all(profile.join("Default")).expect("profile dir");
    let record = write_record(owner, browser, Some(&profile), true);

    let _ = sweep_stale_browsers();

    assert!(!record.exists(), "the record of a dead owner is removed");
    assert!(!profile.exists(), "an owned profile dir is removed with its owner");
  }

  /// A live process group that looks like an automation browser to
  /// `browser_matches`, in its own session so signalling it cannot reach the
  /// test runner. Returns (group leader pid, non-leader child pid); the
  /// child's pgid is the leader, so it is not a group leader itself.
  fn spawn_fake_browser_group() -> (u32, std::process::Child) {
    use std::io::BufRead as _;
    use std::os::unix::process::CommandExt as _;

    let mut cmd = std::process::Command::new("sh");
    // The inner `sh` carries the automation marker in its argv and is a
    // plain child, so `pgid(child) == leader != child`.
    cmd
      .arg("-c")
      .arg("sh -c 'sleep 300; wait' --remote-debugging-port=59999 & echo $!; sleep 300; wait")
      .stdout(std::process::Stdio::piped())
      .stderr(std::process::Stdio::null());
    #[allow(unsafe_code)]
    unsafe {
      // SAFETY: `setsid` is async-signal-safe and the closure allocates
      // nothing. Own session so the group kill under test cannot reach us.
      cmd.pre_exec(|| {
        libc::setsid();
        Ok(())
      });
    }
    let mut leader = cmd.spawn().expect("spawn fake browser group");
    let mut line = String::new();
    let stdout = leader.stdout.take().expect("stdout piped");
    std::io::BufReader::new(stdout)
      .read_line(&mut line)
      .expect("read child pid");
    let child: u32 = line.trim().parse().expect("child pid");
    (child, leader)
  }

  fn kill_group(leader: &mut std::process::Child) {
    kill_process_group(leader.id());
    let _ = leader.kill();
    let _ = leader.wait();
  }

  /// `kill(pid, 0)` answers "live" for a killed-but-unreaped child, so tests
  /// that assert a process survived a sweep have to ask the kernel for its
  /// state instead.
  #[cfg(target_os = "macos")]
  fn is_running(pid: u32) -> bool {
    super::proc_bsdinfo(pid).is_some_and(|info| info.pbi_status != libc::SZOMB)
  }

  #[cfg(target_os = "linux")]
  fn is_running(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
      return false;
    };
    stat
      .rsplit_once(')')
      .and_then(|(_, rest)| rest.split_whitespace().next().map(ToString::to_string))
      .is_some_and(|state| state != "Z")
  }

  /// Give the group kill a moment to land before asking.
  fn settle() {
    std::thread::sleep(std::time::Duration::from_millis(200));
  }

  /// `killpg` addresses a group by id, and a group's id is the pid of its
  /// leader — so a recycled pid that is merely a group MEMBER of someone
  /// else's group cannot be signalled through it. Pinned, because the
  /// blast radius of getting this wrong is another session's browsers.
  #[test]
  fn sweep_never_group_kills_a_pid_that_is_not_its_own_group_leader() {
    let (non_leader, mut leader) = spawn_fake_browser_group();
    let record = write_record(dead_pid(), non_leader, None, false);

    let _ = sweep_stale_browsers();
    settle();

    let survived = is_running(non_leader);
    kill_group(&mut leader);
    let _ = std::fs::remove_file(&record);
    assert!(survived, "a non-leader pid must never be group-killed by the sweep");
  }

  /// With no profile directory to match on, the only thing separating our
  /// browser from anyone else's is the recorded start time: a recycled pid
  /// landing on an unrelated automation browser must be spared.
  #[test]
  fn sweep_spares_a_recycled_pid_whose_start_time_differs() {
    let (_, mut leader) = spawn_fake_browser_group();
    let victim = leader.id();
    let dir = registry_dir().expect("registry dir");
    let owner = dead_pid();
    let record_path = dir.join(format!("{owner}-{victim}.json"));
    let record = BrowserRecord {
      owner_pid: owner,
      browser_pid: victim,
      profile_dir: None,
      owned_dirs: Vec::new(),
      // The browser this record described started at a different time; the
      // pid has since been recycled onto someone else's browser.
      start_time: Some("Thu Jan  1 00:00:00 1970".to_string()),
    };
    write_record_file(&record_path, &record).expect("write record");

    let _ = sweep_stale_browsers();
    settle();

    let survived = is_running(victim);
    kill_group(&mut leader);
    let _ = std::fs::remove_file(&record_path);
    assert!(survived, "a pid whose start time does not match the record is not ours");
  }

  #[test]
  fn sweep_kills_a_recorded_browser_whose_start_time_matches() {
    let (_, mut leader) = spawn_fake_browser_group();
    let victim = leader.id();
    let dir = registry_dir().expect("registry dir");
    let owner = dead_pid();
    let record_path = dir.join(format!("{owner}-{victim}.json"));
    let record = BrowserRecord {
      owner_pid: owner,
      browser_pid: victim,
      profile_dir: None,
      owned_dirs: Vec::new(),
      start_time: process_start_time(victim),
    };
    assert!(record.start_time.is_some(), "ps reports a start time");
    write_record_file(&record_path, &record).expect("write record");

    let _ = sweep_stale_browsers();
    settle();

    let killed = !is_running(victim);
    kill_group(&mut leader);
    let _ = std::fs::remove_file(&record_path);
    assert!(killed, "our own leaked browser is still reclaimed");
  }

  #[test]
  fn sweep_keeps_a_profile_dir_it_does_not_own() {
    let owner = dead_pid();
    let browser = dead_pid();
    let profile = std::env::temp_dir().join(format!("ferridriver-sweep-keep-{owner}"));
    std::fs::create_dir_all(&profile).expect("profile dir");
    let record = write_record(owner, browser, Some(&profile), false);

    let _ = sweep_stale_browsers();

    assert!(!record.exists(), "the record is still removed");
    assert!(
      profile.exists(),
      "a caller-supplied persistent profile is never deleted"
    );
    let _ = std::fs::remove_dir_all(&profile);
  }

  #[test]
  fn sweep_leaves_records_of_live_owners_alone() {
    let profile = std::env::temp_dir().join(format!("ferridriver-sweep-live-{}", std::process::id()));
    std::fs::create_dir_all(&profile).expect("profile dir");
    // Our own pid is live by definition, so this record must survive.
    let record = write_record(std::process::id(), dead_pid(), Some(&profile), true);

    let _ = sweep_stale_browsers();

    assert!(record.exists(), "a live owner's browser is not reclaimed");
    assert!(profile.exists(), "a live owner keeps its profile dir");
    let _ = std::fs::remove_file(&record);
    let _ = std::fs::remove_dir_all(&profile);
  }

  #[test]
  fn cleanup_only_records_survive_failed_removal_and_are_reclaimed_on_retry() {
    let root = tempfile::tempdir().expect("owned fixture");
    let path = root.path().join("profile");
    std::fs::write(&path, b"sashoush").expect("removal failure fixture");
    let record = write_record(std::process::id(), 0, Some(&path), true);
    let metadata: BrowserRecord = serde_json::from_slice(&std::fs::read(&record).expect("record")).expect("metadata");
    assert!(!reclaim_record(&record, &metadata));
    assert!(record.exists(), "failed directory removal keeps recovery metadata");
    assert!(path.exists());
    std::fs::remove_file(&path).expect("repair fixture");
    std::fs::create_dir(&path).expect("repair directory");
    assert!(!reclaim_record(&record, &metadata));
    assert!(!record.exists());
    assert!(!path.exists());
  }

  #[test]
  fn a_record_is_written_at_launch_and_removed_on_shutdown() {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    rt.block_on(async {
      let profile = std::env::temp_dir().join(format!("ferridriver-record-test-{}", std::process::id()));
      std::fs::create_dir_all(&profile).expect("profile dir");
      let child = tokio::process::Command::new("sleep")
        .arg("30")
        .kill_on_drop(true)
        .spawn()
        .expect("spawn sleep");
      let pid = child.id().expect("pid");
      let mut group = ChildGroup::recorded(child, Some(&profile), false);
      let path = registry_dir()
        .expect("registry")
        .join(format!("{owner}-{pid}.json", owner = std::process::id()));
      assert!(path.exists(), "launch writes a record");
      assert!(group.is_running(), "the child is alive");

      group.shutdown().await.expect("shutdown");
      assert!(!path.exists(), "teardown removes the record");
      assert!(!group.is_running(), "the child is reaped");
      let _ = std::fs::remove_dir_all(&profile);
    });
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn observing_exit_keeps_the_leader_waitable_until_group_cleanup() {
    let child = tokio::process::Command::new("sh")
      .args(["-c", "exit 7"])
      .process_group(0)
      .kill_on_drop(true)
      .spawn()
      .expect("spawn leader");
    let mut group = ChildGroup::unregistered(child);
    tokio::time::timeout(std::time::Duration::from_secs(5), group.observe_exit())
      .await
      .expect("leader exits")
      .expect("observe exit");
    assert!(!group.is_running());
    assert!(!group.is_running());
    assert!(group.exit_observed().expect("leader is still waitable"));
    assert_eq!(group.wait().await.expect("reap leader").code(), Some(7));
    assert!(group.child.id().is_none());
    group.terminate().expect("completed termination is harmless");
    assert_eq!(group.wait().await.expect("repeat wait").code(), Some(7));
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn failed_directory_cleanup_preserves_the_record_and_unreaped_child_for_retry() {
    let root = tempfile::tempdir().expect("owned directory fixture");
    let path = root.path().join("profile");
    std::fs::write(&path, b"sashoush").expect("failed removal fixture");
    let child = tokio::process::Command::new("sh")
      .args(["-c", "exit 7"])
      .process_group(0)
      .kill_on_drop(true)
      .spawn()
      .expect("spawn leader");
    let mut group = ChildGroup::new(child);
    group.own_dir(&path);
    let record = group
      .record
      .as_ref()
      .expect("record")
      .path
      .clone()
      .expect("record path");
    assert!(group.wait().await.is_err(), "a file cannot be removed as a directory");
    assert!(!record.exists(), "PID authority is retired separately");
    let directories = std::sync::Arc::clone(&group.record.as_ref().expect("record").owned_dirs);
    let recovery = directories
      .lock()
      .expect("directories")
      .recovery_path
      .clone()
      .expect("recovery path");
    assert!(recovery.exists(), "failed cleanup preserves recovery metadata");
    assert!(group.exit_observed().expect("leader remains waitable"));
    assert_eq!(directories.lock().expect("directories").paths, vec![path.clone()]);
    std::fs::remove_file(&path).expect("repair fixture");
    std::fs::create_dir(&path).expect("repair directory");
    assert_eq!(group.wait().await.expect("retry cleanup").code(), Some(7));
    assert!(!path.exists());
    assert!(!record.exists());
  }

  #[cfg(unix)]
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn cancelled_directory_cleanup_keeps_its_worker_and_record_until_retry() {
    let root = tempfile::tempdir().expect("owned directory fixture");
    let path = root.path().join("profile");
    std::fs::create_dir(&path).expect("owned directory");
    let child = tokio::process::Command::new("sh")
      .args(["-c", "exit 0"])
      .process_group(0)
      .kill_on_drop(true)
      .spawn()
      .expect("spawn leader");
    let mut group = ChildGroup::new(child);
    group.own_dir(&path);
    let record = group
      .record
      .as_ref()
      .expect("record")
      .path
      .clone()
      .expect("record path");
    group.retire_registration().expect("retire process identity");
    let directories = std::sync::Arc::clone(&group.record.as_ref().expect("record").owned_dirs);
    let recovery = directories
      .lock()
      .expect("directories")
      .recovery_path
      .clone()
      .expect("recovery path");
    let (release, released) = std::sync::mpsc::channel();
    let (started, running) = tokio::sync::oneshot::channel();
    group.directory_cleanup = Some(tokio::task::spawn_blocking(move || {
      started.send(()).expect("worker ready");
      released.recv().expect("release cleanup");
      directories.lock().expect("directories").remove()
    }));
    tokio::select! {
      result = group.wait() => panic!("wait finished before cleanup was released: {result:?}"),
      result = running => result.expect("cleanup started"),
    }
    assert!(group.directory_cleanup.is_some());
    assert!(!record.exists());
    assert!(recovery.exists());
    release.send(()).expect("release worker");
    group.wait().await.expect("retry awaits retained worker");
    assert!(!record.exists());
    assert!(!path.exists());
    assert!(!recovery.exists());
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn leader_exit_cleans_descendants_holding_its_output_open() {
    use tokio::io::AsyncReadExt;
    let mut child = tokio::process::Command::new("sh")
      .args(["-c", "sleep 30 & printf ready; exit 7"])
      .process_group(0)
      .stdout(std::process::Stdio::piped())
      .kill_on_drop(true)
      .spawn()
      .expect("spawn leader and descendant");
    let mut output = child.stdout.take().expect("output");
    let mut group = ChildGroup::unregistered(child);
    let mut bytes = Vec::new();
    let (status, _) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
      tokio::try_join!(group.wait(), output.read_to_end(&mut bytes))
    })
    .await
    .expect("descendant pipe closes without waiting for sleep")
    .expect("wait and drain");
    assert_eq!(status.code(), Some(7));
    assert_eq!(bytes, b"ready");
  }

  #[cfg(unix)]
  #[tokio::test]
  async fn dropping_an_observed_exited_leader_cleans_its_descendants() {
    use tokio::io::AsyncReadExt;
    let mut child = tokio::process::Command::new("sh")
      .args(["-c", "sleep 30 & printf ready; exit 0"])
      .process_group(0)
      .stdout(std::process::Stdio::piped())
      .kill_on_drop(true)
      .spawn()
      .expect("spawn leader and descendant");
    let mut output = child.stdout.take().expect("output");
    let mut group = ChildGroup::unregistered(child);
    tokio::time::timeout(std::time::Duration::from_secs(5), group.observe_exit())
      .await
      .expect("leader exits")
      .expect("observe exit");
    assert!(!group.is_running());
    drop(group);
    let mut bytes = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(5), output.read_to_end(&mut bytes))
      .await
      .expect("drop closes descendant pipe")
      .expect("read output");
    assert_eq!(bytes, b"ready");
  }
  #[cfg(unix)]
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn dropping_during_directory_cleanup_leaves_the_worker_as_sole_owner() {
    let root = tempfile::tempdir().expect("owned fixture");
    let path = root.path().join("profile");
    std::fs::create_dir(&path).expect("owned profile");
    let child = tokio::process::Command::new("sh")
      .args(["-c", "exit 0"])
      .process_group(0)
      .kill_on_drop(true)
      .spawn()
      .expect("leader");
    let mut group = ChildGroup::new(child);
    group.own_dir(&path);
    group.observe_exit().await.expect("leader exited");
    group.terminate().expect("terminate descendants");
    group.retire_registration().expect("retire PID authority");
    let directories = std::sync::Arc::clone(&group.record.as_ref().expect("record").owned_dirs);
    let recovery = directories
      .lock()
      .expect("directories")
      .recovery_path
      .clone()
      .expect("recovery record");
    let record: BrowserRecord =
      serde_json::from_slice(&std::fs::read(&recovery).expect("recovery bytes")).expect("record");
    assert_eq!(record.browser_pid, 0);
    let (release, released) = std::sync::mpsc::channel();
    let (started, running) = tokio::sync::oneshot::channel();
    let (finished, done) = tokio::sync::oneshot::channel();
    group.directory_cleanup = Some(tokio::task::spawn_blocking(move || {
      started.send(()).expect("started");
      released.recv().expect("release");
      let result = directories.lock().expect("directories").remove();
      finished.send(()).expect("finished");
      result
    }));
    running.await.expect("worker is blocked");
    drop(group);
    assert!(path.exists(), "drop must not start a competing removal");
    assert!(recovery.exists(), "in-flight cleanup retains its record");
    release.send(()).expect("release");
    done.await.expect("cleanup finishes after owner drop");
    assert!(!path.exists());
    assert!(!recovery.exists());
  }
}

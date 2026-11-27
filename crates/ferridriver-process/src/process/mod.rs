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

/// Run `spawn`, which starts a child process, while no other child of
/// this process is being started.
///
/// Where the platform has no `pipe2` (macOS), std opens the pipes a
/// spawn needs and marks them close-on-exec in a second step. A child
/// another thread forks in between inherits them. One of those pipes is
/// how `Command::spawn` learns its own child reached `exec`: it reads
/// until every writer is gone, so a browser that inherited the write end
/// keeps a second browser's launch reading for as long as it lives. A
/// pool of workers launching together hit that in a few runs out of a
/// hundred, and nothing bounds the read.
pub fn spawn_exclusive<T>(spawn: impl FnOnce() -> T) -> T {
  static STARTING: std::sync::Mutex<()> = std::sync::Mutex::new(());
  let _starting = STARTING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
  spawn()
}

/// Run a subprocess to completion, optionally piping `input` to stdin and enforcing `timeout`.
///
/// # Errors
///
/// Returns an error if spawning fails, I/O fails, the timeout expires, or the process exits non-zero.
pub async fn output(
  mut command: tokio::process::Command,
  input: Option<&[u8]>,
  timeout: Option<std::time::Duration>,
) -> ferridriver_error::Result<String> {
  use std::process::Stdio;

  use tokio::io::{AsyncReadExt, AsyncWriteExt};

  use ferridriver_error::FerriError;
  let program = command.as_std().get_program().to_string_lossy().into_owned();
  command.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
  command.stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
  #[cfg(unix)]
  command.process_group(0);
  let mut child =
    spawn_exclusive(|| command.spawn()).map_err(|error| FerriError::backend(format!("starting {program}: {error}")))?;
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
/// session leader, which is fine for tests. `setsid` is async-signal-safe
/// per POSIX.1-2017, and the closure allocates nothing and captures nothing.
#[cfg(unix)]
pub fn setsid_pre_exec() -> impl FnMut() -> std::io::Result<()> + Send + Sync + 'static {
  || {
    // EPERM means the child already leads a session, which is benign.
    rustix::process::setsid().ok();
    Ok(())
  }
}

/// Make `command`'s child the leader of a new session and process group,
/// through [`setsid_pre_exec`]. Pass a tokio command's `as_std_mut()`.
#[cfg(unix)]
#[expect(
  unsafe_code,
  reason = "std's safe `CommandExt::setsid` is unstable, and `pre_exec` is the only stable way to run setsid in the child"
)]
pub fn spawn_in_own_session(command: &mut std::process::Command) {
  use std::os::unix::process::CommandExt as _;
  // SAFETY: the hook runs in the child between fork and exec, where only
  // async-signal-safe work is sound. `setsid_pre_exec` calls setsid(2)
  // alone, allocates nothing, and shares no state with the parent.
  unsafe {
    command.pre_exec(setsid_pre_exec());
  }
}

/// A command line run through the platform shell, as Node's `shell: true`
/// does: `sh -c <line>` on Unix, `cmd.exe /d /s /c "<line>"` on Windows.
/// The Windows line is passed verbatim, since `cmd` does not parse its
/// command line the way the C runtime quoting `Command` applies expects.
#[must_use]
pub fn shell_command(line: &str) -> std::process::Command {
  #[cfg(unix)]
  {
    let mut command = std::process::Command::new("sh");
    command.args(["-c", line]);
    command
  }
  #[cfg(windows)]
  {
    use std::os::windows::process::CommandExt as _;
    let mut command = std::process::Command::new("cmd.exe");
    command.args(["/d", "/s", "/c"]).raw_arg(format!("\"{line}\""));
    command
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

pub fn drain_child_output(child: &mut tokio::process::Child) -> StderrTail {
  let tail = drain_child_stderr(child);
  if let Some(stdout) = child.stdout.take() {
    drain_stream(stdout, tail.clone(), "stdout");
  }
  tail
}

/// [`drain_child_output`] for a child that writes its output to the
/// write end of `reader`'s pipe rather than to tokio's.
#[cfg(windows)]
#[must_use]
pub fn drain_pipe(reader: std::io::PipeReader) -> StderrTail {
  let tail = StderrTail::default();
  let file = std::fs::File::from(std::os::windows::io::OwnedHandle::from(reader));
  drain_stream(tokio::fs::File::from_std(file), tail.clone(), "output");
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
  pub fn record(&self, line: String) {
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
pub fn kill_process_group(pid: u32) {
  // Pid 0 would address our own group; a pid past `i32::MAX` names no
  // process. Neither is a browser to kill.
  let Some(group) = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw) else {
    return;
  };
  rustix::process::kill_process_group(group, rustix::process::Signal::KILL).ok();
}

/// Windows has no process groups to signal: `taskkill /T` walks the tree
/// from `pid`, as Playwright's `killProcessTree` does, so a browser's
/// renderer and GPU processes go with it.
#[cfg(windows)]
pub fn kill_process_group(pid: u32) {
  use std::os::windows::process::CommandExt as _;
  const CREATE_NO_WINDOW: u32 = 0x0800_0000;
  let _ = std::process::Command::new("taskkill")
    .args(["/pid", &pid.to_string(), "/T", "/F"])
    .creation_flags(CREATE_NO_WINDOW)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status();
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
  child: OwnedChild,
  registered: bool,
  directory_cleanup: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
  /// On-disk record of this browser, removed once the process is
  /// killed. Left behind when our own process dies without running
  /// `Drop` (SIGKILL, panic-abort) — exactly the case
  /// [`sweep_stale_browsers`] reclaims on the next start.
  record: Option<ProcRecord>,
  /// Windows' stand-in for the process group: everything the child
  /// starts joins its job, so terminating the job reaches what a group
  /// kill would, even once the child itself has exited.
  #[cfg(windows)]
  job: Option<job::ProcessJob>,
}

/// The child a [`ChildGroup`] owns: one tokio spawned, or on Windows one
/// spawned with C runtime descriptors past stdio, which `Command` cannot.
pub enum OwnedChild {
  Tokio(Box<tokio::process::Child>),
  #[cfg(windows)]
  Raw(super::windows_spawn::RawChild),
}

impl From<tokio::process::Child> for OwnedChild {
  fn from(child: tokio::process::Child) -> Self {
    Self::Tokio(Box::new(child))
  }
}

#[cfg(windows)]
impl From<super::windows_spawn::RawChild> for OwnedChild {
  fn from(child: super::windows_spawn::RawChild) -> Self {
    Self::Raw(child)
  }
}

impl OwnedChild {
  /// The pid, until the child's exit has been observed.
  #[must_use]
  pub fn id(&self) -> Option<u32> {
    match self {
      Self::Tokio(child) => child.id(),
      #[cfg(windows)]
      Self::Raw(child) => child.id(),
    }
  }

  #[cfg(not(unix))]
  fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
    match self {
      Self::Tokio(child) => child.try_wait(),
      #[cfg(windows)]
      Self::Raw(child) => child.try_wait(),
    }
  }

  /// # Errors
  /// Fails when the child is running and refuses to end.
  pub fn start_kill(&mut self) -> std::io::Result<()> {
    match self {
      Self::Tokio(child) => child.start_kill(),
      #[cfg(windows)]
      Self::Raw(child) => child.start_kill(),
    }
  }

  /// # Errors
  /// Fails when the system cannot report on the child.
  pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
    match self {
      Self::Tokio(child) => child.wait().await,
      #[cfg(windows)]
      Self::Raw(child) => child.wait().await,
    }
  }
}

/// Whether our child `pid` has exited, without reaping it: the exit
/// status stays for its owner to collect.
///
/// # Errors
/// Fails when `pid` is not a waitable child of this process.
#[cfg(unix)]
pub fn child_exited(pid: u32) -> std::io::Result<bool> {
  use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
  let pid = Pid::from_raw(i32::try_from(pid).map_err(std::io::Error::other)?)
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

/// Whether `pid` has exited. Windows keeps no unreaped state to read
/// without disturbing, so this asks whether the process is still running.
///
/// # Errors
/// Fails when the process table cannot be read.
#[cfg(windows)]
pub fn child_exited(pid: u32) -> std::io::Result<bool> {
  windows_process(pid, |process| process.is_none())
    .ok_or_else(|| std::io::Error::other("cannot read the process table"))
}

impl ChildGroup {
  #[must_use]
  pub fn new(child: impl Into<OwnedChild>) -> Self {
    Self::recorded(child, None, false)
  }

  /// Own a command without adding it to the browser recovery registry.
  #[must_use]
  pub fn unregistered(child: impl Into<OwnedChild>) -> Self {
    let child = child.into();
    let pid = child.id().unwrap_or(0);
    Self {
      pid,
      child,
      registered: false,
      directory_cleanup: None,
      record: None,
      #[cfg(windows)]
      job: job::claim(pid),
    }
  }

  /// Like [`Self::new`], but also writes a launch record so a later run
  /// can reclaim this browser if our process dies without teardown.
  /// `profile_dir` is the browser's `--user-data-dir` / `--profile`;
  /// `owns_profile_dir` marks it as ours to delete (a temp dir), false
  /// for a caller-supplied persistent directory.
  #[must_use]
  pub fn recorded(child: impl Into<OwnedChild>, profile_dir: Option<&std::path::Path>, owns_profile_dir: bool) -> Self {
    let child = child.into();
    // `id()` is `None` only after the child has been polled to
    // completion; fresh children always have an id.
    let pid = child.id().unwrap_or(0);
    let record = if pid == 0 {
      None
    } else {
      // Two layers, because they cover different deaths: the record
      // survives a hard kill and is reclaimed by the next start, the
      // watchdog (on Windows, the job) acts immediately.
      #[cfg(unix)]
      super::reaper::watch(pid);
      ProcRecord::write(pid, profile_dir, owns_profile_dir)
    };
    Self {
      pid,
      child,
      registered: pid != 0,
      directory_cleanup: None,
      record,
      #[cfg(windows)]
      job: job::claim(pid),
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
  #[cfg_attr(
    unix,
    expect(
      clippy::needless_pass_by_ref_mut,
      reason = "non-Unix path calls Child::try_wait(&mut self)"
    )
  )]
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
    child_exited(self.pid)
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
      #[cfg(unix)]
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
    #[cfg(windows)]
    match &self.job {
      Some(job) => job.terminate()?,
      // A browser Windows would not put in a job: walk its tree instead.
      None => kill_process_group(self.pid),
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
      // The leader is gone; what it started may not be.
      #[cfg(windows)]
      {
        self.child.wait().await?;
        if let Some(job) = &self.job {
          job.terminate()?;
          job.until_empty(std::time::Duration::from_secs(10)).await?;
        }
      }
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
  ///
  /// # Errors
  ///
  /// Returns an error message if waiting fails or the child is still running past `timeout`.
  pub async fn wait_for_exit(&mut self, timeout: std::time::Duration) -> Result<(), String> {
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

  /// [`Self::shutdown`] for a browser's `close()`, which Playwright's
  /// `processLauncher` never fails over cleanup: the error is logged. What
  /// could not be removed (a profile file Windows still reports in use)
  /// stays owned, so the group's drop tries again and the retained
  /// recovery record lets the next sweep reclaim it.
  pub async fn shutdown_for_close(&mut self) {
    if let Err(error) = self.shutdown().await {
      tracing::warn!(%error, "browser shutdown left cleanup for a later attempt");
    }
  }
}

impl Drop for ChildGroup {
  fn drop(&mut self) {
    if let Err(error) = self.terminate() {
      tracing::warn!(%error, "failed to clean up owned process group during drop");
    }
    #[cfg(unix)]
    if self.registered {
      super::reaper::forget(self.pid);
    }
    // Retire the PID record before the child field drops. An active directory
    // worker retains its separate recovery record without that PID authority.
    self.record.take();
  }
}

mod inspect;
mod registry;
#[cfg(test)]
mod tests;

pub use inspect::PROFILE_FLUSH_TIMEOUT;
#[cfg(target_os = "linux")]
pub use inspect::describe_process;
#[cfg(target_os = "macos")]
use inspect::process_group_exiting_settled;
pub use inspect::process_is_live;
#[cfg(any(test, feature = "test-support"))]
pub use inspect::test_sleeper;
#[cfg(all(unix, any(test, feature = "test-support")))]
pub use inspect::test_support;
#[cfg(windows)]
use inspect::windows_process;
#[cfg(windows)]
use registry::job;
pub use registry::{ProcRecord, sweep_stale_browsers, track_spawned};

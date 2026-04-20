//! Shared browser-process lifecycle helpers.
//!
//! Chrome/Firefox spawn a pool of subprocesses (renderer, GPU, utility,
//! zygote). When the parent dies via SIGKILL — as happens when a test
//! harness panics or `kill_on_drop(true)` fires — those helpers are
//! supposed to notice the parent IPC pipe closing and exit on their own.
//! In practice, on macOS this is flaky: helpers can linger for seconds
//! or get stuck, showing up as "automation Chrome zombies" in tools like
//! `box-dev-gate browser zombies` that pgrep `--remote-debugging` etc.
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

/// Send `SIGKILL` to every process in the given pid's process group.
///
/// Assumes the target was spawned with [`setsid_pre_exec`], so its
/// `pgid == pid`. Failures are logged at `debug` level and ignored —
/// the common cases are ESRCH (group already dead) and EPERM (we don't
/// own the group), neither of which is actionable.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn kill_process_group(pid: u32) {
  // Cast is safe: Chrome PIDs fit in i32 on every platform we target.
  #[allow(clippy::cast_possible_wrap)]
  let group_id = pid as i32;
  // SAFETY: `killpg` is async-signal-safe. `SIGKILL` is always
  // deliverable. Even if `group_id` is stale (reused by another
  // process), the worst case is we no-op because we don't own the
  // new group.
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
/// `box-dev-gate browser zombies`.
///
/// The inner `Child` still has `kill_on_drop(true)` set, so the parent
/// PID is also killed directly (belt + suspenders). The group kill
/// runs first because fields drop in declaration order.
pub struct ChildGroup {
  pid: u32,
  child: tokio::process::Child,
}

impl ChildGroup {
  #[must_use]
  pub fn new(child: tokio::process::Child) -> Self {
    // `id()` is `None` only after the child has been polled to
    // completion; fresh children always have an id.
    let pid = child.id().unwrap_or(0);
    Self { pid, child }
  }

  /// Access the underlying child for `kill().await`, `wait().await`,
  /// or `try_wait()`. Group kill still fires on drop regardless.
  pub fn inner_mut(&mut self) -> &mut tokio::process::Child {
    &mut self.child
  }
}

impl Drop for ChildGroup {
  fn drop(&mut self) {
    if self.pid != 0 {
      kill_process_group(self.pid);
    }
  }
}

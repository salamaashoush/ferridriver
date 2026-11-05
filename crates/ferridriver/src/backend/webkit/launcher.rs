//! Locate and spawn the Playwright `WebKit` binary with
//! `--inspector-pipe`. The child opens fd 3 + fd 4 for the pipe
//! transport; we dup our socketpair halves into place before exec so
//! `pw_run.sh` inherits them naturally.
//!
//! Binary discovery: `FERRIDRIVER_WEBKIT` env var first, then
//! ferridriver's own cache (populated by `ferridriver install webkit`),
//! then Playwright's browser cache.

use crate::backend::process::ChildGroup;
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

/// Knobs for [`spawn`]. `headless` toggles `--headless`; `user_data_dir`
/// switches the launch to persistent-context mode (passes
/// `--user-data-dir=...` and drops `--no-startup-window`).
#[derive(Debug, Default, Clone)]
pub struct LaunchConfig {
  pub executable_path: Option<PathBuf>,
  pub headless: bool,
  /// Extra environment for the `pw_run.sh` child, merged onto the
  /// inherited environment.
  pub env: rustc_hash::FxHashMap<String, String>,
  pub user_data_dir: Option<PathBuf>,
  pub proxy_server: Option<String>,
  pub proxy_bypass_list: Option<String>,
  pub extra_args: Vec<String>,
}

#[derive(Debug, Error)]
pub enum LaunchError {
  #[error(
    "Playwright WebKit binary not found in any of: FERRIDRIVER_WEBKIT, ferridriver cache, Playwright cache. Run `ferridriver install webkit` to download it."
  )]
  BinaryNotFound,
  #[error("io: {0}")]
  Io(#[from] std::io::Error),
}

/// Resolve the `pw_run.sh` (or platform-equivalent) path. Returns the
/// first existing candidate from the search order.
pub fn locate_binary() -> Result<PathBuf, LaunchError> {
  if let Ok(path) = std::env::var("FERRIDRIVER_WEBKIT") {
    let p = PathBuf::from(path);
    if p.is_file() {
      return Ok(p);
    }
  }
  crate::install::BrowserInstaller::new()
    .find_installed_webkit()
    .map(PathBuf::from)
    .or_else(|| crate::install::find_playwright_browser(crate::install::CachedBrowser::WebKit))
    .ok_or(LaunchError::BinaryNotFound)
}

/// The PW `WebKit` build revision parsed from the located binary's
/// parent directory (`.../webkit-2272/pw_run.sh` → `"2272"`). Falls
/// back to `"unknown"` when the binary can't be located or the
/// directory isn't revision-named — never panics.
#[must_use]
pub fn binary_revision() -> String {
  let Ok(path) = locate_binary() else {
    return "unknown".to_string();
  };
  revision_from_path(&path)
}

pub(crate) fn revision_from_path(path: &Path) -> String {
  path
    .parent()
    .and_then(Path::file_name)
    .and_then(|n| n.to_str())
    .and_then(|n| n.strip_prefix("webkit-"))
    .unwrap_or("unknown")
    .to_string()
}

/// Spawn the Playwright `WebKit` child with `--inspector-pipe`.
///
/// `read_fd` is the fd the child should write outbound messages to
/// (becomes fd 4 inside the child after `dup2`). `write_fd` is the fd
/// the child should read from (becomes fd 3 inside the child). The
/// caller is responsible for keeping ownership of the parent ends of
/// the pipe; this function only borrows the descriptors long enough to
/// dup them into the child's environment.
#[cfg(unix)]
pub fn spawn(binary: &Path, config: &LaunchConfig, read_fd: i32, write_fd: i32) -> Result<ChildGroup, LaunchError> {
  let mut cmd = Command::new(binary);
  cmd.envs(&config.env);
  cmd.arg("--inspector-pipe");
  if config.headless {
    cmd.arg("--headless");
  }
  if let Some(ref dir) = config.user_data_dir {
    crate::backend::ensure_user_data_dir(Some(&dir.to_string_lossy()))
      .map_err(|e| LaunchError::Io(std::io::Error::other(e.to_string())))?;
    cmd.arg(format!("--user-data-dir={}", dir.display()));
  } else {
    cmd.arg("--no-startup-window");
  }
  if let Some(ref proxy) = config.proxy_server {
    cmd.arg(format!("--proxy={proxy}"));
  }
  if let Some(ref bypass) = config.proxy_bypass_list {
    if cfg!(target_os = "linux") {
      cmd.args(bypass.split(',').map(|host| format!("--ignore-host={host}")));
    } else {
      cmd.arg(format!("--proxy-bypass-list={bypass}"));
    }
  }
  for arg in &config.extra_args {
    cmd.arg(arg);
  }

  // The child expects fd 3 (its read end) and fd 4 (its write end).
  // SAFETY: the dup2 + fcntl calls happen post-fork in `pre_exec`
  // before any other thread can observe the descriptors. We do NOT
  // close `read_fd` / `write_fd` here — the parent owns those.
  #[allow(unsafe_code)]
  unsafe {
    use std::os::unix::process::CommandExt;
    cmd.pre_exec(move || {
      // Own session + process group: `pw_run.sh` is a shell wrapper, so
      // killing the direct child would leave the real WebKit process
      // and its helpers behind. `crate::backend::process::kill_process_group`
      // takes the whole group down on teardown instead.
      if libc::setsid() == -1 {
        return Err(std::io::Error::last_os_error());
      }
      pre_exec_setup_fds(read_fd, write_fd)
    });
  }

  tracing::debug!(
    target: "ferridriver::browser::launch",
    binary = %binary.display(),
    args = ?cmd.get_args().collect::<Vec<_>>(),
    "spawning webkit"
  );
  let child =
    crate::backend::process::spawn_exclusive(|| tokio::process::Command::from(cmd).kill_on_drop(true).spawn())?;
  Ok(ChildGroup::recorded(child, config.user_data_dir.as_deref(), false))
}

/// Windows stub. The `--inspector-pipe` transport relies on `pre_exec`
/// to dup fd 3 / fd 4 into the child before the binary starts; Windows
/// has no fork model and PW WebKit does not ship a usable Windows
/// inspector-pipe binary today either. Return a typed error so callers
/// surface "Unsupported" instead of a build failure.
#[cfg(not(unix))]
pub fn spawn(_config: &LaunchConfig, _read_fd: i32, _write_fd: i32) -> Result<ChildGroup, LaunchError> {
  Err(LaunchError::Io(std::io::Error::other(
    "webkit launcher: --inspector-pipe spawn is not supported on this platform yet",
  )))
}

#[cfg(unix)]
fn pre_exec_setup_fds(read_fd: i32, write_fd: i32) -> std::io::Result<()> {
  // Child reads its input from fd 3 (parent's write_fd) and writes its
  // output to fd 4 (parent's read_fd). Order matters: write_fd → 3
  // first, read_fd → 4 second; if we did it the other way around and
  // write_fd happened to be 4, the first dup2 would clobber it.
  // SAFETY: post-fork, single-threaded child.
  #[allow(unsafe_code)]
  unsafe {
    if libc::dup2(write_fd, 3) == -1 {
      return Err(std::io::Error::last_os_error());
    }
    if libc::dup2(read_fd, 4) == -1 {
      return Err(std::io::Error::last_os_error());
    }
    // Clear FD_CLOEXEC on fds 3 and 4 so they survive the exec.
    for fd in [3i32, 4] {
      let flags = libc::fcntl(fd, libc::F_GETFD);
      if flags != -1 {
        libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
      }
    }
  }
  Ok(())
}

#[cfg(not(unix))]
fn pre_exec_setup_fds(_read_fd: i32, _write_fd: i32) -> std::io::Result<()> {
  Err(std::io::Error::other("webkit launcher: non-unix not yet supported"))
}

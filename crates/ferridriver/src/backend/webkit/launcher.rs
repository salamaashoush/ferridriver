//! Locate and spawn the Playwright `WebKit` binary with
//! `--inspector-pipe`. The child opens fd 3 + fd 4 for the pipe
//! transport; we dup our socketpair halves into place before exec so
//! `pw_run.sh` inherits them naturally.
//!
//! Binary discovery: `FERRIDRIVER_WEBKIT` env var first, then the
//! Playwright Node.js cache, then ferridriver's own cache (populated
//! by `ferridriver install webkit`).

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use thiserror::Error;

/// Knobs for [`spawn`]. `headless` toggles `--headless`; `user_data_dir`
/// switches the launch to persistent-context mode (passes
/// `--user-data-dir=...` and drops `--no-startup-window`).
#[derive(Debug, Default, Clone)]
pub struct LaunchConfig {
  pub headless: bool,
  pub user_data_dir: Option<PathBuf>,
  pub proxy_server: Option<String>,
  pub proxy_bypass_list: Option<String>,
  pub extra_args: Vec<String>,
}

#[derive(Debug, Error)]
pub enum LaunchError {
  #[error(
    "Playwright WebKit binary not found in any of: FERRIDRIVER_WEBKIT, Playwright cache, ferridriver cache. Run `ferridriver install webkit` to download it."
  )]
  BinaryNotFound,
  #[error("io: {0}")]
  Io(#[from] std::io::Error),
}

const BINARY_RELATIVE: &str = "pw_run.sh";

/// Resolve the `pw_run.sh` (or platform-equivalent) path. Returns the
/// first existing candidate from the search order.
pub fn locate_binary() -> Result<PathBuf, LaunchError> {
  if let Ok(path) = std::env::var("FERRIDRIVER_WEBKIT") {
    let p = PathBuf::from(path);
    if p.is_file() {
      return Ok(p);
    }
  }
  for root in playwright_caches() {
    if let Some(found) = newest_pw_run(&root) {
      return Ok(found);
    }
  }
  Err(LaunchError::BinaryNotFound)
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
pub fn spawn(config: &LaunchConfig, read_fd: i32, write_fd: i32) -> Result<Child, LaunchError> {
  let binary = locate_binary()?;
  let mut cmd = Command::new(&binary);
  cmd.arg("--inspector-pipe");
  if config.headless {
    cmd.arg("--headless");
  }
  if let Some(ref dir) = config.user_data_dir {
    cmd.arg(format!("--user-data-dir={}", dir.display()));
  } else {
    cmd.arg("--no-startup-window");
  }
  if let Some(ref proxy) = config.proxy_server {
    cmd.arg(format!("--proxy={proxy}"));
  }
  if let Some(ref bypass) = config.proxy_bypass_list {
    cmd.arg(format!("--proxy-bypass-list={bypass}"));
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
    cmd.pre_exec(move || pre_exec_setup_fds(read_fd, write_fd));
  }

  let child = cmd.spawn()?;
  Ok(child)
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

fn playwright_caches() -> Vec<PathBuf> {
  let mut out = Vec::new();
  if let Some(home) = dirs::home_dir() {
    // Playwright Node.js cache (macOS + Linux + Windows variants).
    #[cfg(target_os = "macos")]
    out.push(home.join("Library/Caches/ms-playwright"));
    #[cfg(target_os = "linux")]
    out.push(home.join(".cache/ms-playwright"));
    #[cfg(target_os = "windows")]
    out.push(home.join("AppData/Local/ms-playwright"));
  }
  // ferridriver's own cache (populated by `install webkit`). Mirrors
  // BrowserInstaller::new(): respects FERRIDRIVER_BROWSERS_PATH first,
  // then dirs::cache_dir() (~/.cache on Linux, ~/Library/Caches on
  // macOS, %LOCALAPPDATA% on Windows) joined with `ferridriver/webkit`.
  let ferri_cache = if let Ok(p) = std::env::var("FERRIDRIVER_BROWSERS_PATH") {
    PathBuf::from(p)
  } else {
    dirs::cache_dir()
      .unwrap_or_else(|| PathBuf::from(".cache"))
      .join("ferridriver")
  };
  out.push(ferri_cache.join("webkit"));
  out
}

fn newest_pw_run(root: &Path) -> Option<PathBuf> {
  if !root.is_dir() {
    return None;
  }
  let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
  for entry in std::fs::read_dir(root).ok()?.flatten() {
    let path = entry.path();
    if !path.is_dir() {
      continue;
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !name.starts_with("webkit-") {
      continue;
    }
    let candidate = path.join(BINARY_RELATIVE);
    if !candidate.is_file() {
      continue;
    }
    let mtime = entry
      .metadata()
      .and_then(|m| m.modified())
      .unwrap_or(std::time::UNIX_EPOCH);
    match best {
      Some((_, ref best_mtime)) if mtime <= *best_mtime => {},
      _ => best = Some((candidate, mtime)),
    }
  }
  best.map(|(p, _)| p)
}

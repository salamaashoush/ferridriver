//! Platform process inspection, liveness checks, and test-support process helpers.

/// Whether `pid` names a live process. A recycled pid reads as live,
/// which keeps the sweep conservative: it never kills on a maybe.
#[cfg(unix)]
#[must_use]
pub fn process_is_live(pid: u32) -> bool {
  let Ok(pid) = i32::try_from(pid) else {
    return false;
  };
  // kill(0, 0) probes our own process group, which always exists.
  let Some(pid) = rustix::process::Pid::from_raw(pid) else {
    return true;
  };
  matches!(
    rustix::process::test_kill_process(pid),
    Ok(()) | Err(rustix::io::Errno::PERM)
  )
}

#[cfg(windows)]
#[must_use]
pub fn process_is_live(pid: u32) -> bool {
  // Unreadable reads as live, as a recycled pid does on Unix: the sweep
  // never kills on a maybe.
  windows_process(pid, |process| process.is_some()).unwrap_or(true)
}

/// Run `read` on what Windows reports about `pid`, or `None` when the
/// process table cannot be read at all.
#[cfg(windows)]
pub(super) fn windows_process<T>(pid: u32, read: impl FnOnce(Option<&sysinfo::Process>) -> T) -> Option<T> {
  use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
  let mut system = System::new().ok()?;
  let pid = Pid::from_u32(pid);
  system.refresh_processes_specifics(
    ProcessesToUpdate::Some(&[pid]),
    true,
    ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
  );
  Some(read(system.process(pid)))
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
pub(super) fn process_start_time(pid: u32) -> Option<String> {
  let info = proc_bsdinfo(pid)?;
  Some(format!("{}.{:06}", info.pbi_start_tvsec, info.pbi_start_tvusec))
}

/// `proc_pidinfo(PROC_PIDTBSDINFO)` for `pid`, or `None` when the
/// process is gone or not ours to inspect.
#[cfg(target_os = "macos")]
pub(super) fn proc_bsdinfo(pid: u32) -> Option<libc::proc_bsdinfo> {
  proc_bsdinfo_including_exited(pid, false)
}

#[cfg(target_os = "macos")]
#[expect(unsafe_code, reason = "libproc's proc_pidinfo has no safe binding in libc or rustix")]
fn proc_bsdinfo_including_exited(pid: u32, include_exited: bool) -> Option<libc::proc_bsdinfo> {
  let pid = i32::try_from(pid).ok()?;
  // SAFETY: `proc_bsdinfo` is a C struct of integers and byte arrays, for
  // which all-zero bytes are a valid value.
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
pub const PROFILE_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// XNU's `killpg` answers EPERM when no member can be signalled, which is
/// what a group whose leader is mid-exit looks like. That leader is visible
/// to neither `proc_listpgrppids` nor `proc_pidinfo` until it settles into a
/// zombie, so the snapshot taken straight after the EPERM can miss it and
/// the close failed. A few milliseconds later it is a zombie the snapshot
/// accepts; this blocks only on that error path, for at most 100ms.
#[cfg(target_os = "macos")]
pub(super) fn process_group_exiting_settled(pid: u32) -> bool {
  for _ in 0..20 {
    if process_group_exiting(pid) {
      return true;
    }
    std::thread::sleep(std::time::Duration::from_millis(5));
  }
  false
}

#[cfg(target_os = "macos")]
pub(super) fn process_group_exiting(pid: u32) -> bool {
  const PROC_FLAG_INEXIT: u32 = 4;
  // XNU killpg1 also loses exiting members before they become waitable
  // zombies. Require a complete group snapshot proving exit has begun;
  // an unknown or live member must preserve the permission error.
  let Some(members) = process_group_members(pid) else {
    return false;
  };
  !members.is_empty()
    && members.iter().all(|&member| {
      proc_bsdinfo_including_exited(member, true).is_some_and(|info| {
        info.pbi_pgid == pid && (info.pbi_status == libc::SZOMB || info.pbi_flags & PROC_FLAG_INEXIT != 0)
      })
    })
}

/// Every pid in process group `group`, or `None` when the table cannot be read.
#[cfg(target_os = "macos")]
#[expect(
  unsafe_code,
  reason = "libproc's proc_listpgrppids has no safe binding in libc or rustix"
)]
pub(super) fn process_group_members(group: u32) -> Option<Vec<u32>> {
  let group = i32::try_from(group).ok()?;
  let mut members = vec![0_i32; 16];
  loop {
    let size = i32::try_from(std::mem::size_of_val(members.as_slice())).ok()?;
    // SAFETY: libproc writes at most `size` bytes into this initialized PID
    // buffer. Its result is a PID count, checked before indexing the buffer.
    let count = usize::try_from(unsafe { libc::proc_listpgrppids(group, members.as_mut_ptr().cast(), size) }).ok()?;
    if count >= members.len() {
      let capacity = members.len().checked_mul(2)?;
      members.resize(capacity, 0);
      continue;
    }
    return Some(
      members[..count]
        .iter()
        .filter_map(|&member| u32::try_from(member).ok())
        .collect(),
    );
  }
}

/// Every pid in process group `group`, read from each `/proc/<pid>/stat`.
#[cfg(target_os = "linux")]
pub(super) fn process_group_members(group: u32) -> Option<Vec<u32>> {
  let entries = std::fs::read_dir("/proc").ok()?;
  let group = group.to_string();
  Some(
    entries
      .flatten()
      .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
      .filter(|pid| {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
          // After the parenthesised comm field: state (3), ppid (4), pgrp (5).
          stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(2))
            .is_some_and(|pgrp| pgrp == group)
        })
      })
      .collect(),
  )
}

/// `/proc/<pid>/stat` field 22 — start time in clock ticks since boot.
/// Unique per (pid, process) exactly like the macOS form; the two are
/// never compared with each other, only with a value this same function
/// produced.
#[cfg(target_os = "linux")]
pub(super) fn process_start_time(pid: u32) -> Option<String> {
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
#[expect(
  unsafe_code,
  reason = "the KERN_PROCARGS2 sysctl has no safe binding in libc or rustix"
)]
pub(super) fn process_command(pid: u32) -> Option<String> {
  let pid_arg = i32::try_from(pid).ok()?;
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

/// What a live process is doing, from procfs: its command, its scheduler
/// state, the kernel function it sleeps in, and the same for its
/// children. `None` when the process is gone or procfs is unreadable.
#[cfg(target_os = "linux")]
#[must_use]
pub fn describe_process(pid: u32) -> Option<String> {
  fn describe(pid: u32, depth: usize) -> Option<String> {
    use std::fmt::Write as _;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let state = stat.rsplit_once(')')?.1.split_whitespace().next()?.to_string();
    let command = process_command(pid).unwrap_or_default();
    let wchan = std::fs::read_to_string(format!("/proc/{pid}/wchan")).unwrap_or_default();
    let mut text = format!("{pid} state {state}");
    if !wchan.is_empty() && wchan != "0" {
      let _ = write!(text, " in {wchan}");
    }
    let _ = write!(text, " `{command}`");
    if depth < 2 {
      let children = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")).unwrap_or_default();
      let children: Vec<String> = children
        .split_whitespace()
        .filter_map(|child| describe(child.parse().ok()?, depth + 1))
        .collect();
      if !children.is_empty() {
        let _ = write!(text, ", children [{}]", children.join("; "));
      }
    }
    Some(text)
  }
  describe(pid, 0)
}

#[cfg(target_os = "linux")]
pub(super) fn process_command(pid: u32) -> Option<String> {
  let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
  let joined = raw
    .split(|b| *b == 0)
    .filter(|arg| !arg.is_empty())
    .map(|arg| String::from_utf8_lossy(arg).into_owned())
    .collect::<Vec<_>>()
    .join(" ");
  (!joined.is_empty()).then_some(joined)
}

/// The process's creation time, in seconds since the epoch.
#[cfg(windows)]
pub(super) fn process_start_time(pid: u32) -> Option<String> {
  windows_process(pid, |process| process.map(|process| process.start_time().to_string())).flatten()
}

#[cfg(windows)]
pub(super) fn process_command(pid: u32) -> Option<String> {
  windows_process(pid, |process| {
    let joined = process?
      .cmd()
      .iter()
      .map(|arg| arg.to_string_lossy().into_owned())
      .collect::<Vec<_>>()
      .join(" ");
    (!joined.is_empty()).then_some(joined)
  })
  .flatten()
}

/// A child that sleeps `seconds` and prints nothing, as a stand-in for a
/// browser in tests: in its own process group on Unix, so a group kill
/// cannot reach the test runner.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn test_sleeper(seconds: u32) -> std::process::Command {
  #[cfg(unix)]
  {
    use std::os::unix::process::CommandExt as _;
    let mut command = std::process::Command::new("sleep");
    command.arg(seconds.to_string()).process_group(0);
    command
  }
  #[cfg(windows)]
  {
    let mut command = std::process::Command::new("powershell");
    command.args(["-NoProfile", "-Command", &format!("Start-Sleep -Seconds {seconds}")]);
    command
  }
}

#[cfg(all(unix, any(test, feature = "test-support")))]
pub mod test_support {
  use super::process_is_live;

  /// A stand-in for a browser binary that forks the real browser and exits,
  /// leaving it in the launcher's process group. Returns the launcher and the
  /// file it writes the forked pid to.
  ///
  /// # Errors
  ///
  /// Returns any filesystem error encountered while writing or marking the launcher script executable.
  pub fn test_forking_launcher(dir: &std::path::Path) -> std::io::Result<(std::path::PathBuf, std::path::PathBuf)> {
    use std::os::unix::fs::PermissionsExt as _;
    let launcher = dir.join("forking-launcher");
    let forked = dir.join("forked.pid");
    // `; wait` keeps the forked shell from exec-ing into sleep.
    std::fs::write(
      &launcher,
      format!(
        "#!/bin/sh\nsh -c 'sleep 60; wait' &\necho $! > '{}'\nexit 0\n",
        forked.display()
      ),
    )?;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
    Ok((launcher, forked))
  }

  /// Whether `pid` is gone within a few seconds. A process whose parent
  /// exited is reaped by init, so it reads as live only until then.
  #[must_use]
  pub fn test_process_ends(pid: u32) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
      if !process_is_live(pid) {
        return true;
      }
      std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
  }

  /// Read a decimal pid written by a test launcher script.
  ///
  /// # Errors
  ///
  /// Returns an error if the pid file cannot be read or parsed as a `u32`.
  pub fn test_read_pid(file: &std::path::Path) -> std::io::Result<u32> {
    std::fs::read_to_string(file)?
      .trim()
      .parse()
      .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
  }
}

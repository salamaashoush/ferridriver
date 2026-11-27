//! On-disk browser launch registry and stale-browser reclamation.

#[cfg(any(target_os = "macos", target_os = "linux"))]
use super::inspect::process_group_members;
use super::inspect::{process_command, process_is_live, process_start_time};
use super::kill_process_group;

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
pub(super) struct BrowserRecord {
  pub(super) owner_pid: u32,
  pub(super) browser_pid: u32,
  /// Identifies the process at sweep time; present even when the
  /// directory belongs to the caller and must not be deleted.
  pub(super) profile_dir: Option<String>,
  /// Temp directories this browser owns: its profile when we made it,
  /// plus the downloads directory.
  #[serde(default)]
  pub(super) owned_dirs: Vec<String>,
  /// The browser's start time as `ps` reports it. Pids are recycled, and
  /// (pid, start time) is the pair that identifies a process across that:
  /// without it a stale record can name someone else's browser. Absent on
  /// records written by an older build, which fall back to matching the
  /// command line alone.
  #[serde(default)]
  pub(super) start_time: Option<String>,
}

// Concurrent sweepers delete malformed JSON, so never expose a partial update.
pub(super) fn write_record_file(path: &std::path::Path, record: &BrowserRecord) -> std::io::Result<()> {
  use std::io::Write;

  let parent = path
    .parent()
    .ok_or_else(|| std::io::Error::other("process record has no parent directory"))?;
  let bytes = serde_json::to_vec(record).map_err(std::io::Error::other)?;
  let mut file = staged_record(STAGED_RECORD_PREFIX, parent)?;
  file.write_all(&bytes)?;
  file.persist(path).map_err(|error| error.error)?;
  Ok(())
}

pub(super) const STAGED_RECORD_PREFIX: &str = "staging-";
pub(super) const RECOVERY_RECORD_PREFIX: &str = "cleanup-";

/// A record file being written, named `<prefix><writer pid>-<random>` so a
/// writer killed before it renames the file into place leaves a stray that
/// [`sweep_stale_browsers`] can attribute to a dead process.
fn staged_record(prefix: &str, parent: &std::path::Path) -> std::io::Result<tempfile::NamedTempFile> {
  tempfile::Builder::new()
    .prefix(&format!("{prefix}{}-", std::process::id()))
    .tempfile_in(parent)
}

/// The pid of the process that staged `name`, when it is a staged record.
fn staged_record_writer(name: &str) -> Option<u32> {
  let rest = name
    .strip_prefix(STAGED_RECORD_PREFIX)
    .or_else(|| name.strip_prefix(RECOVERY_RECORD_PREFIX))?;
  rest.split_once('-')?.0.parse().ok()
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
  pub(super) path: Option<std::path::PathBuf>,
  record: BrowserRecord,
  pub(super) owned_dirs: std::sync::Arc<std::sync::Mutex<OwnedDirectories>>,
}

pub(super) struct OwnedDirectories {
  pub(super) paths: Vec<std::path::PathBuf>,
  pub(super) recovery_path: Option<std::path::PathBuf>,
}

impl OwnedDirectories {
  pub(super) fn remove(&mut self) -> std::io::Result<()> {
    for dir in &self.paths {
      crate::async_tempdir::removal_result(dir, crate::async_tempdir::remove_dir_all_retrying(dir))?;
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

pub(super) fn registry_dir() -> Option<std::path::PathBuf> {
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
  #[cfg(unix)]
  crate::reaper::watch(pid);
  #[cfg(windows)]
  job::track(pid);
  let _ = ProcRecord::write_file(pid, profile_dir, owns_profile_dir);
}

/// Windows Job Objects, one per browser, each set to kill everything in
/// it when its last handle closes. Only this process holds the handles,
/// so a browser dies with ferridriver however ferridriver dies: what the
/// Unix watchdog does, done by the kernel.
#[cfg(windows)]
pub(super) mod job {
  use std::sync::{Mutex, OnceLock, PoisonError};

  use rustc_hash::FxHashMap;
  use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
  use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
  };
  use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

  /// A job's handle, held as its address so the type is `Send`.
  pub(in crate::process) struct ProcessJob(usize);

  impl ProcessJob {
    fn handle(&self) -> HANDLE {
      self.0 as HANDLE
    }

    /// A new kill-on-close job holding `pid`, or `None` (logged) when
    /// Windows refuses: the browser then lives only as long as teardown.
    #[expect(
      unsafe_code,
      reason = "Job Objects are reachable only through the Win32 FFI in windows-sys"
    )]
    fn holding(pid: u32) -> Option<Self> {
      // SAFETY: both pointers may be null: an unnamed job with default
      // security. The handle is checked before use.
      let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
      if job.is_null() {
        warn(pid, "cannot create a job for the browser");
        return None;
      }
      // From here `Drop` closes the job if a later step fails.
      let job = Self(job as usize);
      // SAFETY: the limits struct is plain integers, for which all-zero
      // bytes are a valid value.
      let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
      limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
      let size = u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).ok()?;
      // SAFETY: the handle is a live job this value owns, and `limits`
      // is the struct the information class names, `size` bytes long,
      // outliving the call.
      let limited = unsafe {
        SetInformationJobObject(
          job.handle(),
          JobObjectExtendedLimitInformation,
          (&raw const limits).cast(),
          size,
        )
      };
      if limited == 0 {
        warn(pid, "cannot make the browser's job kill on close");
        return None;
      }
      // SAFETY: plain integer arguments; the handle is checked before use.
      let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
      if process.is_null() {
        warn(pid, "cannot open the browser to put it in a job");
        return None;
      }
      // SAFETY: both handles are live: the job this value owns and the
      // process opened above.
      let assigned = unsafe { AssignProcessToJobObject(job.handle(), process) };
      // SAFETY: `process` is live, owned here alone, and not used again.
      unsafe { CloseHandle(process) };
      if assigned == 0 {
        warn(pid, "cannot put the browser in a job");
        return None;
      }
      Some(job)
    }

    /// Kill every process in the job.
    ///
    /// # Errors
    /// Returns the OS error when Windows refuses to terminate the job.
    #[expect(
      unsafe_code,
      reason = "Job Objects are reachable only through the Win32 FFI in windows-sys"
    )]
    pub(in crate::process) fn terminate(&self) -> std::io::Result<()> {
      // SAFETY: the handle is a live job this value owns.
      if unsafe { TerminateJobObject(self.handle(), 1) } == 0 {
        return Err(std::io::Error::last_os_error());
      }
      Ok(())
    }

    /// How many processes in the job are still running.
    #[expect(
      unsafe_code,
      reason = "Job Objects are reachable only through the Win32 FFI in windows-sys"
    )]
    fn active_processes(&self) -> std::io::Result<u32> {
      let size =
        u32::try_from(std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>()).map_err(std::io::Error::other)?;
      // SAFETY: the accounting struct is plain integers, for which all-zero
      // bytes are a valid value.
      let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
      // SAFETY: the handle is a live job this value owns, and `info` is the
      // struct the information class names, `size` bytes long.
      let queried = unsafe {
        QueryInformationJobObject(
          self.handle(),
          JobObjectBasicAccountingInformation,
          (&raw mut info).cast(),
          size,
          std::ptr::null_mut(),
        )
      };
      if queried == 0 {
        return Err(std::io::Error::last_os_error());
      }
      Ok(info.ActiveProcesses)
    }

    /// Wait until every process in the job has gone. `TerminateJobObject`
    /// only starts the kills, and a process it ended keeps its files open
    /// until it is gone: a Firefox content process still holding the
    /// profile makes deleting it fail with a sharing violation.
    pub(in crate::process) async fn until_empty(&self, limit: std::time::Duration) -> std::io::Result<()> {
      let deadline = std::time::Instant::now() + limit;
      while self.active_processes()? > 0 {
        if std::time::Instant::now() >= deadline {
          return Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
              "the browser's processes were still running {}s after being killed",
              limit.as_secs()
            ),
          ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
      }
      Ok(())
    }
  }

  impl Drop for ProcessJob {
    #[expect(
      unsafe_code,
      reason = "Job Objects are reachable only through the Win32 FFI in windows-sys"
    )]
    fn drop(&mut self) {
      // SAFETY: the handle is a live job this value owns; closing the last
      // handle kills what is left in it.
      unsafe {
        CloseHandle(self.handle());
      }
    }
  }

  fn warn(pid: u32, what: &str) {
    tracing::warn!(
      target: "ferridriver::process",
      pid,
      error = %std::io::Error::last_os_error(),
      "{what}; a hard kill of this process would leak it"
    );
  }

  /// Jobs of browsers spawned but not yet owned by a `ChildGroup`, which
  /// is built only after the launch handshake.
  fn pending() -> &'static Mutex<FxHashMap<u32, ProcessJob>> {
    static PENDING: OnceLock<Mutex<FxHashMap<u32, ProcessJob>>> = OnceLock::new();
    PENDING.get_or_init(Mutex::default)
  }

  /// Put `pid` in a job the instant it is spawned.
  pub(super) fn track(pid: u32) {
    let mut pending = pending().lock().unwrap_or_else(PoisonError::into_inner);
    if !pending.contains_key(&pid)
      && let Some(job) = ProcessJob::holding(pid)
    {
      pending.insert(pid, job);
    }
  }

  /// The job `track` made for `pid`, or a new one.
  pub(in crate::process) fn claim(pid: u32) -> Option<ProcessJob> {
    if pid == 0 {
      return None;
    }
    let tracked = pending().lock().unwrap_or_else(PoisonError::into_inner).remove(&pid);
    tracked.or_else(|| ProcessJob::holding(pid))
  }
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
  pub(super) fn write(browser_pid: u32, profile_dir: Option<&std::path::Path>, owns_profile_dir: bool) -> Option<Self> {
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
  pub(super) fn own_dir(&mut self, dir: &std::path::Path) {
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

  pub(super) fn retire(&mut self) -> std::io::Result<()> {
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
      let mut file = staged_record(RECOVERY_RECORD_PREFIX, parent)?;
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

/// Whether the recorded browser exited but left members of its process
/// group running, as a launcher that forks the real browser and exits does.
///
/// The group id cannot be handed to a new process while the group has
/// members, yet the pid could have been reused by an unrelated group leader
/// that has since exited too. A member must therefore still name the
/// record's profile directory, which no one else's process carries.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn orphaned_group_is_ours(record: &BrowserRecord) -> bool {
  let Some(profile) = record.profile_dir.as_deref() else {
    return false;
  };
  process_group_members(record.browser_pid).is_some_and(|members| {
    members
      .iter()
      .any(|&member| process_command(member).is_some_and(|command| command.contains(profile)))
  })
}

/// Windows has no process group to outlive its leader: a browser's tree is
/// held by its job, which the job's own teardown ends.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn orphaned_group_is_ours(_record: &BrowserRecord) -> bool {
  false
}

/// Whether a recorded directory is somewhere we are willing to delete
/// recursively. Every directory we own is a temp dir we created, so anything
/// outside the temp root or our own cache directory is a corrupt or tampered
/// record, not ours to remove.
pub(super) fn is_reclaimable_dir(path: &std::path::Path) -> bool {
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
      reclaimed += usize::from(reclaim_staged_record(&path));
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

/// A record its writer died before renaming into place. One that was fully
/// written may be the only trace of a launched browser, so it is reclaimed
/// like any record; a partial one names nothing and is removed.
fn reclaim_staged_record(path: &std::path::Path) -> bool {
  let Some(writer) = path
    .file_name()
    .and_then(|name| name.to_str())
    .and_then(staged_record_writer)
  else {
    return false;
  };
  if writer == std::process::id() || process_is_live(writer) {
    return false;
  }
  if let Some(record) = std::fs::read(path)
    .ok()
    .and_then(|bytes| serde_json::from_slice::<BrowserRecord>(&bytes).ok())
  {
    reclaim_record(path, &record)
  } else {
    let _ = std::fs::remove_file(path);
    false
  }
}

pub(super) fn reclaim_record(path: &std::path::Path, record: &BrowserRecord) -> bool {
  let identity = if record.browser_pid == 0 {
    Identity::NotOurs
  } else if process_is_live(record.browser_pid) {
    identify_browser(record)
  } else if orphaned_group_is_ours(record) {
    Identity::Ours
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
      if let Err(error) = crate::async_tempdir::removal_result(dir, crate::async_tempdir::remove_dir_all_retrying(dir))
      {
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

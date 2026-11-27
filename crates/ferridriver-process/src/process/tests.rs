//! Process lifecycle and registry unit tests.

#[cfg(target_os = "macos")]
use super::inspect::proc_bsdinfo;
use super::inspect::process_start_time;
use super::registry::{
  BrowserRecord, RECOVERY_RECORD_PREFIX, STAGED_RECORD_PREFIX, reclaim_record, registry_dir, write_record_file,
};
use super::*;

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

#[cfg(unix)]
use super::test_support::test_process_ends;

/// A pid that has exited and been reaped, so `process_is_live` is
/// false for it. Kernel pid reuse could in principle hand it to
/// someone else, which is exactly why the sweep also checks the
/// command line before signalling anything.
fn dead_pid() -> u32 {
  let mut child = super::test_sleeper(0)
    .spawn()
    .expect("spawn a sleeper that exits at once");
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

#[cfg(unix)]
/// A live process group that looks like an automation browser to
/// `browser_matches`, in its own session so signalling it cannot reach the
/// test runner. Returns (group leader pid, non-leader child pid); the
/// child's pgid is the leader, so it is not a group leader itself.
fn spawn_fake_browser_group() -> (u32, std::process::Child) {
  use std::io::BufRead as _;

  let mut cmd = std::process::Command::new("sh");
  // The inner `sh` carries the automation marker in its argv and is a
  // plain child, so `pgid(child) == leader != child`.
  cmd
    .arg("-c")
    .arg("sh -c 'sleep 300; wait' --remote-debugging-port=59999 & echo $!; sleep 300; wait")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null());
  // Own session so the group kill under test cannot reach us.
  spawn_in_own_session(&mut cmd);
  let mut leader = cmd.spawn().expect("spawn fake browser group");
  let mut line = String::new();
  let stdout = leader.stdout.take().expect("stdout piped");
  std::io::BufReader::new(stdout)
    .read_line(&mut line)
    .expect("read child pid");
  let child: u32 = line.trim().parse().expect("child pid");
  (child, leader)
}

/// Each child outlives the round, so a spawn another child's inherited
/// pipe holds up stays held until the deadline.
#[cfg(unix)]
fn spawn_together(
  children: usize,
  spawn: fn(&mut std::process::Command) -> std::io::Result<std::process::Child>,
) -> usize {
  let (started, starts) = std::sync::mpsc::channel();
  let together = std::sync::Arc::new(std::sync::Barrier::new(children));
  for _ in 0..children {
    let (started, together) = (started.clone(), std::sync::Arc::clone(&together));
    std::thread::spawn(move || {
      let mut command = std::process::Command::new("sleep");
      command.arg("30");
      // A `pre_exec` hook is what puts std on its fork path, as a
      // browser launch is.
      spawn_in_own_session(&mut command);
      together.wait();
      let _ = started.send(spawn(&mut command));
    });
  }
  let started: Vec<_> = (0..children)
    .map(|_| starts.recv_timeout(std::time::Duration::from_secs(5)))
    .collect();
  let stalled = started.iter().filter(|child| child.is_err()).count();
  for mut child in started.into_iter().flatten().flatten() {
    let _ = child.kill();
    let _ = child.wait();
  }
  stalled
}

#[cfg(unix)]
#[test]
fn children_started_together_do_not_hold_up_each_others_spawn() {
  for round in 0..40 {
    let stalled = spawn_together(12, |command| spawn_exclusive(|| command.spawn()));
    assert_eq!(stalled, 0, "round {round}: spawns still waiting on their own child");
  }
}

#[cfg(unix)]
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
  proc_bsdinfo(pid).is_some_and(|info| info.pbi_status != libc::SZOMB)
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

#[cfg(windows)]
fn is_running(pid: u32) -> bool {
  super::process_is_live(pid)
}

#[cfg(unix)]
/// Give the group kill a moment to land before asking.
fn settle() {
  std::thread::sleep(std::time::Duration::from_millis(200));
}

#[cfg(unix)]
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

/// A group whose leader has exited and been reaped, as a launcher that
/// forks the browser and exits leaves it. `marker` is the member's argv[0],
/// standing in for the `--user-data-dir` a browser carries. Returns
/// (dead leader pid, live member pid).
#[cfg(unix)]
fn spawn_orphaned_group(marker: &str) -> (u32, u32) {
  use std::io::BufRead as _;
  let mut cmd = std::process::Command::new("sh");
  cmd
    .arg("-c")
    .arg(format!(
      "sh -c 'sleep 300; wait' '{marker}' >/dev/null 2>&1 & echo $!; exit 0"
    ))
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::null());
  spawn_in_own_session(&mut cmd);
  let mut leader = cmd.spawn().expect("spawn launcher");
  let mut line = String::new();
  std::io::BufReader::new(leader.stdout.take().expect("stdout piped"))
    .read_line(&mut line)
    .expect("read member pid");
  let member = line.trim().parse().expect("member pid");
  let leader_pid = leader.id();
  leader.wait().expect("reap launcher");
  (leader_pid, member)
}

#[cfg(unix)]
#[test]
fn sweep_kills_the_group_a_dead_recorded_leader_left_running() {
  let profile = std::env::temp_dir().join(format!("ferridriver-orphaned-group-{}", std::process::id()));
  std::fs::create_dir_all(&profile).expect("profile dir");
  let (leader, member) = spawn_orphaned_group(&format!("--user-data-dir={}", profile.display()));
  let record = write_record(dead_pid(), leader, Some(&profile), true);

  let _ = sweep_stale_browsers();

  let ended = test_process_ends(member);
  if !ended {
    kill_process_group(leader);
  }
  let _ = std::fs::remove_file(&record);
  let _ = std::fs::remove_dir_all(&profile);
  assert!(ended, "the member a dead leader left in its group must be killed");
}

/// The pid of a dead leader may have been reused by an unrelated group
/// leader that exited too, so a surviving group is only ours while a
/// member names the record's profile.
#[cfg(unix)]
#[test]
fn sweep_spares_a_leftover_group_that_names_another_profile() {
  let (leader, member) = spawn_orphaned_group("--user-data-dir=/elsewhere");
  let profile = std::env::temp_dir().join(format!("ferridriver-not-this-group-{}", std::process::id()));
  let record = write_record(dead_pid(), leader, Some(&profile), false);

  let _ = sweep_stale_browsers();
  settle();

  let survived = is_running(member);
  kill_process_group(leader);
  let _ = std::fs::remove_file(&record);
  assert!(
    survived,
    "a group none of whose members names the profile must be spared"
  );
}

#[test]
fn sweep_reclaims_a_record_its_dead_writer_never_renamed_into_place() {
  let dir = registry_dir().expect("registry dir");
  let profile = std::env::temp_dir().join(format!("ferridriver-staged-record-{}", std::process::id()));
  std::fs::create_dir_all(&profile).expect("profile dir");
  let record = BrowserRecord {
    owner_pid: dead_pid(),
    browser_pid: dead_pid(),
    profile_dir: Some(profile.to_string_lossy().into_owned()),
    owned_dirs: vec![profile.to_string_lossy().into_owned()],
    start_time: None,
  };
  let writer = dead_pid();
  let staged = dir.join(format!("{STAGED_RECORD_PREFIX}{writer}-a1b2c3"));
  std::fs::write(&staged, serde_json::to_vec(&record).expect("encode")).expect("stage record");
  let partial = dir.join(format!("{RECOVERY_RECORD_PREFIX}{writer}-d4e5f6"));
  std::fs::write(&partial, b"{\"owner_pid\":").expect("stage partial record");
  let live = dir.join(format!("{STAGED_RECORD_PREFIX}{}-g7h8i9", std::process::id()));
  std::fs::write(&live, b"").expect("stage live record");

  let _ = sweep_stale_browsers();

  let left = [staged.exists(), partial.exists(), profile.exists(), live.exists()];
  let _ = std::fs::remove_dir_all(&profile);
  let _ = std::fs::remove_file(&staged);
  let _ = std::fs::remove_file(&partial);
  let _ = std::fs::remove_file(&live);
  assert_eq!(
    left,
    [false, false, false, true],
    "[staged, partial, its profile, live writer's] after the sweep"
  );
}

#[cfg(unix)]
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

#[cfg(unix)]
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
    let child = tokio::process::Command::from(super::test_sleeper(30))
      .kill_on_drop(true)
      .spawn()
      .expect("spawn sleeper");
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

/// Windows has no process group to kill once the leader is gone: the
/// job the group owns reaches what the leader started anyway.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_group_ends_what_its_exited_leader_left_running() {
  use tokio::io::AsyncBufReadExt as _;
  let mut leader = tokio::process::Command::new("powershell")
    .args([
      "-NoProfile",
      "-Command",
      "$p = Start-Process powershell -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 60' \
         -WindowStyle Hidden -PassThru; Write-Output $p.Id",
    ])
    .stdout(std::process::Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .expect("spawn leader");
  let stdout = leader.stdout.take().expect("stdout piped");
  let mut group = ChildGroup::unregistered(leader);
  let line = tokio::io::BufReader::new(stdout)
    .lines()
    .next_line()
    .await
    .expect("read the sleeper's pid")
    .expect("the leader prints the sleeper's pid");
  let sleeper: u32 = line.trim().parse().expect("a pid");
  assert!(is_running(sleeper), "the sleeper outlives its leader");
  group.wait().await.expect("wait for the group");
  std::thread::sleep(std::time::Duration::from_millis(500));
  assert!(!is_running(sleeper), "the group's job ends what the leader left");
}

/// The sweep's kill reaches a recorded browser's whole tree.
#[cfg(windows)]
#[test]
fn sweep_kills_a_recorded_browser_tree_whose_start_time_matches() {
  use std::io::BufRead as _;
  let mut leader = std::process::Command::new("powershell")
    .args([
      "-NoProfile",
      "-Command",
      "$p = Start-Process powershell -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 60' \
         -WindowStyle Hidden -PassThru; Write-Output $p.Id; Start-Sleep -Seconds 60 # --remote-debugging-port=59999",
    ])
    .stdout(std::process::Stdio::piped())
    .spawn()
    .expect("spawn leader");
  let mut line = String::new();
  std::io::BufReader::new(leader.stdout.take().expect("stdout piped"))
    .read_line(&mut line)
    .expect("read the sleeper's pid");
  let sleeper: u32 = line.trim().parse().expect("a pid");
  let victim = leader.id();
  let owner = dead_pid();
  let record_path = write_record(owner, victim, None, false);

  let _ = sweep_stale_browsers();
  std::thread::sleep(std::time::Duration::from_millis(500));

  let leader_killed = !is_running(victim);
  let sleeper_killed = !is_running(sleeper);
  let _ = leader.kill();
  let _ = leader.wait();
  kill_process_group(sleeper);
  let _ = std::fs::remove_file(&record_path);
  assert!(leader_killed, "our own leaked browser is reclaimed");
  assert!(sleeper_killed, "and so is what it started");
}

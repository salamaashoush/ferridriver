//! A watchdog process that kills our browsers if we die without teardown.
//!
//! [`super::process::ChildGroup`] covers every exit path where this
//! process gets to run code, and [`super::process::sweep_stale_browsers`]
//! reclaims leftovers on the NEXT start. Neither helps in the window
//! that matters most: `kill -9` on an MCP server leaves a headed
//! `cdp-raw` Chrome or a `bidi` Firefox running — reparented to pid 1,
//! no window, no owner — until someone starts ferridriver again.
//!
//! The watchdog closes that window. It is a `sh` child holding the read
//! end of a pipe whose write end lives only here (Rust sets CLOEXEC on
//! its own pipes, so no browser inherits a copy). We send it a browser
//! pid to watch on launch and a pid to forget on teardown. However this
//! process dies, the write end closes, the watchdog's `read` hits EOF,
//! and it kills every process group still on its list.
//!
//! It runs in its own session, so a Ctrl-C aimed at the foreground
//! process group never reaches it, and it is not in any browser's group.

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Mutex, OnceLock};

/// Watch list maintenance, then a group kill for whatever is left when
/// the pipe closes.
///
/// A recycled pid is the one way this could signal something that is
/// not ours, so a live pid is only killed while it is still its own
/// process group leader — which is true of every browser we spawn (they
/// are `setsid`-ed) and false of almost anything else.
///
/// A watched leader with no process left was reaped by whoever inherited
/// it when this process died, not by us: we forget a pid before reaping
/// it. If its group still has members, the pid cannot have been reused,
/// so those members are what the browser left running and the group is
/// killed too.
const REAPER_SH: &str = r#"
watched=
while IFS= read -r line; do
  pid=${line#?}
  op=${line%"$pid"}
  case "$op" in
    +) watched="$watched $pid" ;;
    -) kept=; for p in $watched; do [ "$p" = "$pid" ] || kept="$kept $p"; done; watched=$kept ;;
  esac
done
for p in $watched; do
  pgid=$(ps -p "$p" -o pgid= 2>/dev/null | tr -d ' ')
  if [ "$pgid" = "$p" ]; then
    kill -9 -"$p" 2>/dev/null
  elif [ -z "$pgid" ] && ps -A -o pgid= | tr -d ' ' | grep -qx "$p"; then
    kill -9 -"$p" 2>/dev/null
  fi
done
exit 0
"#;

struct Reaper {
  stdin: ChildStdin,
  /// Held so the watchdog is reaped rather than left a zombie when this
  /// process exits normally.
  _child: Child,
}

fn reaper() -> &'static Mutex<Option<Reaper>> {
  static REAPER: OnceLock<Mutex<Option<Reaper>>> = OnceLock::new();
  REAPER.get_or_init(|| Mutex::new(spawn_reaper()))
}

fn spawn_reaper() -> Option<Reaper> {
  let mut command = Command::new("sh");
  command
    .arg("-c")
    .arg(REAPER_SH)
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
  // Own session: terminal signals aimed at our foreground group must not
  // take the watchdog down with us.
  #[cfg(unix)]
  super::process::spawn_in_own_session(&mut command);
  let mut child = match super::process::spawn_exclusive(|| command.spawn()) {
    Ok(c) => c,
    Err(e) => {
      tracing::warn!(
        target: "ferridriver::process",
        error = %e,
        "cannot start the browser watchdog; a hard kill of this process will leak browsers until the next start"
      );
      return None;
    },
  };
  let stdin = child.stdin.take()?;
  Some(Reaper { stdin, _child: child })
}

fn send(line: &str) {
  let mut guard = reaper().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
  let Some(r) = guard.as_mut() else { return };
  if r.stdin.write_all(line.as_bytes()).is_err() || r.stdin.flush().is_err() {
    // The watchdog died; drop it rather than retrying on a dead pipe.
    // Its own exit path already killed whatever it was holding.
    *guard = None;
  }
}

/// Ask the watchdog to kill `pid`'s process group if this process dies
/// without calling [`forget`].
pub fn watch(pid: u32) {
  if pid != 0 {
    send(&format!("+{pid}\n"));
  }
}

/// Drop `pid` from the watch list — its group has been dealt with.
pub fn forget(pid: u32) {
  if pid != 0 {
    send(&format!("-{pid}\n"));
  }
}

#[cfg(test)]
mod tests {
  use std::process::{Command, Stdio};

  /// Drive the watchdog script exactly as [`super::watch`] /
  /// [`super::forget`] do, then close the pipe and assert it killed the
  /// watched group and spared the forgotten one.
  #[test]
  fn the_watchdog_kills_watched_groups_when_the_pipe_closes() {
    use std::io::Write;

    let spawn_group_leader = || {
      let mut cmd = Command::new("sleep");
      cmd.arg("30").stdout(Stdio::null()).stderr(Stdio::null());
      // Mirrors how browsers are spawned.
      crate::backend::process::spawn_in_own_session(&mut cmd);
      cmd.spawn().expect("spawn sleep")
    };

    let mut watched = spawn_group_leader();
    let mut forgotten = spawn_group_leader();

    let mut reaper = Command::new("sh")
      .arg("-c")
      .arg(super::REAPER_SH)
      .stdin(Stdio::piped())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .spawn()
      .expect("spawn reaper");
    let mut stdin = reaper.stdin.take().expect("reaper stdin");
    writeln!(stdin, "+{}", watched.id()).expect("watch");
    writeln!(stdin, "+{}", forgotten.id()).expect("watch");
    writeln!(stdin, "-{}", forgotten.id()).expect("forget");
    stdin.flush().expect("flush");
    drop(stdin);

    let status = reaper.wait().expect("reaper exits on EOF");
    assert!(status.success(), "reaper exited cleanly");

    // The reaper's exit means it SENT the signal, not that the kernel has
    // delivered it and reaped the child — so poll rather than assert on the
    // instant after. (Asserting immediately made this test fail under load.)
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut killed = None;
    while std::time::Instant::now() < deadline {
      killed = watched.try_wait().expect("try_wait");
      if killed.is_some() {
        break;
      }
      std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(killed.is_some(), "watched process group was killed");
    // Already exited; this only reaps the zombie the poll left behind.
    let _ = watched.wait();
    // The forgotten leader survived that whole window, which is a stronger
    // statement than "had not died yet the moment the reaper exited".
    assert!(
      forgotten.try_wait().expect("try_wait").is_none(),
      "a forgotten pid must not be signalled"
    );
    let _ = forgotten.kill();
    let _ = forgotten.wait();
  }

  /// When this process dies, whoever inherits a browser leader may reap it
  /// before the watchdog looks, and a leader that forked the real browser
  /// may have exited already. Neither leaves the watchdog a leader to see,
  /// but the group it led is still ours and still running.
  #[test]
  fn the_watchdog_kills_the_group_a_reaped_leader_left_running() {
    use std::io::{BufRead as _, Write as _};

    let mut leader = Command::new("sh");
    leader
      .arg("-c")
      .arg("sh -c 'sleep 30; wait' >/dev/null 2>&1 & echo $!; exit 0")
      .stdout(Stdio::piped())
      .stderr(Stdio::null());
    crate::backend::process::spawn_in_own_session(&mut leader);
    let mut leader = leader.spawn().expect("spawn leader");
    let mut line = String::new();
    std::io::BufReader::new(leader.stdout.take().expect("stdout piped"))
      .read_line(&mut line)
      .expect("read member pid");
    let member: u32 = line.trim().parse().expect("member pid");
    let leader_pid = leader.id();
    leader.wait().expect("reap leader");

    let mut reaper = Command::new("sh")
      .arg("-c")
      .arg(super::REAPER_SH)
      .stdin(Stdio::piped())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .spawn()
      .expect("spawn reaper");
    let mut stdin = reaper.stdin.take().expect("reaper stdin");
    writeln!(stdin, "+{leader_pid}").expect("watch");
    drop(stdin);
    assert!(
      reaper.wait().expect("reaper exits on EOF").success(),
      "reaper exited cleanly"
    );

    let ended = crate::backend::process::test_support::test_process_ends(member);
    if !ended {
      crate::backend::process::kill_process_group(leader_pid);
    }
    assert!(ended, "the member a reaped leader left in its group must be killed");
  }
}

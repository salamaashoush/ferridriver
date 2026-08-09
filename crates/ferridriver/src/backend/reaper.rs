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
/// not ours, so a pid is only killed while it is still its own process
/// group leader — which is true of every browser we spawn (they are
/// `setsid`-ed) and false of almost anything else.
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
  [ "$pgid" = "$p" ] && kill -9 -"$p" 2>/dev/null
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
  #[cfg(unix)]
  #[allow(unsafe_code)]
  unsafe {
    use std::os::unix::process::CommandExt;
    // SAFETY: `setsid` is async-signal-safe and the closure allocates
    // nothing. Own session: terminal signals aimed at our foreground
    // group must not take the watchdog down with us.
    command.pre_exec(|| {
      libc::setsid();
      Ok(())
    });
  }
  let mut child = match command.spawn() {
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
    use std::os::unix::process::CommandExt;

    let spawn_group_leader = || {
      let mut cmd = Command::new("sleep");
      cmd.arg("30").stdout(Stdio::null()).stderr(Stdio::null());
      #[allow(unsafe_code)]
      unsafe {
        // SAFETY: `setsid` is async-signal-safe; the closure captures
        // nothing. Mirrors how browsers are spawned.
        cmd.pre_exec(|| {
          libc::setsid();
          Ok(())
        });
      }
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

    // The watched leader is gone; the forgotten one is untouched.
    let killed = watched.try_wait().expect("try_wait");
    assert!(killed.is_some(), "watched process group was killed");
    assert!(
      forgotten.try_wait().expect("try_wait").is_none(),
      "a forgotten pid must not be signalled"
    );
    let _ = forgotten.kill();
    let _ = forgotten.wait();
  }
}

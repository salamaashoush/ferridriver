//! Interactive key handler for watch mode.
//!
//! Architecture (matches Playwright's pattern):
//! - Raw mode is ONLY active during the idle wait period (between test runs)
//! - Raw mode is DISABLED during test execution so output renders correctly
//! - The watch loop in runner.rs owns the raw mode lifecycle
//!
//! The KeyHandler spawns a background thread that polls crossterm events.
//! Events are only read when raw mode is active (the thread polls a flag).
//! Commands flow through an async channel to the watch loop.

use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Watch mode command from keyboard input.
#[derive(Debug, Clone)]
pub enum WatchCommand {
  /// Run all tests ('a').
  RunAll,
  /// Run only previously failed tests ('f').
  RunFailed,
  /// Re-run with current filter (Enter).
  Rerun,
  /// Enter filter mode, then apply pattern ('p' -> type -> Enter).
  FilterByName(String),
  /// Quit watch mode ('q').
  Quit,
}

/// Interactive key handler for watch mode.
pub struct KeyHandler {
  rx: async_channel::Receiver<WatchCommand>,
  active: Arc<AtomicBool>,
  _handle: std::thread::JoinHandle<()>,
}

impl KeyHandler {
  /// Create the key handler. Does NOT enable raw mode — the watch loop controls that.
  ///
  /// # Errors
  ///
  /// Returns an error if the terminal doesn't support raw mode (non-TTY).
  pub fn new() -> Result<Self, String> {
    // Verify TTY support by briefly enabling/disabling raw mode.
    crossterm::terminal::enable_raw_mode().map_err(|e| format!("raw mode not supported: {e}"))?;
    let _ = crossterm::terminal::disable_raw_mode();

    let (tx, rx) = async_channel::bounded(16);
    let active = Arc::new(AtomicBool::new(false));
    let active_clone = Arc::clone(&active);

    let handle = std::thread::Builder::new()
      .name("ferridriver-keyhandler".into())
      .spawn(move || key_poll_loop(&tx, &active_clone))
      .map_err(|e| format!("spawn key handler: {e}"))?;

    Ok(Self {
      rx,
      active,
      _handle: handle,
    })
  }

  /// Receive the next key command (async).
  pub async fn recv(&self) -> Option<WatchCommand> {
    self.rx.recv().await.ok()
  }

  /// Enter interactive mode: enable raw mode and start accepting keypresses.
  /// Call after test run completes, before the idle wait.
  pub fn enter_interactive(&self) {
    let _ = crossterm::terminal::enable_raw_mode();
    self.active.store(true, Ordering::Release);
  }

  /// Leave interactive mode: disable raw mode so output renders correctly.
  /// Call before running tests.
  pub fn leave_interactive(&self) {
    self.active.store(false, Ordering::Release);
    let _ = crossterm::terminal::disable_raw_mode();
  }
}

impl Drop for KeyHandler {
  fn drop(&mut self) {
    self.active.store(false, Ordering::Release);
    let _ = crossterm::terminal::disable_raw_mode();
  }
}

/// Print the interactive hint after a run (raw mode should be OFF when calling this).
pub fn print_watch_hint() {
  let mut stderr = std::io::stderr();
  let _ = writeln!(stderr);
  let _ = writeln!(stderr, "\x1b[2mWatching for changes...\x1b[0m");
  let _ = writeln!(
    stderr,
    "\x1b[2mPress \x1b[0m\x1b[1ma\x1b[0m\x1b[2m to run all, \
     \x1b[0m\x1b[1mf\x1b[0m\x1b[2m to run failed, \
     \x1b[0m\x1b[1mp\x1b[0m\x1b[2m to filter, \
     \x1b[0m\x1b[1mq\x1b[0m\x1b[2m to quit.\x1b[0m"
  );
  let _ = stderr.flush();
}

/// Blocking poll loop. Only reads key events when `active` is true.
fn key_poll_loop(tx: &async_channel::Sender<WatchCommand>, active: &AtomicBool) {
  use crossterm::event::{self, Event, KeyCode, KeyModifiers};

  loop {
    if tx.is_closed() {
      break;
    }

    // When not active, sleep and retry. The watch loop will set active=true
    // after test output is complete and raw mode is enabled.
    if !active.load(Ordering::Acquire) {
      std::thread::sleep(std::time::Duration::from_millis(50));
      continue;
    }

    // Poll with short timeout so we can recheck the active flag.
    if !event::poll(std::time::Duration::from_millis(100)).unwrap_or(false) {
      continue;
    }

    let Ok(Event::Key(key)) = event::read() else {
      continue;
    };

    // Ctrl+C — quit.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
      let _ = tx.try_send(WatchCommand::Quit);
      break;
    }

    let cmd = match key.code {
      KeyCode::Char('a') => Some(WatchCommand::RunAll),
      KeyCode::Char('f') => Some(WatchCommand::RunFailed),
      KeyCode::Char('q') => Some(WatchCommand::Quit),
      KeyCode::Enter => Some(WatchCommand::Rerun),
      KeyCode::Char('p') => {
        // Leave raw mode for line input, then re-enter.
        let _ = crossterm::terminal::disable_raw_mode();
        let pattern = read_filter_pattern();
        let _ = crossterm::terminal::enable_raw_mode();
        if pattern.is_empty() {
          None
        } else {
          Some(WatchCommand::FilterByName(pattern))
        }
      },
      _ => None,
    };

    if let Some(cmd) = cmd {
      let is_quit = matches!(cmd, WatchCommand::Quit);
      let _ = tx.try_send(cmd);
      if is_quit {
        break;
      }
    }
  }
}

/// Read a filter pattern from stdin (cooked mode).
fn read_filter_pattern() -> String {
  let mut stderr = std::io::stderr();
  let _ = write!(stderr, "\r\n\x1b[1mFilter pattern:\x1b[0m ");
  let _ = stderr.flush();

  let mut input = String::new();
  if std::io::stdin().read_line(&mut input).is_ok() {
    input.trim().to_string()
  } else {
    String::new()
  }
}

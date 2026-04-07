//! Interactive key handler for watch mode.
//!
//! Reads raw stdin keypresses via `crossterm` on a dedicated thread,
//! dispatches commands through an async channel.

use std::io::Write;

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
///
/// Spawns a dedicated thread for crossterm key event polling.
/// Commands are sent through an async channel for consumption by the watch loop.
pub struct KeyHandler {
  rx: async_channel::Receiver<WatchCommand>,
  _handle: std::thread::JoinHandle<()>,
}

impl KeyHandler {
  /// Start the key handler.
  ///
  /// Enables raw mode on stdin and spawns a polling thread.
  /// Raw mode is disabled on drop or when quit is sent.
  pub fn start() -> Result<Self, String> {
    crossterm::terminal::enable_raw_mode()
      .map_err(|e| format!("enable raw mode: {e}"))?;

    let (tx, rx) = async_channel::bounded(16);

    let handle = std::thread::Builder::new()
      .name("ferridriver-keyhandler".into())
      .spawn(move || {
        key_poll_loop(&tx);
        let _ = crossterm::terminal::disable_raw_mode();
      })
      .map_err(|e| format!("spawn key handler thread: {e}"))?;

    Ok(Self { rx, _handle: handle })
  }

  /// Receive the next key command (async).
  pub async fn recv(&self) -> Option<WatchCommand> {
    self.rx.recv().await.ok()
  }
}

impl Drop for KeyHandler {
  fn drop(&mut self) {
    let _ = crossterm::terminal::disable_raw_mode();
  }
}

/// Print the interactive hint after a run.
pub fn print_watch_hint() {
  let mut stderr = std::io::stderr();
  let _ = writeln!(stderr);
  let _ = writeln!(
    stderr,
    "\x1b[2mWatching for changes...\x1b[0m"
  );
  let _ = writeln!(
    stderr,
    "\x1b[2mPress \x1b[0m\x1b[1ma\x1b[0m\x1b[2m to run all, \x1b[0m\x1b[1mf\x1b[0m\x1b[2m to run failed, \x1b[0m\x1b[1mp\x1b[0m\x1b[2m to filter, \x1b[0m\x1b[1mq\x1b[0m\x1b[2m to quit.\x1b[0m"
  );
  let _ = stderr.flush();
}

/// Blocking poll loop for crossterm key events.
fn key_poll_loop(tx: &async_channel::Sender<WatchCommand>) {
  use crossterm::event::{self, Event, KeyCode, KeyModifiers};

  loop {
    // Poll with 200ms timeout so the thread can check if the channel is closed.
    if !event::poll(std::time::Duration::from_millis(200)).unwrap_or(false) {
      if tx.is_closed() {
        break;
      }
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
        // Enter filter mode: disable raw mode temporarily for line input.
        let _ = crossterm::terminal::disable_raw_mode();
        let pattern = read_filter_pattern();
        let _ = crossterm::terminal::enable_raw_mode();
        if pattern.is_empty() {
          None
        } else {
          Some(WatchCommand::FilterByName(pattern))
        }
      }
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

/// Read a filter pattern from stdin in cooked mode.
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

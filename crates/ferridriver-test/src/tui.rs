//! Watch mode TUI: ratatui inline viewport with real-time test progress.
//!
//! Layout:
//! ```text
//! ─── scrollback (completed tests) ───
//!   ✓ Scenario: Navigate (340ms)
//!   ✓ Scenario: Check URL (320ms)
//! ─── viewport (live) ────────────────
//!   Scenario: Reload page
//!     ✓ Given I navigate (310ms)
//!     ● When I reload the page...
//! ─────────────────────────────────────
//!   ● Running 3/5  ████░░  60%   1.2s
//!   a all · f failed · p filter · q quit
//! ```

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Terminal, TerminalOptions, Viewport};
use tokio::sync::mpsc;

use crate::interactive::WatchCommand;

// ── Messages ───────────────────────────────────────────────────────────

/// Messages from the TUI reporter to the TUI.
pub enum TuiMessage {
  /// Push completed lines to scrollback (above viewport).
  Scrollback(Vec<Line<'static>>),
  /// Set the currently running test/scenario name.
  CurrentTest(Option<String>),
  /// Add/update a live step in the viewport.
  LiveStep(LiveStep),
  /// Clear live steps (test finished).
  ClearLive,
  /// Update status bar.
  Status(WatchStatus),
}

/// A step shown live in the viewport during execution.
#[derive(Clone)]
pub struct LiveStep {
  pub title: String,
  pub status: LiveStepStatus,
  pub duration_ms: Option<u64>,
}

#[derive(Clone, Copy)]
pub enum LiveStepStatus {
  Running,
  Passed,
  Failed,
  Skipped,
}

/// Status bar state.
#[derive(Clone)]
pub enum WatchStatus {
  Idle,
  Running { completed: usize, total: usize, start: Instant },
  Done { passed: usize, failed: usize, skipped: usize, flaky: usize, duration: Duration },
}

// ── WatchTui ───────────────────────────────────────────────────────────

const VIEWPORT_HEIGHT: u16 = 10;

pub struct WatchTui {
  terminal: Terminal<CrosstermBackend<Stdout>>,
  event_stream: EventStream,
  msg_rx: mpsc::UnboundedReceiver<TuiMessage>,
  status: WatchStatus,
  current_test: Option<String>,
  live_steps: Vec<LiveStep>,
}

impl WatchTui {
  pub fn new() -> Result<(Self, mpsc::UnboundedSender<TuiMessage>), String> {
    crossterm::terminal::enable_raw_mode().map_err(|e| format!("enable raw mode: {e}"))?;

    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::with_options(
      backend,
      TerminalOptions {
        viewport: Viewport::Inline(VIEWPORT_HEIGHT),
      },
    )
    .map_err(|e| format!("create terminal: {e}"))?;

    let (msg_tx, msg_rx) = mpsc::unbounded_channel();

    let mut tui = Self {
      terminal,
      event_stream: EventStream::new(),
      msg_rx,
      status: WatchStatus::Idle,
      current_test: None,
      live_steps: Vec::new(),
    };

    tui.render();
    Ok((tui, msg_tx))
  }

  /// Push completed lines above the viewport into scrollback.
  fn push_to_scrollback(&mut self, lines: Vec<Line<'static>>) {
    if lines.is_empty() {
      return;
    }
    let count = lines.len() as u16;
    let _ = self.terminal.insert_before(count, |buf| {
      let area = Rect::new(0, 0, buf.area().width, count);
      ratatui::widgets::Widget::render(Paragraph::new(lines), area, buf);
    });
  }

  /// Render the viewport: live test progress + status bar.
  fn render(&mut self) {
    let current_test = self.current_test.clone();
    let live_steps = self.live_steps.clone();
    let status = self.status.clone();

    let _ = self.terminal.draw(|frame| {
      let area = frame.area();
      let mut lines: Vec<Line<'_>> = Vec::with_capacity(VIEWPORT_HEIGHT as usize);

      // Current test name.
      if let Some(ref name) = current_test {
        lines.push(Line::from(vec![
          Span::raw("  "),
          Span::styled(
            format!("\u{25cf} {name}"),
            Style::default().fg(Color::Yellow),
          ),
        ]));
      }

      // Live steps.
      for step in &live_steps {
        let (icon, color) = match step.status {
          LiveStepStatus::Running => ("\u{25cf}", Color::Yellow),
          LiveStepStatus::Passed => ("\u{2713}", Color::Green),
          LiveStepStatus::Failed => ("\u{2717}", Color::Red),
          LiveStepStatus::Skipped => ("\u{2212}", Color::DarkGray),
        };
        let mut spans = vec![
          Span::raw("    "),
          Span::styled(format!("{icon} "), Style::default().fg(color)),
          Span::raw(step.title.clone()),
        ];
        if let Some(ms) = step.duration_ms {
          spans.push(Span::styled(
            format!(" ({ms}ms)"),
            Style::default().fg(Color::DarkGray),
          ));
        }
        lines.push(Line::from(spans));
      }

      // Pad to fill viewport before status bar.
      let status_lines = 3usize; // separator + status + hints
      let content_lines = lines.len();
      let available = (VIEWPORT_HEIGHT as usize).saturating_sub(status_lines);
      for _ in content_lines..available {
        lines.push(Line::raw(""));
      }

      // Separator.
      let width = area.width as usize;
      lines.push(Line::styled(
        "\u{2500}".repeat(width),
        Style::default().fg(Color::DarkGray),
      ));

      // Status line.
      lines.push(render_status_line(&status));

      // Key hints.
      lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("a", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(" all", Style::default().fg(Color::DarkGray)),
        Span::styled(" \u{00b7} ", Style::default().fg(Color::DarkGray)),
        Span::styled("f", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(" failed", Style::default().fg(Color::DarkGray)),
        Span::styled(" \u{00b7} ", Style::default().fg(Color::DarkGray)),
        Span::styled("p", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(" filter", Style::default().fg(Color::DarkGray)),
        Span::styled(" \u{00b7} ", Style::default().fg(Color::DarkGray)),
        Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(" quit", Style::default().fg(Color::DarkGray)),
      ]));

      // Truncate to viewport height.
      lines.truncate(VIEWPORT_HEIGHT as usize);

      frame.render_widget(Paragraph::new(lines), area);
    });
  }

  pub fn set_status(&mut self, status: WatchStatus) {
    self.status = status;
    self.render();
  }

  /// Process a TUI message.
  fn handle_message(&mut self, msg: TuiMessage) {
    match msg {
      TuiMessage::Scrollback(lines) => self.push_to_scrollback(lines),
      TuiMessage::CurrentTest(name) => {
        self.current_test = name;
        self.live_steps.clear();
        self.render();
      }
      TuiMessage::LiveStep(step) => {
        // Update existing step or add new one.
        if let Some(existing) = self.live_steps.iter_mut().find(|s| s.title == step.title) {
          existing.status = step.status;
          existing.duration_ms = step.duration_ms;
        } else {
          self.live_steps.push(step);
        }
        // Keep viewport from overflowing — show only last N steps.
        let max_steps = (VIEWPORT_HEIGHT as usize).saturating_sub(5);
        if self.live_steps.len() > max_steps {
          let drain_count = self.live_steps.len() - max_steps;
          self.live_steps.drain(..drain_count);
        }
        self.render();
      }
      TuiMessage::ClearLive => {
        self.current_test = None;
        self.live_steps.clear();
        self.render();
      }
      TuiMessage::Status(status) => {
        self.status = status;
        self.render();
      }
    }
  }

  /// Drain all pending messages.
  pub fn flush(&mut self) {
    while let Ok(msg) = self.msg_rx.try_recv() {
      self.handle_message(msg);
    }
  }

  /// Wait for the next keyboard command. While waiting, processes reporter
  /// messages and updates the display in real-time.
  pub async fn next_command(&mut self) -> Option<WatchCommand> {
    loop {
      tokio::select! {
        msg = self.msg_rx.recv() => {
          self.handle_message(msg?);
        }
        event = self.event_stream.next() => {
          let Some(Ok(event)) = event else { return None };
          if let Event::Key(key) = event {
            if let Some(cmd) = map_key_event(key) {
              return Some(cmd);
            }
          } else if let Event::Resize(_, _) = event {
            self.render();
          }
        }
      }
    }
  }

  pub fn shutdown(&mut self) {
    let _ = self.terminal.clear();
    let _ = crossterm::terminal::disable_raw_mode();
  }
}

impl Drop for WatchTui {
  fn drop(&mut self) {
    self.shutdown();
  }
}

// ── Helpers ────────────────────────────────────────────────────────────

fn render_status_line(status: &WatchStatus) -> Line<'static> {
  match status {
    WatchStatus::Idle => Line::from(vec![
      Span::styled("  Watching for changes...", Style::default().fg(Color::DarkGray)),
    ]),
    WatchStatus::Running { completed, total, start } => {
      let elapsed = start.elapsed();
      let pct = if *total > 0 { (*completed as f64 / *total as f64) * 100.0 } else { 0.0 };
      let bar_w = 15;
      let filled = (pct / 100.0 * bar_w as f64) as usize;
      let empty = bar_w - filled;
      let bar = format!("{}{}", "\u{2588}".repeat(filled), "\u{2591}".repeat(empty));
      Line::from(vec![
        Span::styled("  \u{25cf} ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        Span::raw(format!("Running {completed}/{total}  ")),
        Span::styled(bar, Style::default().fg(Color::Yellow)),
        Span::raw(format!("  {:.0}%", pct)),
        Span::styled(format!("    {:.1}s", elapsed.as_secs_f64()), Style::default().fg(Color::DarkGray)),
      ])
    }
    WatchStatus::Done { passed, failed, skipped, flaky, duration } => {
      let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
      if *passed > 0 {
        spans.push(Span::styled(format!("\u{2713} {passed} passed"), Style::default().fg(Color::Green)));
        spans.push(Span::raw("  "));
      }
      if *failed > 0 {
        spans.push(Span::styled(format!("\u{2717} {failed} failed"), Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)));
        spans.push(Span::raw("  "));
      }
      if *flaky > 0 {
        spans.push(Span::styled(format!("\u{26a0} {flaky} flaky"), Style::default().fg(Color::Yellow)));
        spans.push(Span::raw("  "));
      }
      if *skipped > 0 {
        spans.push(Span::styled(format!("\u{2212} {skipped} skipped"), Style::default().fg(Color::DarkGray)));
        spans.push(Span::raw("  "));
      }
      spans.push(Span::styled(
        format!("{:.1}s", duration.as_secs_f64()),
        Style::default().fg(Color::DarkGray),
      ));
      Line::from(spans)
    }
  }
}

fn map_key_event(key: crossterm::event::KeyEvent) -> Option<WatchCommand> {
  if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
    return Some(WatchCommand::Quit);
  }
  match key.code {
    KeyCode::Char('a') => Some(WatchCommand::RunAll),
    KeyCode::Char('f') => Some(WatchCommand::RunFailed),
    KeyCode::Char('q') => Some(WatchCommand::Quit),
    KeyCode::Enter => Some(WatchCommand::Rerun),
    KeyCode::Char('p') => Some(WatchCommand::FilterByName(String::new())),
    _ => None,
  }
}

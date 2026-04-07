//! Watch mode TUI: fullscreen ratatui dashboard with real-time test progress.
//!
//! Uses alternate screen for fullscreen, `Layout` for header/body/footer,
//! `Paragraph::scroll()` for test list scrolling, `Scrollbar` for position.
//!
//! Styling follows Vitest/Jest conventions:
//! - Passed: green checkmark, test name in default white (not green)
//! - Failed: red cross, test name in red, error in dim red
//! - Running: yellow spinner dot, test name in white
//! - Skipped: dim gray dash, test name in dim gray
//! - Steps: indented, icon matches status, title in dim white
//! - Duration: always dim gray

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::interactive::WatchCommand;

// ── Messages ───────────────────────────────────────────────────────────

/// Messages from the TUI reporter to the TUI dashboard.
pub enum TuiMessage {
  /// A test run is starting.
  RunStarted {
    total: usize,
    workers: u32,
    names: Vec<TestEntry>,
  },
  /// A test began executing.
  TestStarted { name: String },
  /// A step within a running test started or finished.
  StepUpdate { test_name: String, step_title: String, status: EntryStatus, duration_ms: Option<u64> },
  /// A test finished with a result.
  TestFinished {
    name: String,
    status: EntryStatus,
    duration: Duration,
    error: Option<String>,
  },
  /// The run is complete.
  RunFinished {
    passed: usize,
    failed: usize,
    skipped: usize,
    flaky: usize,
    duration: Duration,
  },
}

/// A test entry in the dashboard.
#[derive(Clone)]
pub struct TestEntry {
  pub name: String,
  pub status: EntryStatus,
  pub duration: Option<Duration>,
  pub steps: Vec<StepEntry>,
  pub error: Option<String>,
}

/// A step within a test entry.
#[derive(Clone)]
pub struct StepEntry {
  pub title: String,
  pub status: EntryStatus,
  pub duration_ms: Option<u64>,
}

/// Status of a test/step entry.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EntryStatus {
  Pending,
  Running,
  Passed,
  Failed,
  Skipped,
  Flaky,
}

/// Status bar state.
#[derive(Clone)]
pub enum WatchStatus {
  Idle,
  Running { completed: usize, total: usize, start: Instant },
  Done { passed: usize, failed: usize, skipped: usize, flaky: usize, duration: Duration },
}

/// Result of `drain_while_running()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainResult {
  Completed,
  Cancelled,
  ChannelClosed,
}

// ── Style constants ───────────────────────────────────────────────────

const CLR_PASS: Color = Color::Green;
const CLR_FAIL: Color = Color::Red;
const CLR_RUN: Color = Color::Yellow;
const CLR_FLAKY: Color = Color::Yellow;
const CLR_DIM: Color = Color::DarkGray;
const CLR_CYAN: Color = Color::Cyan;

const ICON_PASS: &str = "\u{2713}"; // checkmark
const ICON_FAIL: &str = "\u{2717}"; // cross
const ICON_RUN: &str = "\u{25cf}";  // filled circle
const ICON_SKIP: &str = "\u{2212}"; // minus
const ICON_PEND: &str = "\u{25cb}"; // empty circle
const ICON_FLAKY: &str = "\u{25ce}"; // bullseye

// ── WatchTui ───────────────────────────────────────────────────────────

pub struct WatchTui {
  terminal: Terminal<CrosstermBackend<Stdout>>,
  event_stream: EventStream,
  msg_rx: mpsc::UnboundedReceiver<TuiMessage>,
  status: WatchStatus,
  entries: Vec<TestEntry>,
  total_tests: usize,
  num_workers: u32,
  completed: usize,
  run_start: Instant,
  scroll_offset: usize,
  total_content_lines: usize,
  /// Whether a run is in progress (changes key hint text).
  is_running: bool,
  /// Active filter input (Some = filter mode active, None = normal mode).
  filter_input: Option<String>,
  /// The current active grep filter (shown in header).
  pub active_filter: Option<String>,
}

impl WatchTui {
  pub fn new() -> Result<(Self, mpsc::UnboundedSender<TuiMessage>), String> {
    crossterm::terminal::enable_raw_mode().map_err(|e| format!("enable raw mode: {e}"))?;
    execute!(io::stdout(), EnterAlternateScreen).map_err(|e| format!("enter alternate screen: {e}"))?;

    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend).map_err(|e| format!("create terminal: {e}"))?;

    let (msg_tx, msg_rx) = mpsc::unbounded_channel();

    let tui = Self {
      terminal,
      event_stream: EventStream::new(),
      msg_rx,
      status: WatchStatus::Idle,
      entries: Vec::new(),
      total_tests: 0,
      num_workers: 0,
      completed: 0,
      run_start: Instant::now(),
      scroll_offset: 0,
      total_content_lines: 0,
      is_running: false,
      filter_input: None,
      active_filter: None,
    };

    Ok((tui, msg_tx))
  }

  // ── Message handling ──

  fn handle_message(&mut self, msg: TuiMessage) {
    match msg {
      TuiMessage::RunStarted { total, workers, names } => {
        self.total_tests = total;
        self.num_workers = workers;
        self.completed = 0;
        self.run_start = Instant::now();
        self.scroll_offset = 0;
        self.entries = names;
        self.is_running = true;
        self.status = WatchStatus::Running {
          completed: 0,
          total,
          start: self.run_start,
        };
        self.render();
      }
      TuiMessage::TestStarted { name } => {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.name == name) {
          entry.status = EntryStatus::Running;
          entry.steps.clear();
          entry.error = None;
        } else {
          self.entries.push(TestEntry {
            name,
            status: EntryStatus::Running,
            duration: None,
            steps: Vec::new(),
            error: None,
          });
        }
        self.auto_scroll_to_running();
        self.render();
      }
      TuiMessage::StepUpdate { test_name, step_title, status, duration_ms } => {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.name == test_name) {
          if let Some(step) = entry.steps.iter_mut().find(|s| s.title == step_title) {
            step.status = status;
            step.duration_ms = duration_ms;
          } else {
            entry.steps.push(StepEntry { title: step_title, status, duration_ms });
          }
        }
        self.render();
      }
      TuiMessage::TestFinished { name, status, duration, error } => {
        self.completed += 1;
        if let Some(entry) = self.entries.iter_mut().find(|e| e.name == name) {
          entry.status = status;
          entry.duration = Some(duration);
          entry.error = error;
        } else {
          self.entries.push(TestEntry {
            name, status, duration: Some(duration), steps: Vec::new(), error,
          });
        }
        self.status = WatchStatus::Running {
          completed: self.completed,
          total: self.total_tests,
          start: self.run_start,
        };
        self.render();
      }
      TuiMessage::RunFinished { passed, failed, skipped, flaky, duration } => {
        self.is_running = false;
        self.status = WatchStatus::Done { passed, failed, skipped, flaky, duration };
        self.render();
      }
    }
  }

  fn body_height(&mut self) -> usize {
    // header(2) + footer(3) = 5 reserved
    (self.terminal.get_frame().area().height as usize).saturating_sub(5)
  }

  // ── Scrolling ──

  fn auto_scroll_to_running(&mut self) {
    let visible = self.body_height();
    if visible == 0 { return; }

    let mut target_line = 0usize;
    let mut found = false;
    for entry in &self.entries {
      if entry.status == EntryStatus::Running {
        found = true;
        break;
      }
      target_line += 1 + entry.steps.len();
      if entry.status == EntryStatus::Failed && entry.error.is_some() {
        target_line += 1;
      }
    }
    if !found { return; }

    let viewport_end = self.scroll_offset + visible;
    if target_line < self.scroll_offset {
      self.scroll_offset = target_line;
    } else if target_line >= viewport_end {
      let context = visible / 3;
      self.scroll_offset = target_line.saturating_sub(visible.saturating_sub(context));
    }
  }

  fn scroll_by(&mut self, delta: isize, visible_height: usize) {
    let max = self.total_content_lines.saturating_sub(visible_height);
    if delta < 0 {
      self.scroll_offset = self.scroll_offset.saturating_sub(delta.unsigned_abs());
    } else {
      self.scroll_offset = (self.scroll_offset + delta as usize).min(max);
    }
  }

  // ── Content building ──

  fn build_content_lines(entries: &[TestEntry], width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for entry in entries {
      // ── Test name line ──
      let (icon, icon_color) = status_icon(entry.status);
      let name_style = match entry.status {
        EntryStatus::Failed => Style::default().fg(CLR_FAIL),
        EntryStatus::Skipped | EntryStatus::Pending => Style::default().fg(CLR_DIM),
        EntryStatus::Running => Style::default().fg(Color::White),
        _ => Style::default(), // default terminal color for passed
      };

      let mut spans = vec![
        Span::raw(" "),
        Span::styled(format!(" {icon} "), Style::default().fg(icon_color)),
        Span::styled(entry.name.clone(), name_style),
      ];

      if let Some(dur) = entry.duration {
        spans.push(Span::styled(
          format!(" ({:.0}ms)", dur.as_millis()),
          Style::default().fg(CLR_DIM),
        ));
      }

      lines.push(Line::from(spans));

      // ── Steps ──
      for step in &entry.steps {
        let (sicon, sicon_color) = status_icon(step.status);
        let step_name_style = match step.status {
          EntryStatus::Failed => Style::default().fg(CLR_FAIL),
          EntryStatus::Running => Style::default().fg(CLR_RUN),
          _ => Style::default().fg(CLR_DIM),
        };

        let mut step_spans = vec![
          Span::raw("      "),
          Span::styled(format!("{sicon} "), Style::default().fg(sicon_color)),
          Span::styled(step.title.clone(), step_name_style),
        ];

        if let Some(ms) = step.duration_ms {
          step_spans.push(Span::styled(
            format!(" ({ms}ms)"),
            Style::default().fg(CLR_DIM),
          ));
        }

        lines.push(Line::from(step_spans));
      }

      // ── Error ──
      if entry.status == EntryStatus::Failed {
        if let Some(ref err) = entry.error {
          for err_line in err.lines().take(3) {
            if !err_line.is_empty() {
              lines.push(Line::from(vec![
                Span::raw("      "),
                Span::styled(
                  truncate_str(err_line, width.saturating_sub(8)),
                  Style::default().fg(CLR_FAIL).add_modifier(Modifier::DIM),
                ),
              ]));
            }
          }
        }
      }
    }

    lines
  }

  // ── Rendering ──

  fn render(&mut self) {
    let entries = self.entries.clone();
    let status = self.status.clone();
    let total_tests = self.total_tests;
    let num_workers = self.num_workers;
    let scroll_offset = self.scroll_offset;
    let is_running = self.is_running;
    let filter_input = self.filter_input.clone();
    let active_filter = self.active_filter.clone();

    let _ = self.terminal.draw(|frame| {
      let area = frame.area();
      let width = area.width as usize;

      // Layout: header(2) | body(fill) | footer(3)
      let [header_area, body_area, footer_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(3),
      ]).areas(area);

      // ── Header ──
      let mut header_lines = render_header(&status, total_tests, num_workers);
      // Show active filter in header.
      if let Some(ref pattern) = active_filter {
        header_lines[1] = Line::from(vec![
          Span::raw(" "),
          Span::styled("Filter: ", Style::default().fg(CLR_DIM)),
          Span::styled(pattern.clone(), Style::default().fg(CLR_CYAN).add_modifier(Modifier::BOLD)),
        ]);
      }
      frame.render_widget(Paragraph::new(header_lines), header_area);

      // ── Scrollable test list ──
      let content_lines = Self::build_content_lines(&entries, width);
      let total_lines = content_lines.len();

      let paragraph = Paragraph::new(content_lines)
        .scroll((scroll_offset as u16, 0));
      frame.render_widget(paragraph, body_area);

      // ── Scrollbar ──
      if total_lines > body_area.height as usize {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        let max_scroll = total_lines.saturating_sub(body_area.height as usize);
        let mut scrollbar_state = ScrollbarState::new(max_scroll).position(scroll_offset);
        frame.render_stateful_widget(scrollbar, body_area, &mut scrollbar_state);
      }

      // ── Footer ──
      let [sep_area, status_area, hints_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
      ]).areas(footer_area);

      frame.render_widget(
        Paragraph::new(Line::styled("\u{2500}".repeat(width), Style::default().fg(CLR_DIM))),
        sep_area,
      );
      frame.render_widget(Paragraph::new(render_status_line(&status, width)), status_area);

      // Hints line: filter input mode or normal hints.
      let hints_line = if let Some(ref input) = filter_input {
        Line::from(vec![
          Span::raw(" "),
          Span::styled("Filter pattern: ", Style::default().fg(CLR_CYAN).add_modifier(Modifier::BOLD)),
          Span::styled(input.clone(), Style::default().fg(Color::White)),
          Span::styled("\u{2588}", Style::default().fg(Color::White)), // cursor block
          Span::styled("  (Enter to apply, Esc to cancel)", Style::default().fg(CLR_DIM)),
        ])
      } else {
        render_hints(is_running, active_filter.is_some())
      };
      frame.render_widget(Paragraph::new(hints_line), hints_area);
    });

    // Update total content lines outside draw closure.
    let width = self.terminal.get_frame().area().width as usize;
    self.total_content_lines = Self::build_content_lines(&self.entries, width).len();
  }

  pub fn set_status(&mut self, status: WatchStatus) {
    self.status = status;
    self.render();
  }

  pub fn flush(&mut self) {
    while let Ok(msg) = self.msg_rx.try_recv() {
      self.handle_message(msg);
    }
  }

  pub async fn drain_while_running(&mut self) -> DrainResult {
    loop {
      tokio::select! {
        msg = self.msg_rx.recv() => {
          match msg {
            Some(msg) => {
              let is_done = matches!(&msg, TuiMessage::RunFinished { .. });
              self.handle_message(msg);
              if is_done { return DrainResult::Completed; }
            }
            None => return DrainResult::ChannelClosed,
          }
        }
        event = self.event_stream.next() => {
          let Some(Ok(event)) = event else { continue };
          if let Event::Key(key) = event {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
              return DrainResult::Cancelled;
            }
            if key.code == KeyCode::Char('q') {
              return DrainResult::Cancelled;
            }
            let body_height = self.terminal.get_frame().area().height.saturating_sub(5) as usize;
            match key.code {
              KeyCode::Up | KeyCode::Char('k') => { self.scroll_by(-1, body_height); self.render(); }
              KeyCode::Down | KeyCode::Char('j') => { self.scroll_by(1, body_height); self.render(); }
              KeyCode::PageUp => { self.scroll_by(-(body_height as isize), body_height); self.render(); }
              KeyCode::PageDown => { self.scroll_by(body_height as isize, body_height); self.render(); }
              _ => {}
            }
          } else if let Event::Resize(_, _) = event {
            self.render();
          }
        }
      }
    }
  }

  pub async fn next_command(&mut self) -> Option<WatchCommand> {
    loop {
      tokio::select! {
        msg = self.msg_rx.recv() => {
          self.handle_message(msg?);
        }
        event = self.event_stream.next() => {
          let Some(Ok(event)) = event else { return None };
          if let Event::Key(key) = event {
            // ── Filter input mode ──
            if self.filter_input.is_some() {
              match key.code {
                KeyCode::Enter => {
                  let pattern = self.filter_input.take().unwrap_or_default();
                  if !pattern.is_empty() {
                    self.active_filter = Some(pattern.clone());
                    self.render();
                    return Some(WatchCommand::FilterByName(pattern));
                  }
                  self.render();
                  continue;
                }
                KeyCode::Esc => {
                  self.filter_input = None;
                  self.render();
                  continue;
                }
                KeyCode::Backspace => {
                  if let Some(ref mut input) = self.filter_input {
                    input.pop();
                  }
                  self.render();
                  continue;
                }
                KeyCode::Char(c) => {
                  if let Some(ref mut input) = self.filter_input {
                    input.push(c);
                  }
                  self.render();
                  continue;
                }
                _ => continue,
              }
            }

            // ── Normal mode ──
            // Ctrl+C always quits.
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
              return Some(WatchCommand::Quit);
            }

            // Scroll keys.
            let body_height = self.body_height();
            match key.code {
              KeyCode::Up | KeyCode::Char('k') => { self.scroll_by(-1, body_height); self.render(); continue; }
              KeyCode::Down | KeyCode::Char('j') => { self.scroll_by(1, body_height); self.render(); continue; }
              KeyCode::PageUp => { self.scroll_by(-(body_height as isize), body_height); self.render(); continue; }
              KeyCode::PageDown => { self.scroll_by(body_height as isize, body_height); self.render(); continue; }
              _ => {}
            }

            // Command keys.
            match key.code {
              KeyCode::Char('p') => {
                // Enter filter input mode.
                self.filter_input = Some(String::new());
                self.render();
                continue;
              }
              KeyCode::Char('c') => {
                // Clear active filter.
                if self.active_filter.is_some() {
                  self.active_filter = None;
                  self.render();
                  return Some(WatchCommand::RunAll);
                }
              }
              _ => {}
            }
            if let Some(cmd) = map_key_event(key) { return Some(cmd); }
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
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
  }
}

impl Drop for WatchTui {
  fn drop(&mut self) {
    self.shutdown();
  }
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Icon + color for a status.
fn status_icon(status: EntryStatus) -> (&'static str, Color) {
  match status {
    EntryStatus::Pending => (ICON_PEND, CLR_DIM),
    EntryStatus::Running => (ICON_RUN, CLR_RUN),
    EntryStatus::Passed => (ICON_PASS, CLR_PASS),
    EntryStatus::Failed => (ICON_FAIL, CLR_FAIL),
    EntryStatus::Skipped => (ICON_SKIP, CLR_DIM),
    EntryStatus::Flaky => (ICON_FLAKY, CLR_FLAKY),
  }
}

/// Header: title line + blank or status summary.
fn render_header(status: &WatchStatus, total: usize, workers: u32) -> Vec<Line<'static>> {
  match status {
    WatchStatus::Idle => vec![
      Line::from(vec![
        Span::styled(" WATCH ", Style::default().fg(Color::Black).bg(CLR_CYAN).add_modifier(Modifier::BOLD)),
        Span::styled("  Watching for changes...", Style::default().fg(CLR_DIM)),
      ]),
      Line::raw(""),
    ],
    WatchStatus::Running { .. } => vec![
      Line::from(vec![
        Span::styled(" RUNS ", Style::default().fg(Color::Black).bg(CLR_RUN).add_modifier(Modifier::BOLD)),
        Span::raw(format!("  {total} test(s) with {workers} worker(s)")),
      ]),
      Line::raw(""),
    ],
    WatchStatus::Done { passed, failed, skipped, flaky, .. } => {
      let badge = if *failed > 0 {
        Span::styled(" FAIL ", Style::default().fg(Color::White).bg(CLR_FAIL).add_modifier(Modifier::BOLD))
      } else {
        Span::styled(" PASS ", Style::default().fg(Color::Black).bg(CLR_PASS).add_modifier(Modifier::BOLD))
      };
      let mut summary = vec![badge, Span::raw("  ")];
      if *passed > 0 {
        summary.push(Span::styled(format!("{passed} passed"), Style::default().fg(CLR_PASS).add_modifier(Modifier::BOLD)));
      }
      if *failed > 0 {
        if *passed > 0 { summary.push(Span::styled(", ", Style::default().fg(CLR_DIM))); }
        summary.push(Span::styled(format!("{failed} failed"), Style::default().fg(CLR_FAIL).add_modifier(Modifier::BOLD)));
      }
      if *flaky > 0 {
        summary.push(Span::styled(", ", Style::default().fg(CLR_DIM)));
        summary.push(Span::styled(format!("{flaky} flaky"), Style::default().fg(CLR_FLAKY)));
      }
      if *skipped > 0 {
        summary.push(Span::styled(", ", Style::default().fg(CLR_DIM)));
        summary.push(Span::styled(format!("{skipped} skipped"), Style::default().fg(CLR_DIM)));
      }
      let total = passed + failed + skipped;
      summary.push(Span::styled(format!("  ({total} total)"), Style::default().fg(CLR_DIM)));
      vec![Line::from(summary), Line::raw("")]
    }
  }
}

/// Status/progress line.
fn render_status_line(status: &WatchStatus, width: usize) -> Line<'static> {
  match status {
    WatchStatus::Idle => Line::from(vec![
      Span::raw(" "),
      Span::styled("Press ", Style::default().fg(CLR_DIM)),
      Span::styled("a", Style::default().add_modifier(Modifier::BOLD)),
      Span::styled(" to run all, ", Style::default().fg(CLR_DIM)),
      Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
      Span::styled(" to quit", Style::default().fg(CLR_DIM)),
    ]),
    WatchStatus::Running { completed, total, start } => {
      let elapsed = start.elapsed();
      let pct = if *total > 0 { (*completed as f64 / *total as f64) * 100.0 } else { 0.0 };
      let bar_w = (width / 3).max(10).min(40);
      let filled = (pct / 100.0 * bar_w as f64) as usize;
      let empty = bar_w - filled;
      Line::from(vec![
        Span::raw(" "),
        Span::styled("Tests ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!("{completed}"), Style::default().fg(CLR_PASS).add_modifier(Modifier::BOLD)),
        Span::styled(format!("/{total}  "), Style::default().fg(CLR_DIM)),
        Span::styled("\u{2588}".repeat(filled), Style::default().fg(CLR_PASS)),
        Span::styled("\u{2591}".repeat(empty), Style::default().fg(CLR_DIM)),
        Span::styled(format!("  {:.0}%", pct), Style::default().fg(CLR_DIM)),
        Span::styled(format!("  {:.1}s", elapsed.as_secs_f64()), Style::default().fg(CLR_DIM)),
      ])
    }
    WatchStatus::Done { duration, .. } => {
      Line::from(vec![
        Span::raw(" "),
        Span::styled("Time  ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:.2}s", duration.as_secs_f64()), Style::default().fg(CLR_DIM)),
      ])
    }
  }
}

/// Key hints — context-aware (different during run vs idle, with/without filter).
fn render_hints(is_running: bool, has_filter: bool) -> Line<'static> {
  if is_running {
    Line::from(vec![
      Span::raw(" "),
      Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
      Span::styled(" cancel", Style::default().fg(CLR_DIM)),
      Span::styled("  ", Style::default().fg(CLR_DIM)),
      Span::styled("j/k", Style::default().add_modifier(Modifier::BOLD)),
      Span::styled(" scroll", Style::default().fg(CLR_DIM)),
    ])
  } else {
    let mut spans = vec![
      Span::raw(" "),
      Span::styled("a", Style::default().add_modifier(Modifier::BOLD)),
      Span::styled(" run all", Style::default().fg(CLR_DIM)),
      Span::styled("  ", Style::default().fg(CLR_DIM)),
      Span::styled("f", Style::default().add_modifier(Modifier::BOLD)),
      Span::styled(" failed", Style::default().fg(CLR_DIM)),
      Span::styled("  ", Style::default().fg(CLR_DIM)),
      Span::styled("p", Style::default().add_modifier(Modifier::BOLD)),
      Span::styled(" filter", Style::default().fg(CLR_DIM)),
    ];
    if has_filter {
      spans.extend([
        Span::styled("  ", Style::default().fg(CLR_DIM)),
        Span::styled("c", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(" clear filter", Style::default().fg(CLR_DIM)),
      ]);
    }
    spans.extend([
      Span::styled("  ", Style::default().fg(CLR_DIM)),
      Span::styled("j/k", Style::default().add_modifier(Modifier::BOLD)),
      Span::styled(" scroll", Style::default().fg(CLR_DIM)),
      Span::styled("  ", Style::default().fg(CLR_DIM)),
      Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
      Span::styled(" quit", Style::default().fg(CLR_DIM)),
    ]);
    Line::from(spans)
  }
}

fn truncate_str(s: &str, max_len: usize) -> String {
  if s.len() <= max_len {
    s.to_string()
  } else {
    format!("{}...", &s[..max_len.saturating_sub(3)])
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
    // 'p' and 'c' handled in next_command() directly (filter mode).
    _ => None,
  }
}

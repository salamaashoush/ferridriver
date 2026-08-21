//! An in-place progress line for work that takes long enough to watch.
//!
//! The reason this exists: `install` reported download progress by printing
//! one line per HTTP chunk, so fetching a ~150MB browser buried the terminal
//! under thousands of lines and the user could not see the step that failed.
//! Redraws are throttled and land on a single line that is erased when the
//! step ends, leaving one summary line behind per step.
//!
//! Where there is no terminal — a pipe, CI, `--quiet`, `--format json` — the
//! bar is inert and only the summary lines are written, which is what a log
//! wants anyway.

use std::io::IsTerminal as _;
use std::time::{Duration, Instant};

use console::{Style, Term};

/// Minimum gap between redraws. Fast enough to look continuous, slow enough
/// that the terminal is not the bottleneck on a fast link.
const REDRAW: Duration = Duration::from_millis(80);

/// Characters in the bar itself.
const BAR_WIDTH: usize = 24;

pub struct Progress {
  term: Term,
  live: bool,
  label: String,
  started: Instant,
  last_draw: Option<Instant>,
  /// Set once the line has something on it, so `clear` knows whether there is
  /// anything to erase.
  drawn: bool,
}

impl Progress {
  /// Start a step. Nothing is drawn until the first [`Progress::set`] or
  /// [`Progress::tick`], so a step that finishes instantly stays silent.
  #[must_use]
  pub fn new(label: impl Into<String>) -> Self {
    let term = Term::stderr();
    let live = term.is_term() && std::io::stderr().is_terminal() && !super::quiet() && !super::json();
    Self {
      term,
      live,
      label: label.into(),
      started: Instant::now(),
      last_draw: None,
      drawn: false,
    }
  }

  /// Report byte progress. `total` is absent when the server sent no
  /// `Content-Length`, in which case the count is shown without a bar.
  pub fn set(&mut self, done: u64, total: Option<u64>) {
    if !self.live || !self.due() {
      return;
    }
    let elapsed = self.started.elapsed();
    #[allow(clippy::cast_precision_loss)]
    let rate = if elapsed.as_secs_f64() > 0.0 {
      done as f64 / elapsed.as_secs_f64()
    } else {
      0.0
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rate = super::bytes(rate as u64);
    let body = match total {
      Some(total) if total > 0 => {
        // A bar 24 cells wide cannot express more precision than an f64
        // already carries; the cast is the point, not an accident.
        #[allow(clippy::cast_precision_loss)]
        let done_frac = (done as f64 / total as f64).clamp(0.0, 1.0);
        #[allow(
          clippy::cast_possible_truncation,
          clippy::cast_sign_loss,
          clippy::cast_precision_loss
        )]
        let filled = (done_frac * BAR_WIDTH as f64).round() as usize;
        let bar = format!(
          "{}{}",
          "█".repeat(filled),
          Style::new().dim().apply_to("░".repeat(BAR_WIDTH - filled))
        );
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let pct = (done_frac * 100.0).round() as u64;
        format!(
          "{bar} {pct:>3}%  {} / {}  {rate}/s",
          super::bytes(done),
          super::bytes(total)
        )
      },
      _ => format!("{}  {rate}/s", super::bytes(done)),
    };
    self.draw(&body);
  }

  /// Report an unmeasured step by name, for phases with no byte count.
  pub fn tick(&mut self, detail: &str) {
    if !self.live || !self.due() {
      return;
    }
    let elapsed = super::duration(self.started.elapsed());
    self.draw(&format!("{detail} {}", Style::new().dim().apply_to(elapsed)));
  }

  /// End the step with a success line, replacing the bar.
  pub fn finish_ok(mut self, summary: &str) {
    self.clear();
    let took = Style::new()
      .dim()
      .apply_to(format!("({})", super::duration(self.started.elapsed())));
    self.line(&format!("{} {took}", super::success(summary)));
  }

  /// End the step with a failure line, replacing the bar.
  pub fn finish_fail(mut self, summary: &str) {
    self.clear();
    self.line(&super::failure(summary));
  }

  /// End the step without claiming an outcome — the step did nothing, or its
  /// result is reported elsewhere.
  pub fn finish_quiet(mut self, summary: &str) {
    self.clear();
    self.line(&super::info(summary));
  }

  /// Whether enough time has passed since the last redraw to draw again.
  fn due(&mut self) -> bool {
    let now = Instant::now();
    match self.last_draw {
      Some(last) if now.duration_since(last) < REDRAW => false,
      _ => {
        self.last_draw = Some(now);
        true
      },
    }
  }

  fn draw(&mut self, body: &str) {
    if self.drawn {
      let _ = self.term.clear_line();
    }
    let label = Style::new().cyan().apply_to(&self.label);
    let _ = self.term.write_str(&format!("\r{label}  {body}"));
    let _ = self.term.flush();
    self.drawn = true;
  }

  fn clear(&mut self) {
    if self.drawn {
      let _ = self.term.clear_line();
      let _ = self.term.write_str("\r");
      self.drawn = false;
    }
  }

  /// Summary lines go to stderr with the bar, so a command whose real output
  /// is a document on stdout stays parseable while it narrates.
  fn line(&self, text: &str) {
    if super::quiet() || super::json() {
      return;
    }
    let _ = self.term.write_line(text);
  }
}

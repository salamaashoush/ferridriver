//! TUI reporter: sends real-time test progress to the WatchTui.
//!
//! Sends live step updates during execution (StepStarted → running,
//! StepFinished → passed/failed). Completed tests are pushed to
//! scrollback. The TUI renders live steps in the viewport.

use std::time::Instant;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::mpsc;

use crate::config::RunMode;
use crate::model::{StepCategory, StepStatus, TestStatus};
use crate::reporter::ReporterEvent;
use crate::tui::{LiveStep, LiveStepStatus, TuiMessage, WatchStatus};

pub struct TuiReporter {
  tx: mpsc::UnboundedSender<TuiMessage>,
  mode: RunMode,
  completed: usize,
  total: usize,
  start: Instant,
  current_suite: Option<String>,
}

impl TuiReporter {
  pub fn new(tx: mpsc::UnboundedSender<TuiMessage>, mode: RunMode) -> Self {
    Self {
      tx,
      mode,
      completed: 0,
      total: 0,
      start: Instant::now(),
      current_suite: None,
    }
  }

  fn send(&self, msg: TuiMessage) {
    let _ = self.tx.send(msg);
  }
}

#[async_trait::async_trait]
impl crate::reporter::Reporter for TuiReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::RunStarted { total_tests, num_workers } => {
        self.total = *total_tests;
        self.completed = 0;
        self.start = Instant::now();
        self.current_suite = None;

        self.send(TuiMessage::Scrollback(vec![
          Line::raw(""),
          Line::from(vec![
            Span::raw("  Running "),
            Span::styled(format!("{total_tests}"), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!(" test(s) with {num_workers} worker(s)")),
          ]),
          Line::raw(""),
        ]));
        self.send(TuiMessage::Status(WatchStatus::Running {
          completed: 0,
          total: *total_tests,
          start: self.start,
        }));
      }

      ReporterEvent::TestStarted { test_id, .. } => {
        // Show the test name in the viewport as "currently running".
        let name = if self.mode == RunMode::Bdd {
          // Show feature header if changed.
          let suite = test_id.suite.as_deref().unwrap_or("");
          if self.current_suite.as_deref() != Some(suite) && !suite.is_empty() {
            self.current_suite = Some(suite.to_string());
            self.send(TuiMessage::Scrollback(vec![
              Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("Feature: {suite}"), Style::default().add_modifier(Modifier::BOLD)),
              ]),
            ]));
          }
          format!("Scenario: {}", test_id.name)
        } else {
          test_id.full_name()
        };
        self.send(TuiMessage::CurrentTest(Some(name)));
      }

      ReporterEvent::StepStarted(step) => {
        // Show step as "running" in the viewport.
        if step.category == StepCategory::TestStep || self.mode == RunMode::Bdd {
          self.send(TuiMessage::LiveStep(LiveStep {
            title: step.title.clone(),
            status: LiveStepStatus::Running,
            duration_ms: None,
          }));
        }
      }

      ReporterEvent::StepFinished(step) => {
        // Update step status in the viewport.
        if step.category == StepCategory::TestStep || self.mode == RunMode::Bdd {
          let status = if step.error.is_some() {
            LiveStepStatus::Failed
          } else {
            LiveStepStatus::Passed
          };
          self.send(TuiMessage::LiveStep(LiveStep {
            title: step.title.clone(),
            status,
            duration_ms: Some(step.duration.as_millis() as u64),
          }));
        }
      }

      ReporterEvent::TestFinished { test_id, outcome } => {
        self.completed += 1;

        // Move the completed test result to scrollback.
        let (icon, color) = status_style(&outcome.status);
        let name = if self.mode == RunMode::Bdd {
          format!("    {icon} {} ({:.0}ms)", test_id.name, outcome.duration.as_millis())
        } else {
          format!("  {icon} {} ({:.0}ms)", test_id.full_name(), outcome.duration.as_millis())
        };

        let mut scrollback_lines = vec![Line::styled(name, Style::default().fg(color))];

        // Show steps in scrollback for BDD mode.
        if self.mode == RunMode::Bdd {
          for step in &outcome.steps {
            if step.category == StepCategory::TestStep {
              let (sicon, scolor) = step_style(&step.status);
              scrollback_lines.push(Line::from(vec![
                Span::raw("      "),
                Span::styled(format!("{sicon} "), Style::default().fg(scolor)),
                Span::raw(format!("{} ", step.title)),
                Span::styled(
                  format!("({:.0}ms)", step.duration.as_millis()),
                  Style::default().fg(Color::DarkGray),
                ),
              ]));
              if let Some(ref err) = step.error {
                for line in err.lines() {
                  scrollback_lines.push(Line::from(vec![
                    Span::raw("        "),
                    Span::styled(line.to_string(), Style::default().fg(Color::Red)),
                  ]));
                }
              }
            }
          }
        } else if let Some(ref err) = outcome.error {
          for line in err.message.lines() {
            scrollback_lines.push(Line::from(vec![
              Span::raw("    "),
              Span::styled(line.to_string(), Style::default().fg(Color::Red)),
            ]));
          }
        }

        self.send(TuiMessage::Scrollback(scrollback_lines));
        self.send(TuiMessage::ClearLive);
        self.send(TuiMessage::Status(WatchStatus::Running {
          completed: self.completed,
          total: self.total,
          start: self.start,
        }));
      }

      ReporterEvent::RunFinished { total, passed, failed, skipped, flaky, duration } => {
        // Summary line to scrollback.
        let mut spans: Vec<Span<'static>> = vec![
          Span::raw("  "),
          Span::styled(format!("{total} test(s): "), Style::default().add_modifier(Modifier::BOLD)),
        ];
        if *passed > 0 {
          spans.push(Span::styled(format!("{passed} passed"), Style::default().fg(Color::Green)));
          spans.push(Span::raw(", "));
        }
        if *failed > 0 {
          spans.push(Span::styled(format!("{failed} failed"), Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)));
          spans.push(Span::raw(", "));
        }
        if *flaky > 0 {
          spans.push(Span::styled(format!("{flaky} flaky"), Style::default().fg(Color::Yellow)));
          spans.push(Span::raw(", "));
        }
        if *skipped > 0 {
          spans.push(Span::styled(format!("{skipped} skipped"), Style::default().fg(Color::DarkGray)));
          spans.push(Span::raw(", "));
        }
        if let Some(last) = spans.last() {
          if last.content == ", " { spans.pop(); }
        }
        spans.push(Span::styled(
          format!(" ({:.1}s)", duration.as_secs_f64()),
          Style::default().fg(Color::DarkGray),
        ));

        self.send(TuiMessage::Scrollback(vec![Line::raw(""), Line::from(spans), Line::raw("")]));
        self.send(TuiMessage::ClearLive);
        self.send(TuiMessage::Status(WatchStatus::Done {
          passed: *passed,
          failed: *failed,
          skipped: *skipped,
          flaky: *flaky,
          duration: *duration,
        }));
      }

      _ => {}
    }
  }

  async fn finalize(&mut self) -> Result<(), String> {
    Ok(())
  }
}

fn status_style(status: &TestStatus) -> (&'static str, Color) {
  match status {
    TestStatus::Passed => ("\u{2713}", Color::Green),
    TestStatus::Failed => ("\u{2717}", Color::Red),
    TestStatus::TimedOut => ("\u{23f1}", Color::Red),
    TestStatus::Skipped => ("\u{2212}", Color::DarkGray),
    TestStatus::Flaky => ("\u{26a0}", Color::Yellow),
    TestStatus::Interrupted => ("!", Color::Red),
  }
}

fn step_style(status: &StepStatus) -> (&'static str, Color) {
  match status {
    StepStatus::Passed => ("v", Color::Green),
    StepStatus::Failed => ("\u{2717}", Color::Red),
    StepStatus::Skipped => ("\u{2212}", Color::DarkGray),
    StepStatus::Pending => ("?", Color::DarkGray),
  }
}

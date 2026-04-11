//! Rich terminal reporter: Playwright/Vitest-style output with colors and icons.

use std::time::Duration;

use console::Style;

use crate::config::ReportSlowTestsConfig;
use crate::model::{StepStatus, TestStatus, TestStep};
use crate::reporter::{Reporter, ReporterEvent};

pub struct TerminalReporter {
  completed: usize,
  total: usize,
  /// Config for slow test reporting. None = disabled.
  slow_tests_config: Option<ReportSlowTestsConfig>,
  /// Collected (test_name, file, duration) for slow test reporting.
  test_durations: Vec<(String, String, Duration)>,
}

impl TerminalReporter {
  pub fn new() -> Self {
    Self {
      completed: 0,
      total: 0,
      slow_tests_config: Some(ReportSlowTestsConfig::default()),
      test_durations: Vec::new(),
    }
  }

  pub fn with_slow_tests_config(mut self, config: Option<ReportSlowTestsConfig>) -> Self {
    self.slow_tests_config = config;
    self
  }
}

impl Default for TerminalReporter {
  fn default() -> Self {
    Self::new()
  }
}

// ── Styles ──

fn s_pass() -> Style {
  Style::new().green()
}
fn s_fail() -> Style {
  Style::new().red().bold()
}
fn s_skip() -> Style {
  Style::new().dim()
}
fn s_flaky() -> Style {
  Style::new().yellow().bold()
}
fn s_warn() -> Style {
  Style::new().yellow()
}
fn s_dim() -> Style {
  Style::new().dim()
}
fn s_bold() -> Style {
  Style::new().bold()
}
fn s_cyan() -> Style {
  Style::new().cyan().bold()
}

fn status_icon(status: &TestStatus) -> (&'static str, Style) {
  match status {
    TestStatus::Passed => ("\u{2713}", s_pass()),   // checkmark
    TestStatus::Failed => ("\u{2717}", s_fail()),   // cross
    TestStatus::TimedOut => ("\u{2717}", s_fail()), // cross (same as failed)
    TestStatus::Skipped => ("\u{2212}", s_skip()),  // minus
    TestStatus::Flaky => ("\u{25ce}", s_flaky()),   // bullseye
    TestStatus::Interrupted => ("!", s_fail()),
  }
}

fn step_icon(status: StepStatus) -> (&'static str, Style) {
  match status {
    StepStatus::Passed => ("\u{2713}", s_pass()),  // checkmark
    StepStatus::Failed => ("\u{2717}", s_fail()),  // cross
    StepStatus::Skipped => ("\u{2212}", s_skip()), // minus
    StepStatus::Pending => ("\u{25cb}", s_skip()), // empty circle
  }
}

fn format_duration(d: Duration) -> String {
  let ms = d.as_millis();
  if ms < 1000 {
    format!("{ms}ms")
  } else {
    format!("{:.1}s", d.as_secs_f64())
  }
}

fn print_steps(steps: &[&TestStep], indent: usize) {
  let pad = " ".repeat(indent);
  for step in steps {
    let (icon, icon_style) = step_icon(step.status);
    let dur = format_duration(step.duration);
    match step.status {
      StepStatus::Passed => {
        println!(
          "{pad}{} {} {}",
          icon_style.apply_to(icon),
          step.title,
          s_dim().apply_to(format!("({dur})"))
        );
      },
      StepStatus::Failed => {
        println!(
          "{pad}{} {} {}",
          icon_style.apply_to(icon),
          s_fail().apply_to(&step.title),
          s_dim().apply_to(format!("({dur})"))
        );
      },
      StepStatus::Skipped | StepStatus::Pending => {
        println!("{pad}{} {}", icon_style.apply_to(icon), s_skip().apply_to(&step.title));
      },
    }

    if let Some(ref err) = step.error {
      for line in err.lines() {
        println!("{pad}  {}", s_fail().apply_to(line));
      }
    }

    let nested: Vec<&TestStep> = step.steps.iter().filter(|s| s.category.is_visible()).collect();
    if !nested.is_empty() {
      print_steps(&nested, indent + 2);
    }
  }
}

#[async_trait::async_trait]
impl Reporter for TerminalReporter {
  async fn on_event(&mut self, event: &ReporterEvent) {
    match event {
      ReporterEvent::RunStarted {
        total_tests,
        num_workers,
        ..
      } => {
        self.total = *total_tests;
        println!();
        println!(
          "  {} Running {} test(s) with {} worker(s)",
          s_cyan().apply_to("\u{25b6}"), // play icon
          s_bold().apply_to(total_tests),
          num_workers,
        );
        println!();
      },

      ReporterEvent::TestFinished { test_id, outcome } => {
        self.completed += 1;
        // Collect durations for slow test reporting.
        self.test_durations.push((
          test_id.full_name(),
          test_id.file.clone(),
          outcome.duration,
        ));
        let (icon, icon_style) = status_icon(&outcome.status);
        let duration = format_duration(outcome.duration);

        match outcome.status {
          TestStatus::Passed => {
            println!(
              "  {} {} {}",
              icon_style.apply_to(icon),
              test_id.full_name(),
              s_dim().apply_to(format!("({duration})")),
            );
          },
          TestStatus::Failed | TestStatus::TimedOut => {
            println!(
              "  {} {} {}",
              icon_style.apply_to(icon),
              s_fail().apply_to(test_id.full_name()),
              s_dim().apply_to(format!("({duration})")),
            );
          },
          TestStatus::Skipped => {
            println!(
              "  {} {}",
              icon_style.apply_to(icon),
              s_skip().apply_to(test_id.full_name()),
            );
          },
          TestStatus::Flaky => {
            println!(
              "  {} {} {}",
              icon_style.apply_to(icon),
              s_flaky().apply_to(test_id.full_name()),
              s_dim().apply_to(format!("({duration}) [flaky]")),
            );
          },
          TestStatus::Interrupted => {
            println!("  {} {}", icon_style.apply_to(icon), test_id.full_name());
          },
        }

        // Steps.
        let user_steps: Vec<&TestStep> = outcome.steps.iter().filter(|s| s.category.is_visible()).collect();
        if !user_steps.is_empty() {
          print_steps(&user_steps, 4);
        }

        // Error.
        if let Some(error) = &outcome.error {
          println!();
          for line in error.message.lines() {
            println!("    {}", s_fail().apply_to(line));
          }
          if let Some(diff) = &error.diff {
            for line in diff.lines() {
              println!("    {line}");
            }
          }
          println!();
        }
      },

      ReporterEvent::RunFinished {
        total,
        passed,
        failed,
        skipped,
        flaky,
        duration,
      } => {
        // ── Slow test report ──
        if let Some(ref config) = self.slow_tests_config {
          let threshold = Duration::from_millis(config.threshold);
          let mut slow: Vec<_> = self
            .test_durations
            .iter()
            .filter(|(_, _, d)| *d >= threshold)
            .collect();
          slow.sort_by(|a, b| b.2.cmp(&a.2)); // Slowest first.
          let show = if config.max > 0 { config.max.min(slow.len()) } else { slow.len() };
          if show > 0 {
            println!();
            println!("  {} Slow test{} —", s_warn().apply_to("⚠"), if show == 1 { "" } else { "s" });
            for (name, file, dur) in &slow[..show] {
              println!(
                "    {} {} ({})",
                s_warn().apply_to(format_duration(*dur)),
                name,
                s_dim().apply_to(file),
              );
            }
            let remaining = slow.len() - show;
            if remaining > 0 {
              println!("    {} {remaining} more slow test(s)", s_dim().apply_to("…"));
            }
          }
        }

        let dur = format_duration(*duration);
        println!();

        // Summary with colored counts.
        let mut parts = Vec::new();
        if *passed > 0 {
          parts.push(format!("{}", s_pass().apply_to(format!("{passed} passed"))));
        }
        if *failed > 0 {
          parts.push(format!("{}", s_fail().apply_to(format!("{failed} failed"))));
        }
        if *flaky > 0 {
          parts.push(format!("{}", s_flaky().apply_to(format!("{flaky} flaky"))));
        }
        if *skipped > 0 {
          parts.push(format!("{}", s_skip().apply_to(format!("{skipped} skipped"))));
        }

        println!(
          "  {} {}: {} {}",
          s_bold().apply_to("Tests"),
          s_dim().apply_to(format!("{total} total")),
          parts.join(&format!("{}", s_dim().apply_to(" | "))),
          s_dim().apply_to(format!("({dur})")),
        );
        println!();
      },

      _ => {},
    }
  }
}

//! Rich terminal reporter: unified output for E2E and BDD tests.
//!
//! Automatically detects BDD tests by checking step metadata for `bdd_keyword`.
//! E2E tests show as flat results. BDD tests show Feature > Scenario > Step hierarchy
//! with keyword coloring.

use std::time::Duration;

use console::Style;

use crate::config::ReportSlowTestsConfig;
use crate::model::{StepCategory, StepStatus, TestStatus, TestStep};
use crate::reporter::{Reporter, ReporterEvent};

pub struct TerminalReporter {
  completed: usize,
  total: usize,
  slow_tests_config: Option<ReportSlowTestsConfig>,
  test_durations: Vec<(String, String, Duration)>,
  /// Current BDD feature/suite — used to print Feature headers when suite changes.
  current_suite: Option<String>,
}

impl TerminalReporter {
  pub fn new() -> Self {
    Self {
      completed: 0,
      total: 0,
      slow_tests_config: Some(ReportSlowTestsConfig::default()),
      test_durations: Vec::new(),
      current_suite: None,
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
fn s_feature() -> Style {
  Style::new().magenta().bold()
}

fn status_icon(status: &TestStatus) -> (&'static str, Style) {
  match status {
    TestStatus::Passed => ("\u{2713}", s_pass()),
    TestStatus::Failed => ("\u{2717}", s_fail()),
    TestStatus::TimedOut => ("\u{2717}", s_fail()),
    TestStatus::Skipped => ("\u{2212}", s_skip()),
    TestStatus::Flaky => ("\u{25ce}", s_flaky()),
    TestStatus::Interrupted => ("!", s_fail()),
  }
}

fn step_icon(status: StepStatus) -> (&'static str, Style) {
  match status {
    StepStatus::Passed => ("\u{2713}", s_pass()),
    StepStatus::Failed => ("\u{2717}", s_fail()),
    StepStatus::Skipped => ("\u{2212}", s_skip()),
    StepStatus::Pending => ("\u{25cb}", s_skip()),
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

/// Check if a test outcome has BDD steps (any step with bdd_keyword metadata).
fn is_bdd_test(steps: &[TestStep]) -> bool {
  steps.iter().any(|s| {
    s.metadata
      .as_ref()
      .is_some_and(|m| m.get("bdd_keyword").is_some())
      || is_bdd_test(&s.steps)
  })
}

fn print_steps(steps: &[&TestStep], indent: usize) {
  let pad = " ".repeat(indent);
  for step in steps {
    if step.category == StepCategory::Hook {
      // Hook steps get a distinct dimmed style.
      let icon = if step.error.is_some() { "\u{2717}" } else { "\u{2713}" };
      let style = if step.error.is_some() { s_fail() } else { s_dim() };
      let dur = format_duration(step.duration);
      println!(
        "{pad}{} {} {}",
        style.apply_to(icon),
        s_dim().apply_to(format!("[{}]", step.title)),
        s_dim().apply_to(format!("({dur})")),
      );
      if let Some(ref err) = step.error {
        for line in err.lines() {
          println!("{pad}  {}", s_fail().apply_to(line));
        }
      }
      continue;
    }

    let (icon, icon_style) = step_icon(step.status);
    let dur = format_duration(step.duration);

    // BDD steps: color the keyword part in cyan.
    let keyword = step
      .metadata
      .as_ref()
      .and_then(|m| m.get("bdd_keyword"))
      .and_then(|v| v.as_str())
      .map(|k| k.trim().to_string());

    match step.status {
      StepStatus::Passed => {
        if let Some(ref kw) = keyword {
          let rest = step.title.strip_prefix(kw.as_str()).unwrap_or(&step.title);
          println!(
            "{pad}{} {}{} {}",
            icon_style.apply_to(icon),
            s_cyan().apply_to(kw),
            rest,
            s_dim().apply_to(format!("({dur})")),
          );
        } else {
          println!(
            "{pad}{} {} {}",
            icon_style.apply_to(icon),
            step.title,
            s_dim().apply_to(format!("({dur})")),
          );
        }
      },
      StepStatus::Failed => {
        println!(
          "{pad}{} {} {}",
          icon_style.apply_to(icon),
          s_fail().apply_to(&step.title),
          s_dim().apply_to(format!("({dur})")),
        );
        if let Some(ref err) = step.error {
          for line in err.lines() {
            println!("{pad}  {}", s_fail().apply_to(line));
          }
        }
      },
      StepStatus::Skipped | StepStatus::Pending => {
        println!("{pad}{} {}", icon_style.apply_to(icon), s_skip().apply_to(&step.title));
      },
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
          s_cyan().apply_to("\u{25b6}"),
          s_bold().apply_to(total_tests),
          num_workers,
        );
        println!();
      },

      ReporterEvent::TestFinished { test_id, outcome } => {
        self.completed += 1;
        self.test_durations.push((
          test_id.full_name(),
          test_id.file.clone(),
          outcome.duration,
        ));

        let bdd = is_bdd_test(&outcome.steps);

        // BDD: print Feature header when suite changes.
        if bdd {
          if self.current_suite.as_ref() != test_id.suite.as_ref() {
            if self.current_suite.is_some() {
              println!();
            }
            if let Some(suite) = &test_id.suite {
              println!("  {} {}", s_feature().apply_to("Feature:"), s_bold().apply_to(suite));
            }
            self.current_suite = test_id.suite.clone();
          }
        }

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
        // Slow test report.
        if let Some(ref config) = self.slow_tests_config {
          let threshold = Duration::from_millis(config.threshold);
          let mut slow: Vec<_> = self
            .test_durations
            .iter()
            .filter(|(_, _, d)| *d >= threshold)
            .collect();
          slow.sort_by(|a, b| b.2.cmp(&a.2));
          let show = if config.max > 0 { config.max.min(slow.len()) } else { slow.len() };
          if show > 0 {
            println!();
            println!("  {} Slow test{} —", s_warn().apply_to("\u{26a0}"), if show == 1 { "" } else { "s" });
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
              println!("    {} {remaining} more slow test(s)", s_dim().apply_to("\u{2026}"));
            }
          }
        }

        let dur = format_duration(*duration);
        println!();

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

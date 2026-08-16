//! Terminal-facing output for `ferridriver run`.
//!
//! Streaming mode (the default) writes every `console.*` call the moment it
//! happens, on the stream Node would use: `log`/`info`/`debug` to stdout,
//! `warn`/`error`/`trace` to stderr. The script's return value follows on
//! stdout, so — exactly as under Node — stdout in this mode is human output,
//! not a parseable document.
//!
//! `--json` is the machine contract: no sink is installed, console output
//! stays buffered inside one result document, and stdout carries nothing else.
//!
//! `--trace` layers the action stream on top: each `page.*` / `locator.*` /
//! `expect.*` call is announced as it starts and closed out with its duration,
//! so a script parked on a 30s wait shows what it is waiting for.

use std::fmt::Write as _;
use std::io::Write as _;

use console::Style;
use ferridriver::trace::{ActionInfo, ActionObserver};
use ferridriver_script::{ConsoleEntry, ConsoleLevel, ConsoleSink, Outcome, ScriptResult};

/// Writes console entries to stderr as they are produced.
#[derive(Debug)]
pub struct StreamingConsole;

impl ConsoleSink for StreamingConsole {
  fn emit(&self, entry: &ConsoleEntry) {
    print_entry(entry);
  }

  fn styled_for(&self, level: ConsoleLevel) -> bool {
    if goes_to_stderr(level) {
      console::colors_enabled_stderr()
    } else {
      console::colors_enabled()
    }
  }

  fn clear(&self) {
    if console::user_attended() {
      console::Term::stdout().clear_screen().ok();
    }
  }
}

/// Write one console entry on the stream Node would use.
///
/// Shared with `run --session`, where the entries arrive as wire events from
/// the host rather than from a sink on a local VM — the rendering must not
/// depend on which side produced them.
pub fn print_entry(entry: &ConsoleEntry) {
  // Both streams are unbuffered, so the line lands before the next await
  // point; write failures (closed pipe) are not the script's problem.
  let _ = if goes_to_stderr(entry.level) {
    writeln!(std::io::stderr(), "{}", entry.message)
  } else {
    writeln!(std::io::stdout(), "{}", entry.message)
  };
}

/// Node's split, which is also what carries the severity: `log`, `info`,
/// `debug` (and everything built on them — `dir`, `table`, `group`, `count`,
/// `time*`) write to stdout, while `warn`, `error`, `trace` and a failed
/// `assert` write to stderr. No per-level prefix or colour, because the stream
/// is the signal.
fn goes_to_stderr(level: ConsoleLevel) -> bool {
  match level {
    ConsoleLevel::Warn | ConsoleLevel::Error | ConsoleLevel::Trace | ConsoleLevel::System => true,
    ConsoleLevel::Log | ConsoleLevel::Info | ConsoleLevel::Debug => false,
  }
}

/// Longest parameter summary kept on an action line before it is elided.
const MAX_PARAM_WIDTH: usize = 100;

/// The action observer a local `ferridriver run` installs.
///
/// One observer, because a process has one: `--trace` prints the live action
/// lines and `--code` renders each action as source. The session host runs the
/// same policy over its wire — see `ferridriver-script`'s forwarder — so a
/// script echoes the same code whichever side ran it.
#[derive(Debug)]
pub struct RunObserver {
  pub trace: bool,
  pub code: Option<ferridriver::codegen::OutputLanguage>,
  /// Print each generated line as it happens. Off when the destination is a
  /// file, so the lines are not written twice in two shapes.
  pub echo_code: bool,
  pub collected: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
  /// Applied to the echoed source, so a generated file carries an
  /// environment read where the run used a declared credential.
  pub secrets: ferridriver::response::Secrets,
}

impl ActionObserver for RunObserver {
  fn action_begin(&self, action: &ActionInfo) {
    if self.trace {
      print_action_begin(
        &action.title,
        &action.params,
        action.location.as_ref().map(ToString::to_string).as_deref(),
      );
    }
  }

  fn action_end(&self, action: &ActionInfo, elapsed: std::time::Duration, error: Option<&str>) {
    if let Some(language) = self.code
      && let Some(line) = ferridriver::codegen::echo::line_for_with_secrets(action, language, &self.secrets)
    {
      if self.echo_code {
        let _ = writeln!(std::io::stderr(), "{line}");
      }
      if let Ok(mut collected) = self.collected.lock() {
        collected.push(line);
      }
    }
    if self.trace {
      print_action_end(&action.title, elapsed.as_secs_f64() * 1000.0, error);
    }
  }

  fn action_log(&self, _action: &ActionInfo, message: &str) {
    if self.trace {
      print_action_log(message);
    }
  }
}

/// The three action lines, as free functions.
///
/// Shared with `run --session`, where the actions arrive as wire events from
/// the host rather than from an observer on a local browser — a traced run
/// must look the same whichever side ran it.
pub fn print_action_begin(title: &str, params: &serde_json::Value, location: Option<&str>) {
  let dim = Style::new().for_stderr().dim();
  let params = summarize_params(params);
  let mut line = format!("› {title} {params}").trim_end().to_string();
  if let Some(location) = location {
    let _ = write!(line, "  at {location}");
  }
  let _ = writeln!(std::io::stderr(), "{}", dim.apply_to(line));
}

pub fn print_action_end(title: &str, ms: f64, error: Option<&str>) {
  let style = Style::new().for_stderr();
  let line = match error {
    // The message is often multi-line (Playwright-shaped call logs); the
    // first line is the failure itself.
    Some(err) => style
      .red()
      .apply_to(format!("✗ {title} {ms:.0}ms — {}", err.lines().next().unwrap_or(err))),
    None => style.dim().apply_to(format!("✓ {title} {ms:.0}ms")),
  };
  let _ = writeln!(std::io::stderr(), "{line}");
}

pub fn print_action_log(message: &str) {
  let dim = Style::new().for_stderr().dim();
  let _ = writeln!(std::io::stderr(), "{}", dim.apply_to(format!("  · {message}")));
}

/// Flatten an action's params into one line: object values joined by spaces,
/// anything else compact-JSON, elided past [`MAX_PARAM_WIDTH`].
fn summarize_params(params: &serde_json::Value) -> String {
  let rendered = match params {
    serde_json::Value::Object(map) => map
      .values()
      .map(|v| match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
      })
      .collect::<Vec<_>>()
      .join(" "),
    serde_json::Value::Null => String::new(),
    serde_json::Value::String(s) => s.clone(),
    other => other.to_string(),
  };
  let rendered = rendered.replace('\n', " ");
  if rendered.chars().count() > MAX_PARAM_WIDTH {
    let head: String = rendered.chars().take(MAX_PARAM_WIDTH).collect();
    format!("{head}…")
  } else {
    rendered
  }
}

/// Render a finished run for a human: the returned value on stdout (strings
/// raw, everything else pretty JSON, `null` printed as nothing at all), the
/// failure on stderr.
pub fn print_result(result: &ScriptResult) {
  match &result.outcome {
    Outcome::Ok { success } => match &success.value {
      serde_json::Value::Null => {},
      serde_json::Value::String(s) => println!("{s}"),
      value => println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
      ),
    },
    Outcome::Error { error } => {
      let red = Style::new().for_stderr().red();
      let dim = Style::new().for_stderr().dim();
      let name = error.name.as_deref().unwrap_or_else(|| error_name(error.kind));
      eprintln!("{}", red.apply_to(format!("{name}: {}", error.message)));
      // The code frame the engine already builds around the throwing line —
      // Node prints the offending source above the stack, so do the same.
      if let Some(snippet) = &error.source_snippet {
        eprintln!();
        eprint!("{}", dim.apply_to(snippet));
      }
      if let Some(stack) = error.stack.as_deref().map(str::trim_end).filter(|s| !s.is_empty()) {
        eprintln!("{}", dim.apply_to(stack));
      }
      eprintln!("{}", dim.apply_to(format!("({}ms)", result.duration_ms)));
    },
  }
}

/// JS-shaped constructor name for a failure kind, so a terminal failure reads
/// like the `Error: message` header Node and Playwright print.
fn error_name(kind: ferridriver_script::ScriptErrorKind) -> &'static str {
  use ferridriver_script::ScriptErrorKind as K;
  match kind {
    K::Syntax => "SyntaxError",
    K::Runtime => "Error",
    K::Timeout => "TimeoutError",
    K::MemoryLimit => "MemoryLimitError",
    K::SandboxViolation => "SandboxViolationError",
    K::Internal => "InternalError",
  }
}

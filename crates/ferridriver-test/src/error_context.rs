//! Playwright's `errorContext.ts`: the markdown a failed test leaves
//! beside its other artifacts, written so it can be handed to a model as
//! it stands. It names the test, carries every error with the
//! accessibility tree the failing assertion saw (or, failing that, the
//! page's), and quotes the source around the line that failed.

use std::path::Path;

use crate::model::TestFailure;
use crate::reporter::base::strip_ansi;

const FIX_TEST_INSTRUCTIONS: &str = "# Instructions

- Following Playwright test failed.
- Explain why, be concise, respect Playwright best practices.
- Provide a snippet of code with the fix, if possible.
";

/// Lines of source quoted on either side of the failing line.
const CODE_FRAME_RADIUS: usize = 100;

pub struct ErrorContextInput<'a> {
  pub title_path: &'a [String],
  pub file: &'a str,
  pub line: u32,
  pub column: u32,
  pub errors: &'a [&'a TestFailure],
  /// The page's own snapshot, used only when no error brought one.
  pub page_snapshot: Option<&'a str>,
  /// Where relative paths, in the location and in stack frames, start.
  pub cwd: &'a Path,
}

/// `buildErrorContext`: `None` when there is nothing to say.
#[must_use]
pub fn build(input: &ErrorContextInput<'_>) -> Option<String> {
  let errors: Vec<&TestFailure> = input.errors.iter().copied().filter(|e| !e.message.is_empty()).collect();
  if errors.is_empty() && input.page_snapshot.is_none() {
    return None;
  }

  let mut lines = vec![
    FIX_TEST_INSTRUCTIONS.to_string(),
    "# Test info".into(),
    String::new(),
    format!("- Name: {}", input.title_path.join(" >> ")),
    format!(
      "- Location: {}:{}:{}",
      relative_file_path(input.file, input.cwd),
      input.line,
      input.column
    ),
  ];

  if !errors.is_empty() {
    lines.extend([String::new(), "# Error details".into()]);
    for error in &errors {
      lines.extend([String::new(), "```".into(), message_of(error), "```".into()]);
      if let Some(context) = &error.error_context {
        lines.extend([String::new(), "```yaml".into(), context.clone(), "```".into()]);
      }
    }
  }

  if let Some(snapshot) = input.page_snapshot {
    lines.extend([
      String::new(),
      "# Page snapshot".into(),
      String::new(),
      "```yaml".into(),
      snapshot.to_string(),
      "```".into(),
    ]);
  }

  if let Some(frame) = errors.last().and_then(|e| code_frame(e, input.cwd)) {
    lines.extend([
      String::new(),
      "# Test source".into(),
      String::new(),
      "```ts".into(),
      frame,
      "```".into(),
    ]);
  }

  Some(lines.join("\n"))
}

/// The error as a reporter prints it, without the terminal escapes.
fn message_of(error: &TestFailure) -> String {
  strip_ansi(&error.to_string()).into_owned()
}

fn relative_file_path(file: &str, cwd: &Path) -> String {
  let path = Path::new(file);
  if !path.is_absolute() {
    return file.to_string();
  }
  path
    .strip_prefix(cwd)
    .map_or_else(|_| file.to_string(), |p| p.display().to_string())
}

/// `buildCodeFrame`: the source around the first frame of the error's
/// stack that is not a dependency, with a caret under the failing column.
fn code_frame(error: &TestFailure, cwd: &Path) -> Option<String> {
  let (file, line, column) = error.stack.as_deref().and_then(first_user_frame)?;
  let path = Path::new(&file);
  let path = if path.is_absolute() {
    path.to_path_buf()
  } else {
    cwd.join(path)
  };
  let source = std::fs::read_to_string(path).ok()?;
  let source_lines: Vec<&str> = source.split('\n').collect();
  let line = usize::try_from(line).ok()?;
  let start = line.saturating_sub(CODE_FRAME_RADIUS + 1);
  let end = source_lines.len().min(line + CODE_FRAME_RADIUS);
  if start >= end {
    return None;
  }
  let width = end.to_string().len();
  let mut frame: Vec<String> = source_lines[start..end]
    .iter()
    .enumerate()
    .map(|(index, text)| {
      let number = start + index + 1;
      let marker = if number == line { "> " } else { "  " };
      format!("{marker}{number:<width$} | {text}")
    })
    .collect();
  let message = strip_ansi(&error.message)
    .lines()
    .next()
    .unwrap_or_default()
    .to_string();
  if !message.is_empty() {
    let caret_pad = usize::try_from(column).unwrap_or_default().saturating_sub(2);
    frame.insert(
      (line - start).min(frame.len()),
      format!("{} | {} ^ {message}", " ".repeat(width + 2), " ".repeat(caret_pad)),
    );
  }
  Some(frame.join("\n"))
}

/// `parseErrorStack(...).location`: the first frame naming a file that is
/// not under `node_modules`. Frames read `at fn (file:line:col)` or
/// `at file:line:col`; a Rust assertion's stack is the bare
/// `file:line:col` of its caller.
fn first_user_frame(stack: &str) -> Option<(String, u32, u32)> {
  stack.lines().find_map(|raw| {
    let line = raw.trim();
    let line = line.strip_prefix("at ").unwrap_or(line);
    let location = match (line.rfind('('), line.ends_with(')')) {
      (Some(open), true) => &line[open + 1..line.len() - 1],
      _ => line,
    };
    let mut parts = location.rsplitn(3, ':');
    let column = parts.next()?.parse::<u32>().ok()?;
    let line_number = parts.next()?.parse::<u32>().ok()?;
    let file = parts.next()?;
    let in_dependency = file.contains("/node_modules/") || file.contains("\\node_modules\\");
    (!file.is_empty() && !in_dependency).then(|| (file.to_string(), line_number, column))
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn failure(message: &str, stack: Option<&str>, context: Option<&str>) -> TestFailure {
    TestFailure {
      message: message.into(),
      stack: stack.map(Into::into),
      diff: None,
      screenshot: None,
      error_context: context.map(Into::into),
    }
  }

  #[test]
  fn nothing_to_say_without_errors_or_snapshot() {
    let input = ErrorContextInput {
      title_path: &[],
      file: "a.spec.ts",
      line: 1,
      column: 1,
      errors: &[],
      page_snapshot: None,
      cwd: Path::new("/work"),
    };
    assert_eq!(build(&input), None);
  }

  #[test]
  fn carries_each_error_with_its_aria_snapshot() {
    let error = failure(
      "expect(locator).toHaveText(expected) failed",
      None,
      Some("- button \"Hi\""),
    );
    let titles = vec!["a.spec.ts".to_string(), "fail".to_string()];
    let errors = [&error];
    let text = build(&ErrorContextInput {
      title_path: &titles,
      file: "/work/a.spec.ts",
      line: 3,
      column: 5,
      errors: &errors,
      page_snapshot: None,
      cwd: Path::new("/work"),
    })
    .unwrap();
    assert_eq!(
      text,
      [
        FIX_TEST_INSTRUCTIONS,
        "# Test info",
        "",
        "- Name: a.spec.ts >> fail",
        "- Location: a.spec.ts:3:5",
        "",
        "# Error details",
        "",
        "```",
        "expect(locator).toHaveText(expected) failed",
        "```",
        "",
        "```yaml",
        "- button \"Hi\"",
        "```",
      ]
      .join("\n")
    );
  }

  #[test]
  fn code_frame_points_at_the_first_user_frame() {
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join("a.spec.ts");
    std::fs::write(&spec, "line1\nline2\n  expect(1).toBe(2);\nline4\n").unwrap();
    let stack = format!(
      "    at helper (/proj/node_modules/lib/index.js:1:1)\n    at <anonymous> ({}:3:13)",
      spec.display()
    );
    let error = failure(
      "\u{1b}[31mexpect(value).toBe() failed\u{1b}[39m\n\nExpected: 2",
      Some(&stack),
      None,
    );
    let frame = code_frame(&error, dir.path()).unwrap();
    assert_eq!(
      frame,
      [
        "  1 | line1",
        "  2 | line2",
        "> 3 |   expect(1).toBe(2);",
        "    |             ^ expect(value).toBe() failed",
        "  4 | line4",
        "  5 | ",
      ]
      .join("\n")
    );
  }

  #[test]
  fn a_rust_assertion_failure_keeps_its_snapshot() {
    let mut failure = ferridriver_expect::AssertionFailure::new("failed", None);
    failure.error_context = Some("- button".into());
    assert_eq!(TestFailure::from(failure).error_context.as_deref(), Some("- button"));
  }

  #[test]
  fn reads_rust_caller_locations() {
    assert_eq!(
      first_user_frame("at tests/a.rs:10:7"),
      Some(("tests/a.rs".to_string(), 10, 7))
    );
    assert_eq!(first_user_frame("    at <anonymous> (native)"), None);
  }
}

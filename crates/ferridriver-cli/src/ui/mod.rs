//! Terminal presentation for the `ferridriver` binary.
//!
//! Every command writes through here rather than through `println!`, so the
//! three decisions that make output readable are made once: whether colour is
//! on, how wide the terminal is, and whether the caller wanted a document
//! instead of a report.
//!
//! Colour is resolved in [`init`] and pushed into `console`'s global switch,
//! which every [`console::Style`] consults at format time — including the ones
//! inside the test reporters. That is the whole propagation mechanism; nothing
//! threads a flag.

pub mod progress;
pub mod prompt;
pub mod table;

use std::io::IsTerminal as _;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use console::Style;

pub use progress::Progress;
pub use table::Table;

// ── global state ────────────────────────────────────────────────────────

/// What `--format` selected. Human output is a report for a person; JSON is a
/// document for a program, and the two never appear on the same stream.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Format {
  #[default]
  Human,
  Json,
}

/// Colour policy from the command line, before the terminal has a say.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ColorChoice {
  #[default]
  Auto,
  Always,
  Never,
}

static FORMAT: AtomicU8 = AtomicU8::new(0);
static VERBOSITY: AtomicU8 = AtomicU8::new(1);

/// Resolve the presentation policy for this process.
///
/// Called once, before any command runs. Colour is decided here and installed
/// into `console`'s global switch; the format and the quiet flag are read
/// back through [`format`] and [`quiet`].
pub fn init(color: ColorChoice, format: Format, quiet: bool) {
  FORMAT.store(u8::from(format == Format::Json), Ordering::Relaxed);
  VERBOSITY.store(u8::from(!quiet), Ordering::Relaxed);

  let (out, err) = match color {
    // `always` means always: it is how someone gets colour through a pipe
    // into `less -R` or a CI log viewer, so it must not consult the terminal.
    ColorChoice::Always => (true, true),
    ColorChoice::Never => (false, false),
    ColorChoice::Auto => {
      // An enclosing agent reads this output as text: the escape sequences
      // are noise it pays for by the token, even where it allocated a pty.
      // JSON is a document, and a document is never styled.
      let allowed = format == Format::Human && !in_agent_session() && !no_color_requested();
      (
        allowed && std::io::stdout().is_terminal(),
        allowed && std::io::stderr().is_terminal(),
      )
    },
  };
  console::set_colors_enabled(out);
  console::set_colors_enabled_stderr(err);
}

/// The output format this run selected.
pub fn format() -> Format {
  if FORMAT.load(Ordering::Relaxed) == 1 {
    Format::Json
  } else {
    Format::Human
  }
}

/// Whether the run asked for JSON. Sugar for the overwhelmingly common check.
pub fn json() -> bool {
  format() == Format::Json
}

/// Whether `--quiet` suppressed incidental output.
pub fn quiet() -> bool {
  VERBOSITY.load(Ordering::Relaxed) == 0
}

/// Whether both ends of the terminal are attached, so a prompt can be answered.
pub fn interactive() -> bool {
  std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// `NO_COLOR` (any value) per <https://no-color.org>.
fn no_color_requested() -> bool {
  std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
}

/// Whether a coding agent, rather than a person, is reading this output.
fn in_agent_session() -> bool {
  agent_session_from_env(
    std::env::var("CLAUDECODE").ok().as_deref(),
    std::env::var("CURSOR_AGENT").ok().as_deref(),
    std::env::var("FERRIDRIVER_IN_AGENT_SESSION").ok().as_deref(),
  )
}

/// The decision behind [`in_agent_session`], split out so it is testable
/// without mutating the process environment.
fn agent_session_from_env(claude: Option<&str>, cursor: Option<&str>, explicit: Option<&str>) -> bool {
  claude == Some("1") || cursor.is_some() || explicit == Some("1")
}

// ── semantic text ───────────────────────────────────────────────────────

/// Glyph marking an outcome. Kept out of the styles so a status can be shown
/// in a table cell, a check line, or a finished progress bar identically.
pub const OK: &str = "✓";
pub const FAIL: &str = "✗";
pub const WARN: &str = "!";

#[must_use]
pub fn success(msg: &str) -> String {
  format!("{} {msg}", Style::new().green().bold().apply_to(OK))
}

#[must_use]
pub fn failure(msg: &str) -> String {
  format!("{} {msg}", Style::new().red().bold().apply_to(FAIL))
}

#[must_use]
pub fn warning(msg: &str) -> String {
  format!("{} {msg}", Style::new().yellow().bold().apply_to(WARN))
}

#[must_use]
pub fn info(msg: &str) -> String {
  format!("{} {msg}", Style::new().dim().apply_to("·"))
}

/// The status glyph on its own, for a table's status column, where the space
/// after it would be padding the column already adds.
#[must_use]
pub fn glyph_ok() -> String {
  Style::new().green().bold().apply_to(OK).to_string()
}

#[must_use]
pub fn glyph_fail() -> String {
  Style::new().red().bold().apply_to(FAIL).to_string()
}

#[must_use]
pub fn glyph_warn() -> String {
  Style::new().yellow().bold().apply_to(WARN).to_string()
}

/// A section title: the line a reader scans for when skimming a report.
#[must_use]
pub fn header(msg: &str) -> String {
  format!(
    "{} {}",
    Style::new().cyan().apply_to("▸"),
    Style::new().bold().apply_to(msg)
  )
}

/// `key: value`, with the key dimmed so the values line up as the content.
#[must_use]
pub fn kv(key: &str, value: &str) -> String {
  format!("  {} {value}", Style::new().dim().apply_to(format!("{key}:")))
}

/// `key: value` with the key padded to `width`, for a run of them.
#[must_use]
pub fn kv_padded(key: &str, value: &str, width: usize) -> String {
  let key = format!("{key}:");
  let pad = console::pad_str(&key, width + 1, console::Alignment::Left, None).into_owned();
  format!("  {} {value}", Style::new().dim().apply_to(pad))
}

#[must_use]
pub fn list_item(msg: &str) -> String {
  format!("  {} {msg}", Style::new().cyan().apply_to("•"))
}

#[must_use]
pub fn sub_item(msg: &str) -> String {
  format!("    {} {msg}", Style::new().dim().apply_to("→"))
}

#[must_use]
pub fn dim(msg: &str) -> String {
  Style::new().dim().apply_to(msg).to_string()
}

#[must_use]
pub fn bold(msg: &str) -> String {
  Style::new().bold().apply_to(msg).to_string()
}

/// A command or snippet the reader is meant to run or paste.
#[must_use]
pub fn code(msg: &str) -> String {
  Style::new().green().apply_to(msg).to_string()
}

#[must_use]
pub fn path(msg: &str) -> String {
  Style::new().cyan().apply_to(msg).to_string()
}

#[must_use]
pub fn url(msg: &str) -> String {
  Style::new().blue().underlined().apply_to(msg).to_string()
}

#[must_use]
pub fn number(n: impl std::fmt::Display) -> String {
  Style::new().yellow().bold().apply_to(n).to_string()
}

/// An inverted label, for a short categorical value (a backend, a project, a
/// status word) that has to be findable in a wall of text.
#[must_use]
pub fn badge(text: &str, style: &Style) -> String {
  style.apply_to(format!(" {text} ")).to_string()
}

// ── printing ────────────────────────────────────────────────────────────

/// Incidental progress, suppressed by `--quiet` and by `--format json`.
pub fn say(line: &str) {
  if !quiet() && !json() {
    println!("{line}");
  }
}

/// Incidental progress that must not touch stdout, because stdout is either
/// the protocol channel (`mcp --transport stdio`) or the run's document.
pub fn note(line: &str) {
  if !quiet() && !json() {
    eprintln!("{} {line}", Style::new().dim().apply_to("-"));
  }
}

/// A section title followed by its body, with the blank line before it that
/// separates one section from the last.
pub fn section(title: &str) {
  if !quiet() && !json() {
    println!("\n{}", header(title));
  }
}

/// The width to lay out against: the terminal's, clamped to something a
/// paragraph is still readable at, and a fixed width when piped.
#[must_use]
pub fn width() -> usize {
  console::Term::stdout()
    .size_checked()
    .map_or(100, |(_, w)| (w as usize).clamp(40, 120))
}

/// What to do next, as runnable commands. The single most useful thing a CLI
/// prints, and the one thing this binary never printed.
pub fn next_steps(steps: &[(&str, String)]) {
  if quiet() || json() || steps.is_empty() {
    return;
  }
  println!("\n{}", header("Next"));
  for (what, cmd) in steps {
    println!("  {} {}", Style::new().dim().apply_to(format!("{what}:")), code(cmd));
  }
}

/// Break `text` into lines no wider than `width`, on word boundaries.
///
/// For output that must not lose anything — a failing check's detail, a
/// diagnostic — where truncating to fit the column would cut off the part
/// that says what to do. Words longer than `width` are left over-long rather
/// than broken, because a path or a URL is worse split than wrapped.
#[must_use]
pub fn wrap(text: &str, width: usize) -> Vec<String> {
  let width = width.max(8);
  let mut lines = Vec::new();
  let mut current = String::new();
  for word in text.split_whitespace() {
    let candidate = if current.is_empty() {
      console::measure_text_width(word)
    } else {
      console::measure_text_width(&current) + 1 + console::measure_text_width(word)
    };
    if candidate > width && !current.is_empty() {
      lines.push(std::mem::take(&mut current));
    }
    if !current.is_empty() {
      current.push(' ');
    }
    current.push_str(word);
  }
  if !current.is_empty() {
    lines.push(current);
  }
  if lines.is_empty() {
    lines.push(String::new());
  }
  lines
}

/// Emit a value as the run's JSON document.
///
/// # Errors
/// When the value cannot be serialised.
pub fn print_json<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
  use std::io::Write as _;
  let mut out = std::io::stdout().lock();
  writeln!(out, "{}", serde_json::to_string_pretty(value)?)?;
  // Commands reach `std::process::exit` on a failing run, which runs no
  // destructors; a piped stdout is block-buffered, so the document would be
  // dropped on exactly the runs someone wanted the log of.
  out.flush()?;
  Ok(())
}

// ── formatting ──────────────────────────────────────────────────────────

/// Human-readable byte count.
#[must_use]
pub fn bytes(n: u64) -> String {
  const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
  #[allow(clippy::cast_precision_loss)]
  let mut value = n as f64;
  let mut unit = 0;
  while value >= 1024.0 && unit < UNITS.len() - 1 {
    value /= 1024.0;
    unit += 1;
  }
  if unit == 0 {
    format!("{n} B")
  } else {
    format!("{value:.1} {}", UNITS[unit])
  }
}

/// Human-readable duration, at the precision the magnitude deserves.
#[must_use]
pub fn duration(d: Duration) -> String {
  let ms = d.as_millis();
  if ms < 1000 {
    return format!("{ms}ms");
  }
  let secs = d.as_secs_f64();
  if secs < 60.0 {
    return format!("{secs:.1}s");
  }
  let mins = d.as_secs() / 60;
  let rem = d.as_secs() % 60;
  if mins < 60 {
    format!("{mins}m{rem:02}s")
  } else {
    format!("{}h{:02}m", mins / 60, mins % 60)
  }
}

/// A path as short as it can be made without becoming ambiguous: relative to
/// the working directory when it is under it, `~`-relative when it is under
/// the home directory, and elided from the front when it is still too long
/// for `max`.
#[must_use]
pub fn short_path(p: &std::path::Path, max: usize) -> String {
  let text = relative_to_cwd(p)
    .or_else(|| relative_to_home(p))
    .unwrap_or_else(|| p.display().to_string());
  // A path built by joining onto `.` renders as `./x`; the prefix is noise.
  let text = text.strip_prefix("./").unwrap_or(&text).to_string();
  if console::measure_text_width(&text) <= max {
    return text;
  }
  // Keep the tail: the file name identifies the thing, the leading
  // directories only say where it lives.
  let tail = console::truncate_str(&text, max.saturating_sub(1), "").into_owned();
  let kept = text.len().saturating_sub(tail.len());
  format!("…{}", &text[kept..])
}

/// The same shortening as [`short_path`] with no length limit: relative to
/// the working directory or to `~` where it can be, and untouched otherwise.
///
/// For output that wraps rather than truncates, where cutting a path costs
/// the reader the answer and costs nothing to keep.
#[must_use]
pub fn rel_path(p: &std::path::Path) -> String {
  short_path(p, usize::MAX)
}

fn relative_to_cwd(p: &std::path::Path) -> Option<String> {
  let cwd = std::env::current_dir().ok()?;
  Some(p.strip_prefix(&cwd).ok()?.display().to_string())
}

fn relative_to_home(p: &std::path::Path) -> Option<String> {
  let home = std::env::var_os("HOME")?;
  let rest = p.strip_prefix(std::path::Path::new(&home)).ok()?;
  Some(format!("~/{}", rest.display()))
}

/// Shorten a message that is mostly a path — a provenance line, a "resolved
/// to" detail — without needing to know where the path starts.
///
/// Whole-string first, because that is the common case; otherwise the last
/// whitespace-separated token, which is where these messages put the path.
#[must_use]
pub fn short_in(text: &str, max: usize) -> String {
  let whole = short_path(std::path::Path::new(text), max);
  if whole != text {
    return whole;
  }
  match text.rsplit_once(' ') {
    Some((head, tail)) if tail.starts_with('/') => {
      format!("{head} {}", short_path(std::path::Path::new(tail), max))
    },
    _ => text.to_string(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn agent_session_detection_matches_each_signal() {
    assert!(!agent_session_from_env(None, None, None));
    assert!(agent_session_from_env(Some("1"), None, None));
    assert!(!agent_session_from_env(Some("0"), None, None));
    assert!(agent_session_from_env(None, Some("anything"), None));
    assert!(agent_session_from_env(None, None, Some("1")));
    assert!(!agent_session_from_env(Some(""), None, Some("0")));
  }

  #[test]
  fn bytes_scale_to_the_unit_that_fits() {
    assert_eq!(bytes(512), "512 B");
    assert_eq!(bytes(2048), "2.0 KB");
    assert_eq!(bytes(5 * 1024 * 1024), "5.0 MB");
  }

  #[test]
  fn durations_lose_precision_as_they_grow() {
    assert_eq!(duration(Duration::from_millis(37)), "37ms");
    assert_eq!(duration(Duration::from_secs_f64(1.5)), "1.5s");
    assert_eq!(duration(Duration::from_secs(75)), "1m15s");
    assert_eq!(duration(Duration::from_mins(62)), "1h02m");
  }

  #[test]
  fn wrapping_breaks_on_words_and_never_loses_one() {
    let text = "staging and dev both launch with profile /tmp/shared; a Chromium profile takes one";
    let lines = wrap(text, 30);
    for line in &lines {
      assert!(
        console::measure_text_width(line) <= 30 || !line.contains(' '),
        "{line:?}"
      );
    }
    assert_eq!(lines.join(" "), text);
  }

  #[test]
  fn wrapping_keeps_an_over_long_word_whole() {
    let lines = wrap("/a/very/long/path/that/cannot/be/broken", 10);
    assert_eq!(lines, vec!["/a/very/long/path/that/cannot/be/broken"]);
  }

  #[test]
  fn short_path_keeps_the_tail_when_it_has_to_cut() {
    let long = std::path::PathBuf::from("/a/very/long/prefix/that/goes/on/and/on/report.zip");
    let out = short_path(&long, 20);
    assert!(out.ends_with("report.zip"), "{out}");
    assert!(console::measure_text_width(&out) <= 20, "{out}");
  }
}

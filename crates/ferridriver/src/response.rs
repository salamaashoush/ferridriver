//! The agent-facing response contract.
//!
//! A tool call's mechanical payload (a return value, a JSON document) answers
//! "what did the call produce". An agent also needs "what state am I in now",
//! "what code reproduces this", and "what did you not show me". This module is
//! the one place that shape is defined, so the MCP server and the CLI answer
//! the same way rather than each inventing a layout.
//!
//! Three concerns live here because they are all properties of a response
//! rather than of any one caller:
//!
//! - [`Response`] — titled sections rendered as markdown or as one JSON object.
//! - [`Secrets`] — values that must never reach the caller verbatim, whether
//!   they arrive through a returned value, a console line, or echoed code.
//! - [`OutputBudget`] — a ceiling on the artifact directory, enforced by
//!   evicting least-recently-modified files that the current call did not write.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One titled part of a response.
#[derive(Debug, Clone)]
struct Section {
  title: String,
  lines: Vec<String>,
  /// Fence language for the markdown rendering, when the body is source.
  codeframe: Option<&'static str>,
}

/// The section titles, so producers and parsers agree on the spelling.
pub mod section {
  pub const ERROR: &str = "Error";
  pub const RESULT: &str = "Result";
  pub const CODE: &str = "Ran ferridriver code";
  pub const PAGE: &str = "Page";
}

/// A response under construction.
///
/// Sections render in insertion order, and every caller adds them in the same
/// order — failure first (an agent that reads nothing else must still see it),
/// then the result, then the reproduction, then the page it is now looking at.
#[derive(Debug, Default)]
pub struct Response {
  sections: Vec<Section>,
  secrets: Secrets,
}

impl Response {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Values redacted from every rendering of this response.
  #[must_use]
  pub fn with_secrets(mut self, secrets: Secrets) -> Self {
    self.secrets = secrets;
    self
  }

  /// Add a section, skipping empty ones — an empty `### Page` costs the
  /// reader attention and tells them nothing.
  fn push(&mut self, title: &'static str, lines: Vec<String>, codeframe: Option<&'static str>) {
    if lines.is_empty() {
      return;
    }
    self.sections.push(Section {
      title: title.to_string(),
      lines,
      codeframe,
    });
  }

  pub fn error(&mut self, lines: Vec<String>) {
    self.push(section::ERROR, lines, None);
  }

  pub fn result(&mut self, lines: Vec<String>) {
    self.push(section::RESULT, lines, None);
  }

  /// The source reproducing what ran, fenced in the language it is written in.
  pub fn code(&mut self, lines: Vec<String>, language: crate::codegen::OutputLanguage) {
    let fence = match language {
      crate::codegen::OutputLanguage::TypeScript => "ts",
      crate::codegen::OutputLanguage::Rust => "rust",
      crate::codegen::OutputLanguage::Gherkin => "gherkin",
    };
    self.push(section::CODE, lines, Some(fence));
  }

  pub fn page(&mut self, state: &PageState) {
    self.push(section::PAGE, state.lines(), None);
  }

  /// Markdown: `### Title` per section, source bodies fenced. Redacted.
  #[must_use]
  pub fn render(&self) -> String {
    let mut out = String::new();
    for section in &self.sections {
      if !out.is_empty() {
        out.push('\n');
      }
      out.push_str("### ");
      out.push_str(&section.title);
      out.push('\n');
      if let Some(fence) = section.codeframe {
        out.push_str("```");
        out.push_str(fence);
        out.push('\n');
      }
      for line in &section.lines {
        out.push_str(line);
        out.push('\n');
      }
      if section.codeframe.is_some() {
        out.push_str("```\n");
      }
    }
    self.secrets.redact(&out).into_owned()
  }

  /// One object keyed by lowercased section title, for a machine consumer.
  #[must_use]
  pub fn to_json(&self) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for section in &self.sections {
      let key = section.title.to_lowercase().replace(' ', "_");
      map.insert(key, serde_json::Value::String(section.lines.join("\n")));
    }
    let mut value = serde_json::Value::Object(map);
    self.secrets.redact_json(&mut value);
    value
  }
}

/// What the page looks like right now.
///
/// Everything here is read from state the page already tracks — the URL from
/// the frame cache, the counts from the retained console/error history — so
/// capturing it costs one round-trip (the title) and, importantly, opens no
/// action span: a captured page state never appears in echoed code.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageState {
  pub url: String,
  pub title: String,
  pub console_errors: usize,
  pub console_warnings: usize,
  pub page_errors: usize,
}

impl PageState {
  /// Read the current state of `page`.
  pub async fn capture(page: &crate::Page) -> Self {
    // `since-navigation` is the default Playwright reports against: counts
    // from a document the agent has already navigated away from would
    // describe a page that is no longer on screen.
    let filter = crate::observed::ObservedFilter::SinceNavigation;
    let console = page.console_messages(filter);
    let mut console_errors = 0;
    let mut console_warnings = 0;
    for message in &console {
      match message.type_str() {
        "error" => console_errors += 1,
        "warning" => console_warnings += 1,
        _ => {},
      }
    }
    Self {
      url: page.url(),
      title: page.title().await.unwrap_or_default(),
      console_errors,
      console_warnings,
      page_errors: page.page_errors(filter).len(),
    }
  }

  /// The `### Page` body. Empty when there is no page: a URL-less state
  /// describes nothing, and a heading over it is worse than silence.
  #[must_use]
  pub fn lines(&self) -> Vec<String> {
    if self.url.is_empty() {
      return Vec::new();
    }
    let mut lines = vec![format!("- Page URL: {}", self.url)];
    if !self.title.is_empty() {
      lines.push(format!("- Page Title: {}", self.title));
    }
    if self.console_errors > 0 || self.console_warnings > 0 {
      lines.push(format!(
        "- Console: {} errors, {} warnings",
        self.console_errors, self.console_warnings
      ));
    }
    if self.page_errors > 0 {
      lines.push(format!("- Uncaught page errors: {}", self.page_errors));
    }
    lines
  }
}

// ── Secrets ─────────────────────────────────────────────────────────────────

/// Values that must not reach the caller verbatim.
///
/// A convenience, not a security boundary: it replaces known strings on the
/// way out. A value the operator never declared, or one the page reshapes
/// (base64, a substring, a re-encoding) still passes through — the caller is
/// responsible for what it does with tool output either way.
///
/// Entries are ordered longest-value-first so that when one secret contains
/// another, the longer one is replaced before its substring can be.
#[derive(Debug, Clone, Default)]
pub struct Secrets {
  entries: Vec<(String, String)>,
}

impl Secrets {
  /// Build from `name -> value` pairs. Empty values are dropped: an unset
  /// credential would otherwise match the empty string everywhere.
  #[must_use]
  pub fn new(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
    let mut entries: Vec<(String, String)> = pairs.into_iter().filter(|(_, value)| !value.is_empty()).collect();
    entries.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    Self { entries }
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }

  /// The declared name of an exactly-matching secret value.
  #[must_use]
  pub fn name_for(&self, value: &str) -> Option<&str> {
    self
      .entries
      .iter()
      .find(|(_, secret)| secret == value)
      .map(|(name, _)| name.as_str())
  }

  /// Replace every occurrence of a secret value with `<secret>NAME</secret>`.
  #[must_use]
  pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
    if self.entries.is_empty() {
      return Cow::Borrowed(text);
    }
    let mut out = Cow::Borrowed(text);
    for (name, value) in &self.entries {
      if out.contains(value.as_str()) {
        out = Cow::Owned(out.replace(value.as_str(), &format!("<secret>{name}</secret>")));
      }
    }
    out
  }

  /// [`Self::redact`] over every string in a JSON document, keys included —
  /// a credential used as an object key leaks exactly as readily as one used
  /// as a value.
  pub fn redact_json(&self, value: &mut serde_json::Value) {
    if self.entries.is_empty() {
      return;
    }
    match value {
      serde_json::Value::String(s) => {
        if let Cow::Owned(redacted) = self.redact(s) {
          *s = redacted;
        }
      },
      serde_json::Value::Array(items) => {
        for item in items {
          self.redact_json(item);
        }
      },
      serde_json::Value::Object(map) => {
        let needs_key_rewrite = map.keys().any(|k| matches!(self.redact(k), Cow::Owned(_)));
        if needs_key_rewrite {
          let rewritten: serde_json::Map<String, serde_json::Value> = std::mem::take(map)
            .into_iter()
            .map(|(k, v)| (self.redact(&k).into_owned(), v))
            .collect();
          *map = rewritten;
        }
        for item in map.values_mut() {
          self.redact_json(item);
        }
      },
      _ => {},
    }
  }

  /// The expression that reads this secret from the environment at runtime,
  /// in the language the echoed code is written in. This is what a generated
  /// test must contain instead of the literal.
  #[must_use]
  pub fn env_expression(name: &str, language: crate::codegen::OutputLanguage) -> String {
    match language {
      crate::codegen::OutputLanguage::TypeScript => format!("process.env['{name}']"),
      crate::codegen::OutputLanguage::Rust => format!("&std::env::var(\"{name}\").unwrap_or_default()"),
      // Gherkin has no expression language; the step text names the secret.
      crate::codegen::OutputLanguage::Gherkin => format!("<{name}>"),
    }
  }
}

// ── Output budget ───────────────────────────────────────────────────────────

/// A ceiling on the total size of an output directory.
///
/// Artifacts accumulate across a long-lived session — screenshots and traces
/// from calls whose results were read and forgotten. Without a ceiling the
/// directory grows for as long as the server runs.
#[derive(Debug, Clone, Copy)]
pub struct OutputBudget {
  pub max_bytes: u64,
}

/// What [`OutputBudget::enforce`] removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Evicted {
  pub files: usize,
  pub bytes: u64,
}

impl OutputBudget {
  #[must_use]
  pub fn new(max_bytes: u64) -> Self {
    Self { max_bytes }
  }

  /// Delete least-recently-modified files under `dir` until the total is
  /// within budget, never touching a path in `keep`.
  ///
  /// `keep` is what the current call produced: evicting an artifact whose
  /// link is in the very response being returned would hand the caller a
  /// dead link.
  ///
  /// Errors reading the directory are not failures of the call that triggered
  /// the sweep — an unreadable output dir means nothing was evicted, and the
  /// caller's result still stands.
  pub async fn enforce(&self, dir: &Path, keep: &BTreeSet<PathBuf>) -> Evicted {
    let Ok(mut entries) = list_files(dir).await else {
      return Evicted::default();
    };
    let mut total: u64 = entries.iter().map(|e| e.size).sum();
    if total <= self.max_bytes {
      return Evicted::default();
    }
    entries.sort_by_key(|e| e.modified);
    let mut evicted = Evicted::default();
    for entry in entries {
      if total <= self.max_bytes {
        break;
      }
      if keep.contains(&entry.path) {
        continue;
      }
      if tokio::fs::remove_file(&entry.path).await.is_ok() {
        total = total.saturating_sub(entry.size);
        evicted.files += 1;
        evicted.bytes += entry.size;
      }
    }
    evicted
  }
}

struct FileEntry {
  path: PathBuf,
  size: u64,
  modified: std::time::SystemTime,
}

/// Every file under `dir`, recursively, with the stats the sweep sorts on.
async fn list_files(dir: &Path) -> std::io::Result<Vec<FileEntry>> {
  let mut out = Vec::new();
  let mut stack = vec![dir.to_path_buf()];
  while let Some(current) = stack.pop() {
    let mut read = tokio::fs::read_dir(&current).await?;
    while let Some(entry) = read.next_entry().await? {
      let path = entry.path();
      let Ok(meta) = entry.metadata().await else { continue };
      if meta.is_dir() {
        stack.push(path);
      } else if meta.is_file() {
        out.push(FileEntry {
          path,
          size: meta.len(),
          modified: meta.modified().unwrap_or(std::time::UNIX_EPOCH),
        });
      }
    }
  }
  Ok(out)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn sections_render_in_order_with_fenced_code() {
    let mut response = Response::new();
    response.result(vec!["done".into()]);
    response.code(
      vec!["await page.goto('https://example.com');".into()],
      crate::codegen::OutputLanguage::TypeScript,
    );
    response.page(&PageState {
      url: "https://example.com/".into(),
      title: "Example".into(),
      console_errors: 1,
      console_warnings: 2,
      page_errors: 0,
    });
    assert_eq!(
      response.render(),
      "### Result\ndone\n\
       \n### Ran ferridriver code\n```ts\nawait page.goto('https://example.com');\n```\n\
       \n### Page\n- Page URL: https://example.com/\n- Page Title: Example\n- Console: 1 errors, 2 warnings\n"
    );
  }

  #[test]
  fn an_empty_section_adds_no_heading() {
    let mut response = Response::new();
    response.result(Vec::new());
    response.page(&PageState::default());
    assert!(
      response.render().is_empty(),
      "a section with nothing in it must not print a heading"
    );
    response.error(vec!["boom".into()]);
    assert_eq!(response.render(), "### Error\nboom\n");
  }

  #[test]
  fn json_keys_are_the_lowercased_titles() {
    let mut response = Response::new();
    response.result(vec!["v".into()]);
    response.code(vec!["line".into()], crate::codegen::OutputLanguage::Rust);
    assert_eq!(
      response.to_json(),
      serde_json::json!({ "result": "v", "ran_ferridriver_code": "line" })
    );
  }

  #[test]
  fn longest_secret_wins_when_one_contains_another() {
    let secrets = Secrets::new([
      ("SHORT".to_string(), "hunter".to_string()),
      ("LONG".to_string(), "hunter2000".to_string()),
    ]);
    assert_eq!(secrets.redact("token=hunter2000"), "token=<secret>LONG</secret>");
    assert_eq!(secrets.redact("token=hunter!"), "token=<secret>SHORT</secret>!");
  }

  #[test]
  fn empty_values_never_become_a_match_everywhere() {
    let secrets = Secrets::new([("UNSET".to_string(), String::new())]);
    assert!(secrets.is_empty());
    assert_eq!(secrets.redact("anything at all"), "anything at all");
  }

  #[test]
  fn json_redaction_covers_nested_values_and_keys() {
    let secrets = Secrets::new([("TOK".to_string(), "s3cr3t".to_string())]);
    let mut value = serde_json::json!({
      "headers": { "s3cr3t": ["bearer s3cr3t", 1] },
      "n": 5,
    });
    secrets.redact_json(&mut value);
    assert_eq!(
      value,
      serde_json::json!({
        "headers": { "<secret>TOK</secret>": ["bearer <secret>TOK</secret>", 1] },
        "n": 5,
      })
    );
  }

  #[test]
  fn a_response_redacts_every_section_it_renders() {
    let secrets = Secrets::new([("PW".to_string(), "hunter2".to_string())]);
    let mut response = Response::new().with_secrets(secrets);
    response.result(vec!["logged in as hunter2".into()]);
    assert_eq!(response.render(), "### Result\nlogged in as <secret>PW</secret>\n");
    assert_eq!(
      response.to_json(),
      serde_json::json!({ "result": "logged in as <secret>PW</secret>" })
    );
  }

  #[test]
  fn env_expressions_match_each_target_language() {
    use crate::codegen::OutputLanguage as L;
    assert_eq!(Secrets::env_expression("PW", L::TypeScript), "process.env['PW']");
    assert_eq!(
      Secrets::env_expression("PW", L::Rust),
      "&std::env::var(\"PW\").unwrap_or_default()"
    );
    assert_eq!(Secrets::env_expression("PW", L::Gherkin), "<PW>");
  }

  #[tokio::test]
  async fn the_budget_evicts_oldest_first_and_never_the_current_call_s_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    tokio::fs::create_dir_all(root.join("nested")).await.expect("mkdir");
    let paths = [root.join("old.bin"), root.join("nested/mid.bin"), root.join("new.bin")];
    for (i, path) in paths.iter().enumerate() {
      tokio::fs::write(path, vec![b'x'; 100]).await.expect("write");
      // Distinct mtimes, oldest first, without depending on the filesystem's
      // timestamp resolution to separate three writes issued back to back:
      // the sweep sorts on exactly this value.
      let stamp = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000 + i as u64 * 60);
      std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(stamp))
        .expect("mtime");
    }

    // 300 bytes on disk, 150 allowed, the newest file is this call's output:
    // the oldest goes, the middle one goes, the protected one stays even
    // though the directory is still over budget.
    let keep = BTreeSet::from([paths[2].clone()]);
    let evicted = OutputBudget::new(150).enforce(root, &keep).await;
    assert_eq!(evicted, Evicted { files: 2, bytes: 200 });
    assert!(!paths[0].exists(), "oldest evicted");
    assert!(!paths[1].exists(), "next-oldest evicted, nested dirs included");
    assert!(paths[2].exists(), "the current call's artifact survives");
  }

  #[tokio::test]
  async fn a_directory_within_budget_is_left_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    tokio::fs::write(dir.path().join("a.bin"), vec![b'x'; 10])
      .await
      .expect("write");
    let evicted = OutputBudget::new(1024).enforce(dir.path(), &BTreeSet::new()).await;
    assert_eq!(evicted, Evicted::default());
    assert!(dir.path().join("a.bin").exists());
  }

  #[tokio::test]
  async fn an_unreadable_directory_evicts_nothing_rather_than_failing() {
    let evicted = OutputBudget::new(0)
      .enforce(Path::new("/nonexistent/ferridriver-budget"), &BTreeSet::new())
      .await;
    assert_eq!(evicted, Evicted::default());
  }
}

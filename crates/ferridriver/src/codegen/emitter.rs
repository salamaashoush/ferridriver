//! Code emitters: convert recorded actions into test code.

use super::{Action, ClickOptions, RecordedAction};

/// Emits test code for a specific language.
pub trait CodeEmitter: Send + Sync {
  /// Opening boilerplate (imports, test function signature, initial navigation).
  fn header(&self, url: &str) -> String;
  /// The statements for one recorded action, each on its own line.
  fn action(&self, action: &RecordedAction) -> String;
  /// An already-rendered statement (an echoed action), placed in the body.
  fn line(&self, line: &str) -> String {
    format!("{line}\n")
  }
  /// Closing boilerplate (closing braces, etc.).
  fn footer(&self) -> String;
}

/// A whole file: the scaffolding around `actions`, with no opening
/// navigation of its own because a recording starts with one.
#[must_use]
pub fn render(emitter: &dyn CodeEmitter, actions: &[RecordedAction]) -> String {
  let mut source = emitter.header("");
  for action in actions {
    source.push_str(&emitter.action(action));
  }
  source.push_str(&emitter.footer());
  source
}

/// The code of a recording written while it happens.
///
/// The latest action is held back until the next one shows it is final: a
/// click can still become a double click, a fill can still grow, and an
/// action can still turn out to have opened a popup.
pub struct LiveSource {
  emitter: std::sync::Arc<dyn CodeEmitter>,
  pending: Option<RecordedAction>,
}

impl LiveSource {
  #[must_use]
  pub fn new(emitter: std::sync::Arc<dyn CodeEmitter>) -> Self {
    Self { emitter, pending: None }
  }

  /// Take the next recorded action; answers the code of the one before it
  /// once that one can no longer change. An action replacing an entry
  /// further back than the one held back changes code already written, so
  /// it is left out here and is right in the finished file.
  pub fn push(
    &mut self,
    action: RecordedAction,
    replaces_previous: bool,
    replaces_index: Option<usize>,
  ) -> Option<String> {
    if replaces_index.is_some() && !replaces_previous {
      return None;
    }
    let finished = if replaces_previous { None } else { self.pending.take() };
    self.pending = Some(action);
    finished.map(|action| self.emitter.action(&action))
  }

  /// The code of the action still held back.
  pub fn finish(&mut self) -> Option<String> {
    self.pending.take().map(|action| self.emitter.action(&action))
  }
}

/// Escape a string for use in a Rust string literal.
fn escape_rust(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  for ch in s.chars() {
    match ch {
      '\\' => out.push_str("\\\\"),
      '"' => out.push_str("\\\""),
      '\n' => out.push_str("\\n"),
      '\r' => out.push_str("\\r"),
      '\t' => out.push_str("\\t"),
      other => out.push(other),
    }
  }
  out
}

/// Escape a string for use in a JS/TS string literal (single quotes).
///
/// A raw line break ends a single-quoted literal, and U+2028/U+2029 did too
/// before ES2019, so a recorded textarea value has to be escaped, not quoted.
fn escape_js(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  for ch in s.chars() {
    match ch {
      '\\' => out.push_str("\\\\"),
      '\'' => out.push_str("\\'"),
      '\n' => out.push_str("\\n"),
      '\r' => out.push_str("\\r"),
      '\t' => out.push_str("\\t"),
      '\u{2028}' => out.push_str("\\u2028"),
      '\u{2029}' => out.push_str("\\u2029"),
      other => out.push(other),
    }
  }
  out
}

/// A click's options as a JavaScript object literal laid out one key per
/// line, keys in order, nested objects indented under their key.
fn js_click_options(options: &ClickOptions) -> String {
  let mut keys = Vec::new();
  if let Some(button) = &options.button {
    keys.push(format!("button: '{}'", escape_js(button)));
  }
  if let Some(count) = options.click_count {
    keys.push(format!("clickCount: {count}"));
  }
  if !options.modifiers.is_empty() {
    let names: Vec<String> = options
      .modifiers
      .iter()
      .map(|m| format!("'{}'", escape_js(m)))
      .collect();
    keys.push(format!("modifiers: [{}]", names.join(", ")));
  }
  if let Some(position) = options.position {
    keys.push(format!(
      "position: {{\n    x: {},\n    y: {}\n  }}",
      position.x, position.y
    ));
  }
  if keys.is_empty() {
    return String::new();
  }
  format!("{{\n  {}\n}}", keys.join(",\n  "))
}

/// A click's options as Rust: the options call on the action, or nothing
/// for a plain click.
fn rust_click_options(options: &ClickOptions, double: bool) -> String {
  if options.is_plain() {
    return String::new();
  }
  let mut fields = Vec::new();
  if let Some(button) = &options.button {
    let variant = if button == "middle" { "Middle" } else { "Right" };
    fields.push(format!("button: Some(ferridriver::options::MouseButton::{variant})"));
  }
  if let Some(count) = options.click_count.filter(|_| !double) {
    fields.push(format!("click_count: Some({count})"));
  }
  if !options.modifiers.is_empty() {
    let modifiers: Vec<String> = options
      .modifiers
      .iter()
      .map(|m| format!("ferridriver::options::Modifier::{m}"))
      .collect();
    fields.push(format!("modifiers: vec![{}]", modifiers.join(", ")));
  }
  if let Some(position) = options.position {
    fields.push(format!(
      "position: Some(ferridriver::options::Point {{ x: {}.0, y: {}.0 }})",
      position.x, position.y
    ));
  }
  let kind = if double { "DblClickOptions" } else { "ClickOptions" };
  format!(
    ".options(ferridriver::options::{kind} {{ {}, ..Default::default() }})",
    fields.join(", ")
  )
}

fn quote_js_list(values: &[String]) -> String {
  match values {
    [single] => format!("'{}'", escape_js(single)),
    many => format!(
      "[{}]",
      many
        .iter()
        .map(|v| format!("'{}'", escape_js(v)))
        .collect::<Vec<_>>()
        .join(", ")
    ),
  }
}

// ─── Rust ────────────────────────────────────────────────────────────────────

pub struct RustEmitter;

impl CodeEmitter for RustEmitter {
  fn header(&self, url: &str) -> String {
    // An empty url means the caller's own lines carry the navigation (a
    // recording starts with one, and the action echo records the `goto` it
    // saw).
    let navigation = if url.is_empty() {
      String::new()
    } else {
      format!("  page.goto(\"{}\").await?;\n", escape_rust(url))
    };
    format!(
      "use ferridriver_test::prelude::*;\n\n#[ferritest]\nasync fn recorded_test(page: Arc<Page>) {{\n{navigation}"
    )
  }

  fn action(&self, recorded: &RecordedAction) -> String {
    let page = recorded.page.as_str();
    let locator = |selector: &str| format!("{page}.locator(\"{}\")", escape_rust(selector));
    let call = match &recorded.action {
      Action::Navigate { url } => format!("{page}.goto(\"{}\")", escape_rust(url)),
      Action::Click { selector, options, .. } => {
        format!("{}.click(){}", locator(selector), rust_click_options(options, false))
      },
      Action::Dblclick { selector, options, .. } => {
        format!("{}.dblclick(){}", locator(selector), rust_click_options(options, true))
      },
      Action::Fill { selector, value, .. } => {
        format!("{}.fill(\"{}\")", locator(selector), escape_rust(value))
      },
      Action::Press { selector, key, .. } => {
        format!("{}.press(\"{}\")", locator(selector), escape_rust(key))
      },
      Action::Select { selector, values, .. } => {
        let values = match values.as_slice() {
          [single] => format!("\"{}\"", escape_rust(single)),
          many => format!(
            "[{}]",
            many
              .iter()
              .map(|v| format!("\"{}\"", escape_rust(v)))
              .collect::<Vec<_>>()
              .join(", ")
          ),
        };
        format!("{}.select_option({values})", locator(selector))
      },
      Action::Check { selector, .. } => format!("{}.check()", locator(selector)),
      Action::Uncheck { selector, .. } => format!("{}.uncheck()", locator(selector)),
      Action::Upload { selector, files, .. } => {
        let files = match files.as_slice() {
          [single] => format!("\"{}\"", escape_rust(single)),
          many => format!(
            "vec![{}]",
            many
              .iter()
              .map(|f| format!("std::path::PathBuf::from(\"{}\")", escape_rust(f)))
              .collect::<Vec<_>>()
              .join(", ")
          ),
        };
        format!("{}.set_input_files({files})", locator(selector))
      },
      Action::AssertVisible { selector, .. } => format!("expect(&{}).to_be_visible()", locator(selector)),
      Action::AssertText {
        selector,
        text,
        substring,
        ..
      } => {
        let matcher = if *substring { "to_contain_text" } else { "to_have_text" };
        format!("expect(&{}).{matcher}(\"{}\")", locator(selector), escape_rust(text))
      },
      Action::AssertValue { selector, value, .. } if value.is_empty() => {
        format!("expect(&{}).to_be_empty()", locator(selector))
      },
      Action::AssertValue { selector, value, .. } => format!(
        "expect(&{}).to_have_value(\"{}\")",
        locator(selector),
        escape_rust(value)
      ),
      Action::AssertChecked { selector, checked, .. } => {
        let not = if *checked { "" } else { ".not()" };
        format!("expect(&{}){not}.to_be_checked()", locator(selector))
      },
      Action::AssertUrl { url } => format!("expect(&{page}).to_have_url(\"{}\")", escape_rust(url)),
      Action::AssertTitle { title } => format!("expect(&{page}).to_have_title(\"{}\")", escape_rust(title)),
    };
    match &recorded.popup {
      Some(popup) => format!("  let ({popup}, _) = {page}.expect_popup(|| {call}).await?;\n"),
      None => format!("  {call}.await?;\n"),
    }
  }

  fn line(&self, line: &str) -> String {
    format!("  {line}\n")
  }

  fn footer(&self) -> String {
    "}\n".into()
  }
}

// ─── TypeScript ──────────────────────────────────────────────────────────────

/// One TypeScript statement for `action` on the page named `page`, the same
/// vocabulary a script author, the action echo and a test body use.
fn typescript_statement(page: &str, action: &Action) -> String {
  match action {
    Action::Navigate { url } => format!("await {page}.goto('{}');", escape_js(url)),
    Action::Click { locator, options, .. } => {
      format!("await {page}.{locator}.click({});", js_click_options(options))
    },
    Action::Dblclick { locator, options, .. } => {
      format!("await {page}.{locator}.dblclick({});", js_click_options(options))
    },
    Action::Fill { locator, value, .. } => format!("await {page}.{locator}.fill('{}');", escape_js(value)),
    Action::Press { locator, key, .. } => format!("await {page}.{locator}.press('{}');", escape_js(key)),
    Action::Select { locator, values, .. } => {
      format!("await {page}.{locator}.selectOption({});", quote_js_list(values))
    },
    Action::Check { locator, .. } => format!("await {page}.{locator}.check();"),
    Action::Uncheck { locator, .. } => format!("await {page}.{locator}.uncheck();"),
    Action::Upload { locator, files, .. } => {
      format!("await {page}.{locator}.setInputFiles({});", quote_js_list(files))
    },
    Action::AssertVisible { locator, .. } => format!("await expect({page}.{locator}).toBeVisible();"),
    Action::AssertText {
      locator,
      text,
      substring,
      ..
    } => {
      let matcher = if *substring { "toContainText" } else { "toHaveText" };
      format!("await expect({page}.{locator}).{matcher}('{}');", escape_js(text))
    },
    Action::AssertValue { locator, value, .. } if value.is_empty() => {
      format!("await expect({page}.{locator}).toBeEmpty();")
    },
    Action::AssertValue { locator, value, .. } => {
      format!("await expect({page}.{locator}).toHaveValue('{}');", escape_js(value))
    },
    Action::AssertChecked { locator, checked, .. } => {
      let not = if *checked { "" } else { ".not" };
      format!("await expect({page}.{locator}){not}.toBeChecked();")
    },
    Action::AssertUrl { url } => format!("await expect({page}).toHaveURL('{}');", escape_js(url)),
    Action::AssertTitle { title } => format!("await expect({page}).toHaveTitle('{}');", escape_js(title)),
  }
}

/// The statements for `recorded`: the action, and around it the wait for
/// the popup it opened.
fn typescript_statements(recorded: &RecordedAction) -> Vec<String> {
  let page = recorded.page.as_str();
  let statement = typescript_statement(page, &recorded.action);
  match &recorded.popup {
    Some(popup) => vec![
      format!("const {popup}Promise = {page}.waitForEvent('popup');"),
      statement,
      format!("const {popup} = await {popup}Promise;"),
    ],
    None => vec![statement],
  }
}

fn typescript_lines(recorded: &RecordedAction, indent: &str) -> String {
  let mut lines = String::new();
  for statement in typescript_statements(recorded) {
    for line in statement.lines() {
      lines.push_str(indent);
      lines.push_str(line);
      lines.push('\n');
    }
  }
  lines
}

pub struct TypeScriptEmitter;

impl CodeEmitter for TypeScriptEmitter {
  fn header(&self, url: &str) -> String {
    // Adaptive preamble: reuse the live `page` global when present (a session
    // run and the MCP `run_script` tool both inject one), otherwise launch a
    // browser and define it. Either way the body below is written against
    // plain `page` — the same vocabulary a script author, the action echo,
    // and this recorder all use, so lines from any of them paste together.
    let navigation = if url.is_empty() {
      String::new()
    } else {
      format!("await page.goto('{}');\n", escape_js(url))
    };
    format!(
      "// Recorded with `ferridriver codegen`.\n\
       // Run standalone: `ferridriver run <file>`.\n\
       // Replay on a live session: `ferridriver run --session <id> <file>`.\n\
       if (typeof page === 'undefined') {{\n\
       \x20 globalThis.__browser = await chromium().launch();\n\
       \x20 globalThis.page = await globalThis.__browser.newPage();\n\
       }}\n\
       {navigation}"
    )
  }

  fn action(&self, action: &RecordedAction) -> String {
    typescript_lines(action, "")
  }

  fn footer(&self) -> String {
    // Close only the browser we launched; an injected session `page` lives on.
    "if (globalThis.__browser) await globalThis.__browser.close();\n".into()
  }
}

/// A plain script: the recording as top-level statements against the
/// `page` a `[test] scriptMatch` run puts in scope, with nothing to launch
/// and nothing to close.
pub struct TypeScriptPlainEmitter;

impl CodeEmitter for TypeScriptPlainEmitter {
  fn header(&self, url: &str) -> String {
    if url.is_empty() {
      String::new()
    } else {
      format!("await page.goto('{}');\n", escape_js(url))
    }
  }

  fn action(&self, action: &RecordedAction) -> String {
    typescript_lines(action, "")
  }

  fn footer(&self) -> String {
    String::new()
  }
}

/// An `@ferridriver/test` spec: one `test` whose body is the recording, run
/// by `ferridriver test` against the `page` fixture.
pub struct TypeScriptTestEmitter {
  title: String,
}

impl TypeScriptTestEmitter {
  #[must_use]
  pub fn new(title: impl Into<String>) -> Self {
    Self { title: title.into() }
  }
}

impl Default for TypeScriptTestEmitter {
  /// Playwright's codegen names its test `test` too.
  fn default() -> Self {
    Self::new("test")
  }
}

impl CodeEmitter for TypeScriptTestEmitter {
  fn header(&self, url: &str) -> String {
    let navigation = if url.is_empty() {
      String::new()
    } else {
      format!("  await page.goto('{}');\n", escape_js(url))
    };
    format!(
      "import {{ test, expect }} from '@ferridriver/test';\n\ntest('{}', async ({{ page }}) => {{\n{navigation}",
      escape_js(&self.title)
    )
  }

  fn action(&self, action: &RecordedAction) -> String {
    typescript_lines(action, "  ")
  }

  fn line(&self, line: &str) -> String {
    format!("  {line}\n")
  }

  fn footer(&self) -> String {
    "});\n".into()
  }
}

// ─── Gherkin ─────────────────────────────────────────────────────────────────

const KEYWORD_NONE: u8 = 0;
const KEYWORD_WHEN: u8 = 1;
const KEYWORD_THEN: u8 = 2;

pub struct GherkinEmitter {
  /// The keyword the previous step opened with, so a run of interactions
  /// reads `When ... And ...` and a run of assertions `Then ... And ...`.
  last_keyword: std::sync::atomic::AtomicU8,
  /// The tab the previous step acted on.
  tab: std::sync::atomic::AtomicUsize,
}

impl Default for GherkinEmitter {
  fn default() -> Self {
    Self::new()
  }
}

impl GherkinEmitter {
  #[must_use]
  pub fn new() -> Self {
    Self {
      last_keyword: std::sync::atomic::AtomicU8::new(KEYWORD_NONE),
      tab: std::sync::atomic::AtomicUsize::new(0),
    }
  }

  fn keyword(&self, assertion: bool) -> &'static str {
    let wanted = if assertion { KEYWORD_THEN } else { KEYWORD_WHEN };
    let previous = self.last_keyword.swap(wanted, std::sync::atomic::Ordering::Relaxed);
    match (previous == wanted, assertion) {
      (true, _) => "And",
      (false, true) => "Then",
      (false, false) => "When",
    }
  }
}

impl CodeEmitter for GherkinEmitter {
  fn header(&self, url: &str) -> String {
    // As in the other emitters, an empty url means the body already carries
    // the navigation step.
    let given = if url.is_empty() {
      String::new()
    } else {
      format!("    Given I navigate to \"{url}\"\n")
    };
    format!("Feature: Recorded test\n\n  Scenario: User interaction recording\n{given}")
  }

  fn action(&self, recorded: &RecordedAction) -> String {
    // Pages are tabs in the order they opened, which is the order their
    // names were given in.
    let tab = recorded
      .page
      .strip_prefix(super::FIRST_PAGE)
      .and_then(|n| if n.is_empty() { Some(0) } else { n.parse().ok() })
      .unwrap_or(0);
    let switch = if self.tab.swap(tab, std::sync::atomic::Ordering::Relaxed) == tab {
      String::new()
    } else {
      format!("    {} I switch to tab {tab}\n", self.keyword(false))
    };
    let action = &recorded.action;
    let kw = self.keyword(action.is_assertion());
    let step = match action {
      Action::Navigate { url } => format!("I navigate to \"{url}\""),
      Action::Click { selector, options, .. } if options.button.as_deref() == Some("right") => {
        format!("I right click \"{selector}\"")
      },
      Action::Click { selector, .. } => format!("I click \"{selector}\""),
      Action::Dblclick { selector, .. } => format!("I double click \"{selector}\""),
      Action::Fill { selector, value, .. } => format!("I fill \"{selector}\" with \"{value}\""),
      Action::Press { selector, key, .. } => format!("I press \"{key}\" on \"{selector}\""),
      Action::Select { selector, values, .. } => {
        format!("I select \"{}\" from \"{selector}\"", values.join(","))
      },
      Action::Check { selector, .. } => format!("I check \"{selector}\""),
      Action::Uncheck { selector, .. } => format!("I uncheck \"{selector}\""),
      Action::Upload { selector, files, .. } => {
        format!("I attach file \"{}\" to \"{selector}\"", files.join(","))
      },
      Action::AssertVisible { selector, .. } => format!("\"{selector}\" should be visible"),
      Action::AssertText {
        selector,
        text,
        substring: true,
        ..
      } => format!("\"{selector}\" should contain text \"{text}\""),
      Action::AssertText { selector, text, .. } => format!("\"{selector}\" should have text \"{text}\""),
      Action::AssertValue { selector, value, .. } if value.is_empty() => format!("\"{selector}\" should be empty"),
      Action::AssertValue { selector, value, .. } => format!("\"{selector}\" should have value \"{value}\""),
      Action::AssertChecked {
        selector,
        checked: true,
        ..
      } => format!("\"{selector}\" should be checked"),
      Action::AssertChecked { selector, .. } => format!("\"{selector}\" should not be checked"),
      Action::AssertUrl { url } => format!("the URL should be \"{url}\""),
      Action::AssertTitle { title } => format!("the page title should be \"{title}\""),
    };
    format!("{switch}    {kw} {step}\n")
  }

  fn line(&self, line: &str) -> String {
    format!("    {line}\n")
  }

  fn footer(&self) -> String {
    String::new()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Emit an action done on the first page.
  trait Emit {
    fn emit(&self, action: &Action) -> String;
  }

  impl<E: CodeEmitter + ?Sized> Emit for E {
    fn emit(&self, action: &Action) -> String {
      self.action(&RecordedAction::on_first_page(action.clone()))
    }
  }

  fn on_first_page(actions: &[Action]) -> Vec<RecordedAction> {
    actions.iter().cloned().map(RecordedAction::on_first_page).collect()
  }

  fn click(locator: &str) -> Action {
    Action::Click {
      selector: "internal:role=button[name=\"Go\"i]".into(),
      locator: locator.into(),
      options: ClickOptions::default(),
    }
  }

  #[test]
  fn typescript_emits_runnable_adaptive_script() {
    let e = TypeScriptEmitter;

    let header = e.header("https://example.com");
    // Reuses an injected session `page`; otherwise launches its own browser
    // and defines one — either way the body drives plain `page`.
    assert!(header.contains("if (typeof page === 'undefined')"));
    assert!(header.contains("globalThis.__browser = await chromium().launch();"));
    assert!(header.contains("globalThis.page = await globalThis.__browser.newPage();"));
    assert!(header.contains("await page.goto('https://example.com');"));
    // No dead test-runner import — it must run under `ferridriver run` / MCP.
    assert!(!header.contains("import { test }"));

    assert_eq!(
      e.emit(&click("getByRole('button', { name: 'Go' })")),
      "await page.getByRole('button', { name: 'Go' }).click();\n"
    );

    let fill = e.emit(&Action::Fill {
      selector: "#email".into(),
      locator: "getByLabel('Email')".into(),
      value: "a@b.com".into(),
    });
    assert_eq!(fill, "await page.getByLabel('Email').fill('a@b.com');\n");

    assert!(
      e.footer()
        .contains("if (globalThis.__browser) await globalThis.__browser.close();")
    );
  }

  #[test]
  fn a_headerless_recording_skips_the_opening_navigation() {
    // A session recording starts wherever the session already is, so an empty
    // url must not emit `goto('')`.
    let header = TypeScriptEmitter.header("");
    assert!(header.contains("if (typeof page === 'undefined')"));
    assert!(!header.contains("goto("), "empty url must not navigate: {header}");
  }

  #[test]
  fn the_test_scaffold_wraps_the_recording_in_one_ferridriver_test() {
    let actions = [
      Action::Navigate {
        url: "https://example.com/login".into(),
      },
      Action::Fill {
        selector: "internal:label=\"Email\"i".into(),
        locator: "getByLabel('Email')".into(),
        value: "sashoush@example.com".into(),
      },
      click("getByRole('button', { name: 'Sign in' })"),
      Action::AssertText {
        selector: "#greeting".into(),
        locator: "locator('#greeting')".into(),
        text: "Welcome".into(),
        substring: true,
      },
      Action::AssertUrl {
        url: "https://example.com/home".into(),
      },
    ];
    let source = render(&TypeScriptTestEmitter::new("signs in"), &on_first_page(&actions));
    assert_eq!(
      source,
      "import { test, expect } from '@ferridriver/test';\n\
       \n\
       test('signs in', async ({ page }) => {\n\
       \x20 await page.goto('https://example.com/login');\n\
       \x20 await page.getByLabel('Email').fill('sashoush@example.com');\n\
       \x20 await page.getByRole('button', { name: 'Sign in' }).click();\n\
       \x20 await expect(page.locator('#greeting')).toContainText('Welcome');\n\
       \x20 await expect(page).toHaveURL('https://example.com/home');\n\
       });\n"
    );
  }

  #[test]
  fn the_plain_scaffold_is_the_recording_alone() {
    let actions = [
      Action::Navigate {
        url: "https://example.com/login".into(),
      },
      click("getByRole('button', { name: 'Sign in' })"),
      Action::AssertUrl {
        url: "https://example.com/home".into(),
      },
    ];
    assert_eq!(
      render(&TypeScriptPlainEmitter, &on_first_page(&actions)),
      "await page.goto('https://example.com/login');\n\
       await page.getByRole('button', { name: 'Sign in' }).click();\n\
       await expect(page).toHaveURL('https://example.com/home');\n"
    );
    assert_eq!(
      TypeScriptPlainEmitter.header("https://example.com/"),
      "await page.goto('https://example.com/');\n"
    );
    assert_eq!(
      super::super::Scaffold::parse("plain").map(|s| super::super::OutputLanguage::TypeScript.emitter_for(s).footer()),
      Ok(String::new())
    );
  }

  #[test]
  fn the_test_scaffold_indents_echoed_lines() {
    let e = TypeScriptTestEmitter::default();
    assert!(e.header("").contains("test('test', async ({ page }) => {"));
    assert_eq!(
      e.line("await page.locator('#go').click();"),
      "  await page.locator('#go').click();\n"
    );
  }

  #[test]
  fn assertions_render_the_matcher_their_shape_calls_for() {
    let cases = [
      (
        Action::AssertVisible {
          selector: "#a".into(),
          locator: "locator('#a')".into(),
        },
        "await expect(page.locator('#a')).toBeVisible();",
        "expect(&page.locator(\"#a\")).to_be_visible().await?;",
      ),
      (
        Action::AssertText {
          selector: "#a".into(),
          locator: "locator('#a')".into(),
          text: "exact".into(),
          substring: false,
        },
        "await expect(page.locator('#a')).toHaveText('exact');",
        "expect(&page.locator(\"#a\")).to_have_text(\"exact\").await?;",
      ),
      (
        Action::AssertValue {
          selector: "#a".into(),
          locator: "locator('#a')".into(),
          value: String::new(),
        },
        "await expect(page.locator('#a')).toBeEmpty();",
        "expect(&page.locator(\"#a\")).to_be_empty().await?;",
      ),
      (
        Action::AssertValue {
          selector: "#a".into(),
          locator: "locator('#a')".into(),
          value: "42".into(),
        },
        "await expect(page.locator('#a')).toHaveValue('42');",
        "expect(&page.locator(\"#a\")).to_have_value(\"42\").await?;",
      ),
      (
        Action::AssertChecked {
          selector: "#a".into(),
          locator: "locator('#a')".into(),
          checked: false,
        },
        "await expect(page.locator('#a')).not.toBeChecked();",
        "expect(&page.locator(\"#a\")).not().to_be_checked().await?;",
      ),
      (
        Action::AssertTitle { title: "Home".into() },
        "await expect(page).toHaveTitle('Home');",
        "expect(&page).to_have_title(\"Home\").await?;",
      ),
    ];
    for (action, typescript, rust) in cases {
      assert_eq!(TypeScriptEmitter.emit(&action), format!("{typescript}\n"));
      assert_eq!(RustEmitter.emit(&action), format!("  {rust}\n"));
    }
  }

  #[test]
  fn multi_value_selects_and_uploads_render_lists() {
    let select = Action::Select {
      selector: "#s".into(),
      locator: "locator('#s')".into(),
      values: vec!["a".into(), "b".into()],
    };
    assert_eq!(
      TypeScriptEmitter.emit(&select),
      "await page.locator('#s').selectOption(['a', 'b']);\n"
    );
    assert_eq!(
      RustEmitter.emit(&select),
      "  page.locator(\"#s\").select_option([\"a\", \"b\"]).await?;\n"
    );
    let upload = Action::Upload {
      selector: "#f".into(),
      locator: "getByLabel('Avatar')".into(),
      files: vec!["photo.png".into()],
    };
    assert_eq!(
      TypeScriptEmitter.emit(&upload),
      "await page.getByLabel('Avatar').setInputFiles('photo.png');\n"
    );
  }

  #[test]
  fn line_breaks_in_values_stay_inside_the_literal() {
    let fill = Action::Fill {
      selector: "#notes".into(),
      locator: "locator('#notes')".into(),
      value: "first\nit's\tsecond".into(),
    };
    assert_eq!(
      TypeScriptEmitter.emit(&fill),
      "await page.locator('#notes').fill('first\\nit\\'s\\tsecond');\n"
    );
    assert_eq!(
      RustEmitter.emit(&fill),
      "  page.locator(\"#notes\").fill(\"first\\nit's\\tsecond\").await?;\n"
    );
  }

  #[test]
  fn gherkin_switches_keyword_between_interactions_and_assertions() {
    let e = GherkinEmitter::new();
    let visible = Action::AssertVisible {
      selector: "#ok".into(),
      locator: "locator('#ok')".into(),
    };
    let lines = [
      e.emit(&click("x")),
      e.emit(&click("x")),
      e.emit(&visible),
      e.emit(&visible),
      e.emit(&click("x")),
    ];
    let keywords: Vec<&str> = lines
      .iter()
      .map(|l| l.split_whitespace().next().unwrap_or_default())
      .collect();
    assert_eq!(keywords, ["When", "And", "Then", "And", "When"]);
    assert_eq!(lines[2], "    Then \"#ok\" should be visible\n");
  }

  #[test]
  fn a_popup_is_awaited_around_the_action_that_opened_it() {
    let opened = RecordedAction {
      page: "page".into(),
      action: Action::Click {
        selector: "internal:role=link[name=\"Details\"i]".into(),
        locator: "getByRole('link', { name: 'Details' })".into(),
        options: ClickOptions::default(),
      },
      popup: Some("page1".into()),
    };
    let typed = RecordedAction {
      page: "page1".into(),
      action: Action::Fill {
        selector: "#email".into(),
        locator: "getByLabel('Email')".into(),
        value: "sashoush@example.com".into(),
      },
      popup: None,
    };
    let back = RecordedAction::on_first_page(Action::AssertTitle { title: "Shop".into() });
    let actions = [opened, typed, back];
    assert_eq!(
      render(&TypeScriptTestEmitter::default(), &actions),
      "import { test, expect } from '@ferridriver/test';\n\
       \n\
       test('test', async ({ page }) => {\n\
       \x20 const page1Promise = page.waitForEvent('popup');\n\
       \x20 await page.getByRole('link', { name: 'Details' }).click();\n\
       \x20 const page1 = await page1Promise;\n\
       \x20 await page1.getByLabel('Email').fill('sashoush@example.com');\n\
       \x20 await expect(page).toHaveTitle('Shop');\n\
       });\n"
    );
    assert_eq!(
      RustEmitter.action(&actions[0]),
      "  let (page1, _) = page.expect_popup(|| page.locator(\"internal:role=link[name=\\\"Details\\\"i]\").click()).await?;\n"
    );
    assert_eq!(
      RustEmitter.action(&actions[1]),
      "  page1.locator(\"#email\").fill(\"sashoush@example.com\").await?;\n"
    );
    let gherkin = GherkinEmitter::new();
    let steps: Vec<String> = actions.iter().map(|action| gherkin.action(action)).collect();
    assert_eq!(
      steps,
      [
        "    When I click \"internal:role=link[name=\"Details\"i]\"\n",
        "    And I switch to tab 1\n    And I fill \"#email\" with \"sashoush@example.com\"\n",
        "    And I switch to tab 0\n    Then the page title should be \"Shop\"\n",
      ]
    );
  }

  #[test]
  fn click_options_render_as_the_options_object() {
    let on = |options: ClickOptions| Action::Click {
      selector: "#a".into(),
      locator: "locator('#a')".into(),
      options,
    };
    let triple = on(ClickOptions {
      click_count: Some(3),
      ..ClickOptions::default()
    });
    let held = on(ClickOptions {
      button: Some("middle".into()),
      modifiers: vec!["Alt".into(), "ControlOrMeta".into()],
      ..ClickOptions::default()
    });
    let canvas = on(ClickOptions {
      position: Some(super::super::ClickPosition { x: 4, y: 6 }),
      ..ClickOptions::default()
    });
    let double = Action::Dblclick {
      selector: "#a".into(),
      locator: "locator('#a')".into(),
      options: ClickOptions {
        modifiers: vec!["Shift".into()],
        ..ClickOptions::default()
      },
    };
    let actions = on_first_page(&[triple, held, canvas, double]);
    assert_eq!(
      render(&TypeScriptPlainEmitter, &actions),
      "await page.locator('#a').click({\n  clickCount: 3\n});\n\
       await page.locator('#a').click({\n  button: 'middle',\n  modifiers: ['Alt', 'ControlOrMeta']\n});\n\
       await page.locator('#a').click({\n  position: {\n    x: 4,\n    y: 6\n  }\n});\n\
       await page.locator('#a').dblclick({\n  modifiers: ['Shift']\n});\n"
    );
    assert_eq!(
      TypeScriptTestEmitter::default().action(&actions[0]),
      "  await page.locator('#a').click({\n    clickCount: 3\n  });\n"
    );
    assert_eq!(
      RustEmitter.action(&actions[1]),
      "  page.locator(\"#a\").click().options(ferridriver::options::ClickOptions { \
       button: Some(ferridriver::options::MouseButton::Middle), \
       modifiers: vec![ferridriver::options::Modifier::Alt, ferridriver::options::Modifier::ControlOrMeta], \
       ..Default::default() }).await?;\n"
    );
    assert_eq!(
      RustEmitter.action(&actions[3]),
      "  page.locator(\"#a\").dblclick().options(ferridriver::options::DblClickOptions { \
       modifiers: vec![ferridriver::options::Modifier::Shift], ..Default::default() }).await?;\n"
    );
    let right = on(ClickOptions {
      button: Some("right".into()),
      ..ClickOptions::default()
    });
    assert_eq!(GherkinEmitter::new().emit(&right), "    When I right click \"#a\"\n");
    assert_eq!(
      serde_json::to_value(&actions[0].action).unwrap(),
      serde_json::json!({ "type": "click", "selector": "#a", "locator": "locator('#a')", "options": { "clickCount": 3 } })
    );
    assert_eq!(
      serde_json::to_value(click("x")).unwrap().get("options"),
      None,
      "a plain click carries no options"
    );
  }

  #[test]
  fn live_source_writes_an_action_once_it_is_final() {
    let mut live = LiveSource::new(std::sync::Arc::new(TypeScriptPlainEmitter));
    let fill = |value: &str| {
      RecordedAction::on_first_page(Action::Fill {
        selector: "#q".into(),
        locator: "locator('#q')".into(),
        value: value.into(),
      })
    };
    assert_eq!(live.push(fill("a"), false, None), None);
    assert_eq!(live.push(fill("ab"), true, Some(0)), None);
    assert_eq!(
      live.push(
        RecordedAction::on_first_page(click("getByRole('button', { name: 'Go' })")),
        false,
        None
      ),
      Some("await page.locator('#q').fill('ab');\n".to_string())
    );
    assert_eq!(live.push(fill("written"), false, Some(0)), None);
    assert_eq!(
      live.finish(),
      Some("await page.getByRole('button', { name: 'Go' }).click();\n".to_string())
    );
    assert_eq!(live.finish(), None);
  }

  #[test]
  fn actions_keep_the_page_side_wire_shape() {
    let wire = serde_json::json!({
      "type": "assertText",
      "selector": "#a",
      "locator": "locator('#a')",
      "text": "hi",
    });
    let action: Action = serde_json::from_value(wire).unwrap();
    assert_eq!(
      action,
      Action::AssertText {
        selector: "#a".into(),
        locator: "locator('#a')".into(),
        text: "hi".into(),
        substring: false,
      }
    );
    let back = serde_json::to_value(&action).unwrap();
    assert_eq!(back["type"], "assertText");
    assert_eq!(back["substring"], false);
  }
}

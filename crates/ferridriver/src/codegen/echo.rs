//! Code echo: turning the actions a script actually performed into source.
//!
//! [`crate::codegen::recorder`] generates code from what a *user* does in a
//! headed browser. This module generates it from what an *API caller* does —
//! every `page.*` / `locator.*` / `expect.*` call already opens a trace action
//! carrying its class, method and parameters, so the same run that drives the
//! browser can hand back the code that would reproduce it.
//!
//! That is the raw material for two things: the "ran code" an agent sees
//! alongside a result, and a generated test file.
//!
//! Every action produces a line, including ones no curated emitter covers:
//! showing `await page.setViewportSize({...})` verbatim is honest, whereas
//! dropping it would silently misrepresent what ran.

use crate::response::Secrets;
use crate::trace::{ActionInfo, ActionObserver};

use super::OutputLanguage;

/// Render one action as a line of `language`. `None` for actions that are not
/// user-reproducible calls at all (internal spans without a method).
#[must_use]
pub fn line_for(action: &ActionInfo, language: OutputLanguage) -> Option<String> {
  line_for_with_secrets(action, language, &Secrets::default())
}

/// [`line_for`], with declared secret values replaced by the expression that
/// reads them from the environment.
///
/// A generated test that carries a literal password is not a test anyone can
/// commit, so the substitution happens while the argument is still a value —
/// after rendering, a credential is indistinguishable from any other quoted
/// string and stripping it would mean pattern-matching source.
#[must_use]
pub fn line_for_with_secrets(action: &ActionInfo, language: OutputLanguage, secrets: &Secrets) -> Option<String> {
  if action.method.is_empty() {
    return None;
  }
  Some(match language {
    OutputLanguage::TypeScript => typescript_line(action, secrets),
    OutputLanguage::Rust => rust_line(action, secrets),
    OutputLanguage::Gherkin => gherkin_line(action, secrets),
  })
}

/// The receiver expression an action applies to: `page`, `page.locator('x')`,
/// or `expect(page.locator('x'))`.
fn receiver_ts(action: &ActionInfo) -> String {
  let selector = action.params.get("selector").and_then(serde_json::Value::as_str);
  match (action.class.as_str(), selector) {
    ("Locator", Some(selector)) => format!("page.locator({})", quote_js(selector)),
    ("Expect", Some(selector)) => format!("expect(page.locator({}))", quote_js(selector)),
    ("Expect", None) => "expect(page)".to_string(),
    _ => "page".to_string(),
  }
}

fn typescript_line(action: &ActionInfo, secrets: &Secrets) -> String {
  let receiver = receiver_ts(action);
  let args = call_args(action)
    .iter()
    .map(|(key, value)| match locator_arg(key, value) {
      Some(selector) => format!("page.locator({})", quote_js(selector)),
      None => render_js(value, secrets),
    })
    .collect::<Vec<_>>()
    .join(", ");
  format!("await {receiver}.{}({args});", action.method)
}

fn rust_line(action: &ActionInfo, secrets: &Secrets) -> String {
  let selector = action.params.get("selector").and_then(serde_json::Value::as_str);
  let receiver = match (action.class.as_str(), selector) {
    ("Locator" | "Expect", Some(selector)) => format!("page.locator({})", quote_rust(selector)),
    _ => "page".to_string(),
  };
  let args = call_args(action)
    .iter()
    .map(|(key, value)| match locator_arg(key, value) {
      Some(selector) => format!("&page.locator({})", quote_rust(selector)),
      None => render_rust(value, secrets),
    })
    .collect::<Vec<_>>()
    .join(", ");
  let call = format!("{receiver}.{}({args}).await?", to_snake_case(&action.method));
  if action.class == "Expect" {
    // The assertion crate's builder shape, so the line pastes into a
    // `#[ferritest]` body as-is.
    return format!("expect({call}).await?;");
  }
  format!("{call};")
}

fn gherkin_line(action: &ActionInfo, secrets: &Secrets) -> String {
  let selector = action
    .params
    .get("selector")
    .and_then(serde_json::Value::as_str)
    .unwrap_or_default();
  let value = call_args(action)
    .first()
    .map(|(_, value)| render_plain(value, secrets))
    .unwrap_or_default();
  match (action.method.as_str(), selector.is_empty(), value.is_empty()) {
    ("goto", _, false) => format!("When I navigate to \"{value}\""),
    ("fill", false, false) => format!("When I fill \"{selector}\" with \"{value}\""),
    ("click", false, _) => format!("When I click \"{selector}\""),
    (method, false, true) => format!("When I {method} \"{selector}\""),
    (method, false, false) => format!("When I {method} \"{selector}\" with \"{value}\""),
    (method, true, false) => format!("When I {method} \"{value}\""),
    (method, true, true) => format!("When I {method}"),
  }
}

/// The call's arguments, paired with their parameter names, minus the ones
/// that are part of the receiver rather than the call (`selector`).
///
/// The name is kept because a few arguments are not the plain values they
/// serialize as — see [`locator_arg`].
fn call_args(action: &ActionInfo) -> Vec<(String, serde_json::Value)> {
  match &action.params {
    serde_json::Value::Object(map) => map
      .iter()
      .filter(|(key, _)| key.as_str() != "selector")
      .map(|(key, value)| (key.clone(), value.clone()))
      .collect(),
    serde_json::Value::Null => Vec::new(),
    other => vec![(String::new(), other.clone())],
  }
}

/// The selector behind an argument that is a *locator*, not a string.
///
/// `dragTo` takes a Locator, and its span records the target's selector.
/// Rendering that as a quoted string would produce source that does not
/// compile in either language, so it is rebuilt as a locator expression.
fn locator_arg<'a>(key: &str, value: &'a serde_json::Value) -> Option<&'a str> {
  (key == "target").then(|| value.as_str()).flatten()
}

fn render_js(value: &serde_json::Value, secrets: &Secrets) -> String {
  match value {
    serde_json::Value::String(s) => match secrets.name_for(s) {
      Some(name) => Secrets::env_expression(name, OutputLanguage::TypeScript),
      None => quote_js(&secrets.redact(s)),
    },
    other => other.to_string(),
  }
}

fn render_rust(value: &serde_json::Value, secrets: &Secrets) -> String {
  match value {
    serde_json::Value::String(s) => match secrets.name_for(s) {
      Some(name) => Secrets::env_expression(name, OutputLanguage::Rust),
      None => quote_rust(&secrets.redact(s)),
    },
    other => other.to_string(),
  }
}

fn render_plain(value: &serde_json::Value, secrets: &Secrets) -> String {
  match value {
    serde_json::Value::String(s) => match secrets.name_for(s) {
      Some(name) => Secrets::env_expression(name, OutputLanguage::Gherkin),
      None => secrets.redact(s).into_owned(),
    },
    serde_json::Value::Null => String::new(),
    other => other.to_string(),
  }
}

fn quote_js(s: &str) -> String {
  format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn quote_rust(s: &str) -> String {
  format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `setViewportSize` -> `set_viewport_size`, for the Rust surface.
fn to_snake_case(method: &str) -> String {
  let mut out = String::with_capacity(method.len() + 4);
  for (i, ch) in method.char_indices() {
    if ch.is_ascii_uppercase() {
      if i != 0 {
        out.push('_');
      }
      out.push(ch.to_ascii_lowercase());
    } else {
      out.push(ch);
    }
  }
  out
}

/// An [`ActionObserver`] that renders each finished action as a line of code
/// and hands it to `emit`.
///
/// Lines are produced on `action_end`, so an action appears once it has run —
/// the same order a reader of the generated test needs. Failed actions are
/// emitted too: they are part of what the script did, and hiding them would
/// make the echo a lie.
pub struct CodeEcho<F> {
  language: OutputLanguage,
  secrets: Secrets,
  emit: F,
}

impl<F> CodeEcho<F>
where
  F: Fn(String) + Send + Sync + 'static,
{
  pub fn new(language: OutputLanguage, emit: F) -> Self {
    Self {
      language,
      secrets: Secrets::default(),
      emit,
    }
  }

  #[must_use]
  pub fn with_secrets(mut self, secrets: Secrets) -> Self {
    self.secrets = secrets;
    self
  }
}

impl<F> ActionObserver for CodeEcho<F>
where
  F: Fn(String) + Send + Sync + 'static,
{
  fn action_begin(&self, _action: &ActionInfo) {}

  fn action_end(&self, action: &ActionInfo, _elapsed: std::time::Duration, _error: Option<&str>) {
    if let Some(line) = line_for_with_secrets(action, self.language, &self.secrets) {
      (self.emit)(line);
    }
  }

  fn action_log(&self, _action: &ActionInfo, _message: &str) {}
}

#[cfg(test)]
mod tests {
  use super::*;

  fn action(class: &str, method: &str, params: serde_json::Value) -> ActionInfo {
    ActionInfo {
      call_id: "call@1".into(),
      class: class.into(),
      method: method.into(),
      title: format!("{}.{method}", class.to_ascii_lowercase()),
      params,
    }
  }

  #[test]
  fn page_and_locator_actions_render_playwright_shaped_typescript() {
    let goto = action("Page", "goto", serde_json::json!({ "url": "https://example.com" }));
    assert_eq!(
      line_for(&goto, OutputLanguage::TypeScript).unwrap(),
      "await page.goto('https://example.com');"
    );

    let click = action("Locator", "click", serde_json::json!({ "selector": "button" }));
    assert_eq!(
      line_for(&click, OutputLanguage::TypeScript).unwrap(),
      "await page.locator('button').click();"
    );

    let assertion = action("Expect", "toBeVisible", serde_json::json!({ "selector": "#ok" }));
    assert_eq!(
      line_for(&assertion, OutputLanguage::TypeScript).unwrap(),
      "await expect(page.locator('#ok')).toBeVisible();"
    );
  }

  #[test]
  fn rust_lines_use_the_rust_surface_shape() {
    let click = action("Locator", "click", serde_json::json!({ "selector": "button" }));
    assert_eq!(
      line_for(&click, OutputLanguage::Rust).unwrap(),
      "page.locator(\"button\").click().await?;"
    );

    let viewport = action("Page", "setViewportSize", serde_json::json!({ "width": 800 }));
    assert_eq!(
      line_for(&viewport, OutputLanguage::Rust).unwrap(),
      "page.set_viewport_size(800).await?;"
    );
  }

  #[test]
  fn quotes_inside_values_are_escaped_per_language() {
    let fill = action(
      "Locator",
      "fill",
      serde_json::json!({ "selector": "it's", "value": "a\"b" }),
    );
    assert_eq!(
      line_for(&fill, OutputLanguage::TypeScript).unwrap(),
      "await page.locator('it\\'s').fill('a\"b');"
    );
    assert_eq!(
      line_for(&fill, OutputLanguage::Rust).unwrap(),
      "page.locator(\"it's\").fill(\"a\\\"b\").await?;"
    );
  }

  #[test]
  fn gherkin_uses_the_step_vocabulary() {
    let fill = action(
      "Locator",
      "fill",
      serde_json::json!({ "selector": "#email", "value": "a@b.c" }),
    );
    assert_eq!(
      line_for(&fill, OutputLanguage::Gherkin).unwrap(),
      "When I fill \"#email\" with \"a@b.c\""
    );
  }

  #[test]
  fn a_locator_valued_argument_renders_as_a_locator_not_a_string() {
    let drag = action(
      "Locator",
      "dragTo",
      serde_json::json!({ "selector": "#src", "target": "#dst" }),
    );
    assert_eq!(
      line_for(&drag, OutputLanguage::TypeScript).unwrap(),
      "await page.locator('#src').dragTo(page.locator('#dst'));"
    );
    assert_eq!(
      line_for(&drag, OutputLanguage::Rust).unwrap(),
      "page.locator(\"#src\").drag_to(&page.locator(\"#dst\")).await?;"
    );
  }

  #[test]
  fn an_action_s_own_arguments_reach_the_generated_line() {
    // The whole point of the echo: a `fill` with no value in it generates a
    // file that does not reproduce the run.
    let fill = action(
      "Locator",
      "fill",
      serde_json::json!({ "selector": "#email", "value": "a@b.c" }),
    );
    assert_eq!(
      line_for(&fill, OutputLanguage::TypeScript).unwrap(),
      "await page.locator('#email').fill('a@b.c');"
    );
    let press = action(
      "Locator",
      "press",
      serde_json::json!({ "selector": "#f", "key": "Enter" }),
    );
    assert_eq!(
      line_for(&press, OutputLanguage::TypeScript).unwrap(),
      "await page.locator('#f').press('Enter');"
    );
  }

  #[test]
  fn a_declared_secret_becomes_an_environment_read_in_every_language() {
    let secrets = Secrets::new([("APP_PASSWORD".to_string(), "hunter2".to_string())]);
    let fill = action(
      "Locator",
      "fill",
      serde_json::json!({ "selector": "#password", "value": "hunter2" }),
    );
    assert_eq!(
      line_for_with_secrets(&fill, OutputLanguage::TypeScript, &secrets).unwrap(),
      "await page.locator('#password').fill(process.env['APP_PASSWORD']);"
    );
    assert_eq!(
      line_for_with_secrets(&fill, OutputLanguage::Rust, &secrets).unwrap(),
      "page.locator(\"#password\").fill(&std::env::var(\"APP_PASSWORD\").unwrap_or_default()).await?;"
    );
    assert_eq!(
      line_for_with_secrets(&fill, OutputLanguage::Gherkin, &secrets).unwrap(),
      "When I fill \"#password\" with \"<APP_PASSWORD>\""
    );
  }

  #[test]
  fn a_secret_embedded_in_a_larger_argument_is_redacted_in_place() {
    // Not an exact match, so there is no value to read from the environment;
    // the literal still must not carry the credential.
    let secrets = Secrets::new([("TOK".to_string(), "s3cr3t".to_string())]);
    let goto = action(
      "Page",
      "goto",
      serde_json::json!({ "url": "https://example.com/?token=s3cr3t" }),
    );
    assert_eq!(
      line_for_with_secrets(&goto, OutputLanguage::TypeScript, &secrets).unwrap(),
      "await page.goto('https://example.com/?token=<secret>TOK</secret>');"
    );
  }

  #[test]
  fn an_uncurated_action_still_renders_faithfully() {
    // No emitter knows `page.emulateMedia`, but showing it beats dropping it.
    let emulate = action("Page", "emulateMedia", serde_json::json!({ "colorScheme": "dark" }));
    assert_eq!(
      line_for(&emulate, OutputLanguage::TypeScript).unwrap(),
      "await page.emulateMedia('dark');"
    );
  }
}

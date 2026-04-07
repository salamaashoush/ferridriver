//! Code emitters: convert recorded actions into test code.

use super::Action;

/// Emits test code for a specific language.
pub trait CodeEmitter: Send + Sync {
  /// Opening boilerplate (imports, test function signature, initial navigation).
  fn header(&self, url: &str) -> String;
  /// A single action line.
  fn action(&self, action: &Action) -> String;
  /// Closing boilerplate (closing braces, etc.).
  fn footer(&self) -> String;
}

/// Escape a string for use in a Rust string literal.
fn escape_rust(s: &str) -> String {
  s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape a string for use in a JS/TS string literal (single quotes).
fn escape_js(s: &str) -> String {
  s.replace('\\', "\\\\").replace('\'', "\\'")
}

// ─── Rust ────────────────────────────────────────────────────────────────────

pub struct RustEmitter;

impl CodeEmitter for RustEmitter {
  fn header(&self, url: &str) -> String {
    format!(
      r#"use ferridriver::prelude::*;

#[ferritest]
async fn recorded_test(page: Page) {{
  page.goto("{}").await?;
"#,
      escape_rust(url)
    )
  }

  fn action(&self, action: &Action) -> String {
    match action {
      Action::Navigate { url } => {
        format!("  page.goto(\"{}\").await?;\n", escape_rust(url))
      }
      Action::Click { selector, .. } => {
        format!("  page.locator(\"{}\").click().await?;\n", escape_rust(selector))
      }
      Action::Dblclick { selector, .. } => {
        format!("  page.locator(\"{}\").dblclick().await?;\n", escape_rust(selector))
      }
      Action::Fill { selector, value, .. } => {
        format!(
          "  page.locator(\"{}\").fill(\"{}\").await?;\n",
          escape_rust(selector),
          escape_rust(value)
        )
      }
      Action::Press { selector, key, .. } => {
        format!(
          "  page.locator(\"{}\").press(\"{}\").await?;\n",
          escape_rust(selector),
          escape_rust(key)
        )
      }
      Action::Select { selector, value, .. } => {
        format!(
          "  page.locator(\"{}\").select_option(\"{}\").await?;\n",
          escape_rust(selector),
          escape_rust(value)
        )
      }
      Action::Check { selector, .. } => {
        format!("  page.locator(\"{}\").check().await?;\n", escape_rust(selector))
      }
      Action::Uncheck { selector, .. } => {
        format!("  page.locator(\"{}\").uncheck().await?;\n", escape_rust(selector))
      }
    }
  }

  fn footer(&self) -> String {
    "}\n".into()
  }
}

// ─── TypeScript ──────────────────────────────────────────────────────────────

pub struct TypeScriptEmitter;

impl CodeEmitter for TypeScriptEmitter {
  fn header(&self, url: &str) -> String {
    format!(
      "import {{ test }} from 'ferridriver';\n\ntest('recorded test', async ({{ page }}) => {{\n  await page.goto('{}');\n",
      escape_js(url)
    )
  }

  fn action(&self, action: &Action) -> String {
    match action {
      Action::Navigate { url } => {
        format!("  await page.goto('{}');\n", escape_js(url))
      }
      Action::Click { locator, .. } => {
        format!("  await page.{}.click();\n", locator)
      }
      Action::Dblclick { locator, .. } => {
        format!("  await page.{}.dblclick();\n", locator)
      }
      Action::Fill { locator, value, .. } => {
        format!("  await page.{}.fill('{}');\n", locator, escape_js(value))
      }
      Action::Press { locator, key, .. } => {
        format!("  await page.{}.press('{}');\n", locator, escape_js(key))
      }
      Action::Select { locator, value, .. } => {
        format!("  await page.{}.selectOption('{}');\n", locator, escape_js(value))
      }
      Action::Check { locator, .. } => {
        format!("  await page.{}.check();\n", locator)
      }
      Action::Uncheck { locator, .. } => {
        format!("  await page.{}.uncheck();\n", locator)
      }
    }
  }

  fn footer(&self) -> String {
    "});\n".into()
  }
}

// ─── Gherkin ─────────────────────────────────────────────────────────────────

pub struct GherkinEmitter {
  action_count: std::sync::atomic::AtomicU32,
}

impl GherkinEmitter {
  pub fn new() -> Self {
    Self {
      action_count: std::sync::atomic::AtomicU32::new(0),
    }
  }

  /// First action uses "When", subsequent use "And".
  fn keyword(&self) -> &'static str {
    let n = self.action_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n == 0 { "When" } else { "And" }
  }
}

impl CodeEmitter for GherkinEmitter {
  fn header(&self, url: &str) -> String {
    format!(
      "Feature: Recorded test\n\n  Scenario: User interaction recording\n    Given I navigate to \"{url}\"\n"
    )
  }

  fn action(&self, action: &Action) -> String {
    let kw = self.keyword();
    match action {
      Action::Navigate { url } => {
        format!("    {kw} I navigate to \"{url}\"\n")
      }
      Action::Click { selector, .. } => {
        format!("    {kw} I click \"{selector}\"\n")
      }
      Action::Dblclick { selector, .. } => {
        format!("    {kw} I double click \"{selector}\"\n")
      }
      Action::Fill { selector, value, .. } => {
        format!("    {kw} I fill \"{selector}\" with \"{value}\"\n")
      }
      Action::Press { selector, key, .. } => {
        format!("    {kw} I press \"{key}\" on \"{selector}\"\n")
      }
      Action::Select { selector, value, .. } => {
        format!("    {kw} I select \"{value}\" from \"{selector}\"\n")
      }
      Action::Check { selector, .. } => {
        format!("    {kw} I check \"{selector}\"\n")
      }
      Action::Uncheck { selector, .. } => {
        format!("    {kw} I uncheck \"{selector}\"\n")
      }
    }
  }

  fn footer(&self) -> String {
    String::new()
  }
}

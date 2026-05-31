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
      },
      Action::Click { selector, .. } => {
        format!("  page.locator(\"{}\").click().await?;\n", escape_rust(selector))
      },
      Action::Dblclick { selector, .. } => {
        format!("  page.locator(\"{}\").dblclick().await?;\n", escape_rust(selector))
      },
      Action::Fill { selector, value, .. } => {
        format!(
          "  page.locator(\"{}\").fill(\"{}\").await?;\n",
          escape_rust(selector),
          escape_rust(value)
        )
      },
      Action::Press { selector, key, .. } => {
        format!(
          "  page.locator(\"{}\").press(\"{}\").await?;\n",
          escape_rust(selector),
          escape_rust(key)
        )
      },
      Action::Select { selector, value, .. } => {
        format!(
          "  page.locator(\"{}\").select_option(\"{}\").await?;\n",
          escape_rust(selector),
          escape_rust(value)
        )
      },
      Action::Check { selector, .. } => {
        format!("  page.locator(\"{}\").check().await?;\n", escape_rust(selector))
      },
      Action::Uncheck { selector, .. } => {
        format!("  page.locator(\"{}\").uncheck().await?;\n", escape_rust(selector))
      },
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
    // Adaptive preamble: reuse the live `page` global when present (MCP
    // `run_script` injects it), otherwise launch a browser. The same file
    // therefore runs standalone via `ferridriver run <file>` and against a
    // live session via the MCP `run_script` tool. `typeof page` is safe even
    // when `page` is undeclared, and the `? page :` branch only evaluates
    // when it exists.
    format!(
      "// Recorded with `ferridriver codegen`.\n\
       // Run standalone: `ferridriver run <file>`.\n\
       // Replay on a live session: the MCP `run_script` tool (reuses `page`).\n\
       const __browser = typeof page !== 'undefined' ? null : await chromium().launch();\n\
       const __page = typeof page !== 'undefined' ? page : await __browser.newPage();\n\
       await __page.goto('{}');\n",
      escape_js(url)
    )
  }

  fn action(&self, action: &Action) -> String {
    match action {
      Action::Navigate { url } => {
        format!("await __page.goto('{}');\n", escape_js(url))
      },
      Action::Click { locator, .. } => {
        format!("await __page.{locator}.click();\n")
      },
      Action::Dblclick { locator, .. } => {
        format!("await __page.{locator}.dblclick();\n")
      },
      Action::Fill { locator, value, .. } => {
        format!("await __page.{}.fill('{}');\n", locator, escape_js(value))
      },
      Action::Press { locator, key, .. } => {
        format!("await __page.{}.press('{}');\n", locator, escape_js(key))
      },
      Action::Select { locator, value, .. } => {
        format!("await __page.{}.selectOption('{}');\n", locator, escape_js(value))
      },
      Action::Check { locator, .. } => {
        format!("await __page.{locator}.check();\n")
      },
      Action::Uncheck { locator, .. } => {
        format!("await __page.{locator}.uncheck();\n")
      },
    }
  }

  fn footer(&self) -> String {
    // Close only the browser we launched; an injected session `page` lives on.
    "if (__browser) await __browser.close();\n".into()
  }
}

// ─── Gherkin ─────────────────────────────────────────────────────────────────

pub struct GherkinEmitter {
  action_count: std::sync::atomic::AtomicU32,
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
    format!("Feature: Recorded test\n\n  Scenario: User interaction recording\n    Given I navigate to \"{url}\"\n")
  }

  fn action(&self, action: &Action) -> String {
    let kw = self.keyword();
    match action {
      Action::Navigate { url } => {
        format!("    {kw} I navigate to \"{url}\"\n")
      },
      Action::Click { selector, .. } => {
        format!("    {kw} I click \"{selector}\"\n")
      },
      Action::Dblclick { selector, .. } => {
        format!("    {kw} I double click \"{selector}\"\n")
      },
      Action::Fill { selector, value, .. } => {
        format!("    {kw} I fill \"{selector}\" with \"{value}\"\n")
      },
      Action::Press { selector, key, .. } => {
        format!("    {kw} I press \"{key}\" on \"{selector}\"\n")
      },
      Action::Select { selector, value, .. } => {
        format!("    {kw} I select \"{value}\" from \"{selector}\"\n")
      },
      Action::Check { selector, .. } => {
        format!("    {kw} I check \"{selector}\"\n")
      },
      Action::Uncheck { selector, .. } => {
        format!("    {kw} I uncheck \"{selector}\"\n")
      },
    }
  }

  fn footer(&self) -> String {
    String::new()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn typescript_emits_runnable_adaptive_script() {
    let e = TypeScriptEmitter;

    let header = e.header("https://example.com");
    // Reuses an injected MCP `page`; otherwise launches its own browser.
    assert!(header.contains("typeof page !== 'undefined' ? page : await __browser.newPage()"));
    assert!(header.contains("typeof page !== 'undefined' ? null : await chromium().launch()"));
    assert!(header.contains("await __page.goto('https://example.com');"));
    // No dead test-runner import — it must run under `ferridriver run` / MCP.
    assert!(!header.contains("import { test }"));

    let click = e.action(&Action::Click {
      selector: "internal:role=button".into(),
      locator: "getByRole('button', { name: 'Go' })".into(),
    });
    assert_eq!(click, "await __page.getByRole('button', { name: 'Go' }).click();\n");

    let fill = e.action(&Action::Fill {
      selector: "#email".into(),
      locator: "getByLabel('Email')".into(),
      value: "a@b.com".into(),
    });
    assert_eq!(fill, "await __page.getByLabel('Email').fill('a@b.com');\n");

    assert!(e.footer().contains("if (__browser) await __browser.close();"));
  }
}

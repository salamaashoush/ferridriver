//! Output languages and test/script scaffolds shared across core responses,
//! selector generation, and codegen emitters.

/// Output language for code generation and echoed tool-call source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputLanguage {
  #[default]
  Rust,
  TypeScript,
  Gherkin,
}

/// What the generated code is wrapped in.
///
/// Only TypeScript has a choice: Rust is always a `#[ferritest]` and Gherkin
/// always a feature, both of which are already tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Scaffold {
  /// A script `ferridriver run` executes, standalone or against a session.
  #[default]
  Script,
  /// An `@ferridriver/test` spec `ferridriver test` runs.
  Test,
  /// A plain script `ferridriver test` runs as one test (`[test]
  /// scriptMatch`): the recorded steps alone, against its `page`.
  Plain,
}

impl Scaffold {
  /// Parse `script`, `test` or `plain`.
  ///
  /// # Errors
  ///
  /// Names the accepted values when `s` is none of them.
  pub fn parse(s: &str) -> Result<Self, String> {
    match s {
      "script" => Ok(Self::Script),
      "test" => Ok(Self::Test),
      "plain" => Ok(Self::Plain),
      other => Err(format!(
        "unknown codegen target '{other}': expected 'script', 'test' or 'plain'"
      )),
    }
  }
}

impl OutputLanguage {
  /// The name the injected `asLocator` knows this language by.
  ///
  /// It generates locator EXPRESSIONS and so only speaks the languages
  /// Playwright ships clients for. Rust and Gherkin have no expression
  /// form there, and JavaScript is the closest readable stand-in: a
  /// Rust caller wants `getByRole('button')` to render from, not a raw
  /// selector.
  #[must_use]
  pub fn as_injected_language(self) -> &'static str {
    match self {
      Self::TypeScript | Self::Rust | Self::Gherkin => "javascript",
    }
  }

  /// Parse from a CLI string (e.g. `"typescript"`, `"ts"`, `"gherkin"`).
  /// Unrecognised values default to Rust.
  #[must_use]
  pub fn parse_cli(s: &str) -> Self {
    match s {
      "typescript" | "ts" => Self::TypeScript,
      "gherkin" | "feature" | "bdd" => Self::Gherkin,
      _ => Self::Rust,
    }
  }

  /// Markdown code-fence tag for this language.
  #[must_use]
  pub fn code_fence(self) -> &'static str {
    match self {
      Self::TypeScript => "ts",
      Self::Rust => "rust",
      Self::Gherkin => "gherkin",
    }
  }
}

//! Code generation from what a user or a script did.
//!
//! [`recorder::Recorder`] records a user's interactions on any page as
//! structured [`Action`]s, [`emitter`] renders them as Rust, TypeScript or
//! Gherkin, and [`echo`] renders the actions a script performed.
//!
//! Usage: `ferridriver codegen <url> [--language rust|typescript|gherkin] [--target script|test]`

pub mod echo;
pub mod emitter;
pub mod recorder;

/// A recorded user action.
///
/// The wire shape is the one the page-side recorder sends and the session
/// protocol forwards: `{"type": "click", "selector": ..., "locator": ...}`.
/// `selector` is the raw Playwright selector string (e.g.
/// `internal:role=button[name="Submit"i]`); `locator` is the readable form
/// `asLocator()` renders from it (e.g. `getByRole('button', { name: 'Submit' })`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Action {
  Navigate {
    url: String,
  },
  Click {
    selector: String,
    locator: String,
  },
  Dblclick {
    selector: String,
    locator: String,
  },
  Fill {
    selector: String,
    locator: String,
    value: String,
  },
  Press {
    selector: String,
    locator: String,
    key: String,
  },
  /// The options a `<select>` was left with; one for a single select.
  Select {
    selector: String,
    locator: String,
    values: Vec<String>,
  },
  Check {
    selector: String,
    locator: String,
  },
  Uncheck {
    selector: String,
    locator: String,
  },
  /// The names of the files chosen in an `<input type=file>`. The page only
  /// ever learns the names, so the generated code refers to files of those
  /// names next to the test.
  Upload {
    selector: String,
    locator: String,
    files: Vec<String>,
  },
  AssertVisible {
    selector: String,
    locator: String,
  },
  /// `substring` renders `toContainText`, otherwise `toHaveText`.
  AssertText {
    selector: String,
    locator: String,
    text: String,
    #[serde(default)]
    substring: bool,
  },
  /// An empty `value` renders `toBeEmpty()`.
  AssertValue {
    selector: String,
    locator: String,
    value: String,
  },
  AssertChecked {
    selector: String,
    locator: String,
    checked: bool,
  },
  AssertUrl {
    url: String,
  },
  AssertTitle {
    title: String,
  },
}

impl Action {
  /// Whether this is an assertion rather than an interaction.
  #[must_use]
  pub fn is_assertion(&self) -> bool {
    matches!(
      self,
      Self::AssertVisible { .. }
        | Self::AssertText { .. }
        | Self::AssertValue { .. }
        | Self::AssertChecked { .. }
        | Self::AssertUrl { .. }
        | Self::AssertTitle { .. }
    )
  }
}

/// Output language for code generation.
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
}

impl Scaffold {
  /// Parse `script` or `test`.
  ///
  /// # Errors
  ///
  /// Names the accepted values when `s` is neither.
  pub fn parse(s: &str) -> Result<Self, String> {
    match s {
      "script" => Ok(Self::Script),
      "test" => Ok(Self::Test),
      other => Err(format!("unknown codegen target '{other}': expected 'script' or 'test'")),
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

  /// The emitter that writes this language's script scaffolding around a
  /// body of recorded or echoed action lines.
  #[must_use]
  pub fn emitter(self) -> std::sync::Arc<dyn emitter::CodeEmitter> {
    self.emitter_for(Scaffold::Script)
  }

  /// [`Self::emitter`] for an explicit [`Scaffold`].
  #[must_use]
  pub fn emitter_for(self, scaffold: Scaffold) -> std::sync::Arc<dyn emitter::CodeEmitter> {
    match (self, scaffold) {
      (Self::Rust, _) => std::sync::Arc::new(emitter::RustEmitter),
      (Self::TypeScript, Scaffold::Script) => std::sync::Arc::new(emitter::TypeScriptEmitter),
      (Self::TypeScript, Scaffold::Test) => std::sync::Arc::new(emitter::TypeScriptTestEmitter::default()),
      (Self::Gherkin, _) => std::sync::Arc::new(emitter::GherkinEmitter::new()),
    }
  }
}

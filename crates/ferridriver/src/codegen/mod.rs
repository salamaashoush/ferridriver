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

/// The name generated code gives the page a recording starts on.
pub const FIRST_PAGE: &str = "page";

fn first_page() -> String {
  FIRST_PAGE.to_string()
}

/// A recorded [`Action`] and the page it was done on.
///
/// On the wire the action's own fields sit beside `page` and `popup`:
/// `{"type": "click", "selector": ..., "locator": ..., "page": "page", "popup": "page1"}`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RecordedAction {
  /// The page, by the name the generated code gives it: [`FIRST_PAGE`] for
  /// the one the recording started on, then `page1`, `page2`, ... in the
  /// order the others first took part.
  #[serde(default = "first_page")]
  pub page: String,
  #[serde(flatten)]
  pub action: Action,
  /// The popup this action opened, by its name in the generated code.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub popup: Option<String>,
}

impl RecordedAction {
  /// `action`, done on the page a recording starts on.
  #[must_use]
  pub fn on_first_page(action: Action) -> Self {
    Self {
      page: first_page(),
      action,
      popup: None,
    }
  }
}

impl Action {
  /// The selector and its readable locator, for an action that targets an
  /// element.
  pub fn target_mut(&mut self) -> Option<(&mut String, &mut String)> {
    match self {
      Self::Click { selector, locator }
      | Self::Dblclick { selector, locator }
      | Self::Fill { selector, locator, .. }
      | Self::Press { selector, locator, .. }
      | Self::Select { selector, locator, .. }
      | Self::Check { selector, locator }
      | Self::Uncheck { selector, locator }
      | Self::Upload { selector, locator, .. }
      | Self::AssertVisible { selector, locator }
      | Self::AssertText { selector, locator, .. }
      | Self::AssertValue { selector, locator, .. }
      | Self::AssertChecked { selector, locator, .. } => Some((selector, locator)),
      Self::Navigate { .. } | Self::AssertUrl { .. } | Self::AssertTitle { .. } => None,
    }
  }

  /// The selector of an action that targets an element.
  #[must_use]
  pub fn selector(&self) -> Option<&str> {
    match self {
      Self::Click { selector, .. }
      | Self::Dblclick { selector, .. }
      | Self::Fill { selector, .. }
      | Self::Press { selector, .. }
      | Self::Select { selector, .. }
      | Self::Check { selector, .. }
      | Self::Uncheck { selector, .. }
      | Self::Upload { selector, .. }
      | Self::AssertVisible { selector, .. }
      | Self::AssertText { selector, .. }
      | Self::AssertValue { selector, .. }
      | Self::AssertChecked { selector, .. } => Some(selector),
      Self::Navigate { .. } | Self::AssertUrl { .. } | Self::AssertTitle { .. } => None,
    }
  }

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
  /// A plain script `ferridriver test` runs as one test (`[test]
  /// scriptMatch`): the recorded steps alone, against its `page`.
  Plain,
}

impl Scaffold {
  /// Parse `script`, `test` or `plain`.
  ///
  /// # Errors
  ///
  /// Names the accepted values when `s` is neither.
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
      (Self::TypeScript, Scaffold::Plain) => std::sync::Arc::new(emitter::TypeScriptPlainEmitter),
      (Self::Gherkin, _) => std::sync::Arc::new(emitter::GherkinEmitter::new()),
    }
  }
}

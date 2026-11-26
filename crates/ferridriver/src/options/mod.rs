//! Option structs for the Page and Locator API.

/// A string or a regular expression — the `string | RegExp` union accepted
/// by every `getBy*` matcher, `page.waitForURL`, and similar selector
/// inputs.
///
/// Construction:
/// * `StringOrRegex::from("foo")` — literal string (substring match
///   case-insensitive by default, matched verbatim when `exact: true`).
/// * `StringOrRegex::regex("foo\\d+", "i")` — regex with pattern +
///   `ECMAScript` flags. At the binding boundary NAPI accepts a real
///   JS `RegExp` instance; `QuickJS` similarly reads `source`/`flags`
///   getters off a `RegExp` via prototype-chain access. Wire-shape
///   inputs like `{ regexSource, regexFlags }` are never exposed to
///   the user.
/// * `StringOrRegex::from(regex::Regex::new("^/todos$")?)` — a Rust
///   regex. Locators and web-first assertions evaluate the pattern in the
///   page's JavaScript engine, so the source must also be a valid
///   `ECMAScript` pattern; a leading inline flag group such as `(?i)`
///   becomes the JS flag and `(?P<name>` becomes `(?<name>`.
///
/// The same type serves locators and assertions.
#[derive(Debug, Clone)]
pub enum StringOrRegex {
  String(String),
  Regex { source: String, flags: String },
}

impl StringOrRegex {
  /// Return the literal string if this is a `String` variant. Used by
  /// backends that only accept a literal (no regex support).
  #[must_use]
  pub fn as_str(&self) -> Option<&str> {
    match self {
      Self::String(s) => Some(s),
      Self::Regex { .. } => None,
    }
  }

  /// Convenience — construct the regex variant from source + flags.
  #[must_use]
  pub fn regex(source: impl Into<String>, flags: impl Into<String>) -> Self {
    Self::Regex {
      source: source.into(),
      flags: flags.into(),
    }
  }

  /// The regex variant compiled for Rust-side matching.
  ///
  /// `None` for the string variant.
  ///
  /// # Errors
  ///
  /// [`crate::FerriError::InvalidArgument`] when the pattern uses a
  /// JavaScript-only feature (lookaround, backreferences).
  #[must_use]
  pub fn compile(&self) -> Option<crate::error::Result<regex::Regex>> {
    match self {
      Self::String(_) => None,
      Self::Regex { source, flags } => Some(compile_for_match(source, flags)),
    }
  }

  /// Rust-side match: exact equality for a string, `test` for a regex. A
  /// pattern Rust cannot compile matches nothing.
  #[must_use]
  pub fn matches(&self, actual: &str) -> bool {
    match self {
      Self::String(expected) => actual == expected,
      Self::Regex { .. } => self.compile().is_some_and(|re| re.is_ok_and(|re| re.is_match(actual))),
    }
  }

  /// [`Self::matches`] with Playwright's `ignoreCase` semantics: strings
  /// compare case-folded, regexes are re-evaluated with the `i` flag.
  #[must_use]
  pub fn matches_with(&self, actual: &str, ignore_case: bool) -> bool {
    if !ignore_case {
      return self.matches(actual);
    }
    match self {
      Self::String(expected) => actual.to_lowercase() == expected.to_lowercase(),
      Self::Regex { source, flags } => {
        let flags = if flags.contains('i') {
          flags.clone()
        } else {
          format!("{flags}i")
        };
        compile_for_match(source, &flags).is_ok_and(|re| re.is_match(actual))
      },
    }
  }

  /// Playwright's `ExpectedTextMatcher.matches` with
  /// `normalizeWhiteSpace: true`: a string expectation compares
  /// normalized against normalized, a regex is tested against the raw
  /// text.
  #[must_use]
  pub fn matches_normalized(&self, actual: &str, ignore_case: bool, substring: bool) -> bool {
    match self {
      Self::String(expected) => {
        let (mut exp, mut act) = (normalize_white_space(expected), normalize_white_space(actual));
        if ignore_case {
          exp = exp.to_lowercase();
          act = act.to_lowercase();
        }
        if substring { act.contains(&exp) } else { act == exp }
      },
      Self::Regex { .. } => self.matches_with(actual, ignore_case),
    }
  }

  /// `"text"` for a string, `/source/flags` for a regex.
  #[must_use]
  pub fn description(&self) -> String {
    self.to_string()
  }
}

impl std::fmt::Display for StringOrRegex {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::String(s) => write!(f, "\"{s}\""),
      Self::Regex { source, flags } => write!(f, "/{source}/{flags}"),
    }
  }
}

/// Rust-side compile for matching: the flags that change nothing about
/// whether a string matches (`y`, `d`) are dropped rather than refused.
fn compile_for_match(source: &str, flags: &str) -> crate::error::Result<regex::Regex> {
  let flags: String = flags
    .chars()
    .filter(|c| matches!(c, 'i' | 'm' | 's' | 'g' | 'u'))
    .collect();
  crate::url_matcher::compile_js_regex(source, &flags)
}

/// Playwright's `normalizeWhiteSpace` (`isomorphic/stringUtils.ts`):
/// drop zero-width spaces and soft hyphens, trim, then collapse every
/// whitespace run to a single space.
#[must_use]
pub fn normalize_white_space(text: &str) -> String {
  let stripped: String = text.chars().filter(|c| *c != '\u{200b}' && *c != '\u{ad}').collect();
  stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Split a Rust regex source into an `ECMAScript` source and flags: a
/// leading inline flag group made only of `i`, `m` and `s` becomes the
/// flags, and Python-style named groups become JS named groups.
fn rust_regex_to_js(source: &str) -> (String, String) {
  let mut body = source;
  let mut flags = String::new();
  if let Some(rest) = source.strip_prefix("(?")
    && let Some((group, after)) = rest.split_once(')')
    && !group.is_empty()
    && group.chars().all(|c| matches!(c, 'i' | 'm' | 's'))
  {
    flags = group.to_string();
    body = after;
  }
  (body.replace("(?P<", "(?<"), flags)
}

impl From<regex::Regex> for StringOrRegex {
  fn from(re: regex::Regex) -> Self {
    Self::from(&re)
  }
}

impl From<&regex::Regex> for StringOrRegex {
  fn from(re: &regex::Regex) -> Self {
    let (source, flags) = rust_regex_to_js(re.as_str());
    Self::Regex { source, flags }
  }
}

impl From<&String> for StringOrRegex {
  fn from(s: &String) -> Self {
    Self::String(s.clone())
  }
}

impl From<&str> for StringOrRegex {
  fn from(s: &str) -> Self {
    Self::String(s.to_string())
  }
}

impl From<String> for StringOrRegex {
  fn from(s: String) -> Self {
    Self::String(s)
  }
}

macro_rules! aria_roles {
  ($( $variant:ident => $s:literal ),* $(,)?) => {
    /// ARIA role for `getByRole` — Playwright's `AriaRole` union. String
    /// literals convert via `From<&str>`; unknown strings become
    /// [`Role::Custom`] and flow to the selector engine unchanged (which
    /// reports them like any other unmatched role).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Role {
      $( #[doc = concat!("`", $s, "`")] $variant, )*
      /// A role string outside the WAI-ARIA set.
      Custom(String),
    }

    impl Role {
      #[must_use]
      pub fn as_str(&self) -> &str {
        match self {
          $( Self::$variant => $s, )*
          Self::Custom(s) => s,
        }
      }
    }

    impl From<&str> for Role {
      fn from(s: &str) -> Self {
        match s {
          $( $s => Self::$variant, )*
          other => Self::Custom(other.to_string()),
        }
      }
    }
  };
}

aria_roles! {
  Alert => "alert", AlertDialog => "alertdialog", Application => "application", Article => "article",
  Banner => "banner", Blockquote => "blockquote", Button => "button", Caption => "caption", Cell => "cell",
  Checkbox => "checkbox", Code => "code", ColumnHeader => "columnheader", Combobox => "combobox",
  Complementary => "complementary", ContentInfo => "contentinfo", Definition => "definition",
  Deletion => "deletion", Dialog => "dialog", Directory => "directory", Document => "document",
  Emphasis => "emphasis", Feed => "feed", Figure => "figure", Form => "form", Generic => "generic",
  Grid => "grid", GridCell => "gridcell", Group => "group", Heading => "heading", Img => "img",
  Insertion => "insertion", Link => "link", List => "list", Listbox => "listbox", ListItem => "listitem",
  Log => "log", Main => "main", Marquee => "marquee", Math => "math", Meter => "meter", Menu => "menu",
  Menubar => "menubar", MenuItem => "menuitem", MenuItemCheckbox => "menuitemcheckbox",
  MenuItemRadio => "menuitemradio", Navigation => "navigation", None => "none", Note => "note",
  Option => "option", Paragraph => "paragraph", Presentation => "presentation", ProgressBar => "progressbar",
  Radio => "radio", RadioGroup => "radiogroup", Region => "region", Row => "row", RowGroup => "rowgroup",
  RowHeader => "rowheader", Scrollbar => "scrollbar", Search => "search", SearchBox => "searchbox",
  Separator => "separator", Slider => "slider", SpinButton => "spinbutton", Status => "status",
  Strong => "strong", Subscript => "subscript", Superscript => "superscript", Switch => "switch",
  Tab => "tab", Table => "table", Tablist => "tablist", TabPanel => "tabpanel", Term => "term",
  Textbox => "textbox", Time => "time", Timer => "timer", Toolbar => "toolbar", Tooltip => "tooltip",
  Tree => "tree", TreeGrid => "treegrid", TreeItem => "treeitem",
}

impl From<String> for Role {
  fn from(s: String) -> Self {
    Self::from(s.as_str())
  }
}

impl std::fmt::Display for Role {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.as_str())
  }
}

/// Options for role-based locators (getByRole).
#[derive(Debug, Clone, Default)]
pub struct RoleOptions {
  /// `string | RegExp` — matches the element's accessible name.
  pub name: Option<StringOrRegex>,
  /// `string | RegExp` — matches the element's accessible description
  /// (Playwright 1.60). `exact` applies to both name and description.
  pub description: Option<StringOrRegex>,
  pub exact: Option<bool>,
  pub checked: Option<bool>,
  pub disabled: Option<bool>,
  pub expanded: Option<bool>,
  pub level: Option<i32>,
  pub pressed: Option<bool>,
  pub selected: Option<bool>,
  pub include_hidden: Option<bool>,
}

/// Options for text-based locators (getByText, getByLabel, etc.).
#[derive(Debug, Clone, Default)]
pub struct TextOptions {
  pub exact: Option<bool>,
}

/// Inner-locator reference for [`FilterOptions::has`] / [`FilterOptions::has_not`].
///
/// Accepts either a full [`crate::locator::Locator`] (Rust callers
/// constructing options programmatically) or a raw selector string
/// (NAPI/BDD callers that have already extracted the inner selector). Both
/// variants produce the same encoded `internal:has=` clause —
/// [`crate::locator::Locator`] additionally enables frame-equality checking at filter
/// construction time.
#[derive(Debug, Clone)]
pub enum LocatorLike {
  /// Full locator — preferred form for Rust callers. Enables same-page
  /// checks in [`crate::locator::Locator::filter`].
  Locator(crate::locator::Locator),
  /// Inner selector string verbatim. Used by NAPI/BDD where a full
  /// [`crate::locator::Locator`] cannot be materialized across the binding boundary.
  Selector(String),
}

impl LocatorLike {
  /// The selector string the filter encoder embeds into `internal:has=...`.
  #[must_use]
  pub fn as_selector(&self) -> &str {
    match self {
      Self::Locator(l) => l.selector(),
      Self::Selector(s) => s.as_str(),
    }
  }

  /// Full [`crate::locator::Locator`] if the caller supplied one, for
  /// frame-equality checks. Returns `None` for the `Selector` variant.
  #[must_use]
  pub fn as_locator(&self) -> Option<&crate::locator::Locator> {
    match self {
      Self::Locator(l) => Some(l),
      Self::Selector(_) => None,
    }
  }
}

impl From<crate::locator::Locator> for LocatorLike {
  fn from(l: crate::locator::Locator) -> Self {
    Self::Locator(l)
  }
}

impl From<&crate::locator::Locator> for LocatorLike {
  fn from(l: &crate::locator::Locator) -> Self {
    Self::Locator(l.clone())
  }
}

impl<O> From<crate::locator_builder::LocatorBuilder<O>> for LocatorLike {
  fn from(l: crate::locator_builder::LocatorBuilder<O>) -> Self {
    Self::Locator(l.into_locator())
  }
}

impl<O> From<&crate::locator_builder::LocatorBuilder<O>> for LocatorLike {
  fn from(l: &crate::locator_builder::LocatorBuilder<O>) -> Self {
    Self::Locator((**l).clone())
  }
}

impl From<String> for LocatorLike {
  fn from(s: String) -> Self {
    Self::Selector(s)
  }
}

impl From<&str> for LocatorLike {
  fn from(s: &str) -> Self {
    Self::Selector(s.to_string())
  }
}

impl From<&String> for LocatorLike {
  fn from(s: &String) -> Self {
    Self::Selector(s.clone())
  }
}

/// Script argument shape for [`crate::page::Page::add_init_script`] and
/// [`crate::context::ContextRef::add_init_script`]. Accepts the
/// `Function | string | { path?, content? }` union.
///
/// The binding layer (NAPI / `QuickJS`) is responsible for the engine-local
/// step of extracting a JS function's source via `.toString()`; everything
/// else — reading file paths, JSON-serialising `arg`, composing the
/// `(body)(arg)` wrapper, the "cannot evaluate a string with arguments"
/// invariant — runs here in core.
#[derive(Debug, Clone)]
pub enum InitScriptSource {
  /// Pre-serialised JS function body (the result of `fn.toString()` on
  /// the caller's function). Rendered as `(body)(arg)` by
  /// [`evaluation_script`]; `arg` is JSON-stringified, missing `arg`
  /// renders as the literal `undefined`.
  Function { body: String },
  /// Script source code used verbatim. Passing `arg` alongside this form
  /// errors with "Cannot evaluate a string with arguments".
  Source(String),
  /// Path to an on-disk script file. Read at [`evaluation_script`] call
  /// time; a `//# sourceURL=<path>` comment is appended. Passing `arg`
  /// alongside errors.
  Path(std::path::PathBuf),
  /// Literal script content (from the `{ content }` bag variant).
  /// Semantically equivalent to [`Self::Source`]; kept as a distinct
  /// variant so callers can route the object shape losslessly.
  /// Passing `arg` alongside errors.
  Content(String),
}

impl From<String> for InitScriptSource {
  fn from(s: String) -> Self {
    Self::Source(s)
  }
}

impl From<&str> for InitScriptSource {
  fn from(s: &str) -> Self {
    Self::Source(s.to_string())
  }
}

impl From<std::path::PathBuf> for InitScriptSource {
  fn from(p: std::path::PathBuf) -> Self {
    Self::Path(p)
  }
}

/// Lower an [`InitScriptSource`] + optional JSON argument into the
/// wire-level source string the backend receives.
///
/// Semantics:
/// - [`InitScriptSource::Function`] + `arg` → `(body)(JSON.stringify(arg))`.
///   Absent `arg` renders as `undefined` (matches `Object.is(arg, undefined)`).
///   `null` passes through as `"null"`.
/// - [`InitScriptSource::Source`] / [`InitScriptSource::Content`] →
///   the raw source; `arg.is_some()` rejects with
///   `FerriError::InvalidArgument` ("Cannot evaluate a string with
///   arguments").
/// - [`InitScriptSource::Path`] → file contents followed by
///   `//# sourceURL=<path>` (newlines in the path are stripped so the
///   pragma stays on one line). Same `arg.is_some()` rejection.
///
/// # Errors
///
/// - `arg` is `Some` while `script` is `Source`, `Content`, or `Path`.
/// - `Path` refers to a file that cannot be read.
/// - `Function` + `arg` whose JSON serialisation fails.
pub fn evaluation_script(
  script: InitScriptSource,
  arg: Option<&serde_json::Value>,
) -> Result<String, crate::error::FerriError> {
  match script {
    InitScriptSource::Function { body } => {
      let arg_str = match arg {
        None => "undefined".to_string(),
        Some(v) => serde_json::to_string(v)?,
      };
      Ok(format!("({body})({arg_str})"))
    },
    InitScriptSource::Source(s) | InitScriptSource::Content(s) => {
      if arg.is_some() {
        return Err(crate::error::FerriError::invalid_argument(
          "arg",
          "Cannot evaluate a string with arguments",
        ));
      }
      Ok(s)
    },
    InitScriptSource::Path(p) => {
      if arg.is_some() {
        return Err(crate::error::FerriError::invalid_argument(
          "arg",
          "Cannot evaluate a string with arguments",
        ));
      }
      let source = std::fs::read_to_string(&p)?;
      let safe_path = p.display().to_string().replace('\n', "");
      Ok(format!("{source}\n//# sourceURL={safe_path}"))
    },
  }
}

/// The init-script source for `addInitScript(script, arg, { exposeFunctions:
/// true })`: the argument travels as JavaScript that rebuilds it, each
/// function in it calling back through the bindings controller.
///
/// # Errors
///
/// Returns an error unless `script` is a function, or if `arg` holds a
/// handle.
pub fn evaluation_script_exposing(
  script: InitScriptSource,
  arg: &crate::protocol::SerializedValue,
) -> Result<String, crate::error::FerriError> {
  match script {
    InitScriptSource::Function { body } => Ok(format!("({body})({})", arg.to_js_source()?)),
    InitScriptSource::Source(_) | InitScriptSource::Content(_) | InitScriptSource::Path(_) => {
      Err(crate::error::FerriError::invalid_argument(
        "script",
        "Passing functions requires the init script to be a function",
      ))
    },
  }
}

/// Options for filtering locators — used by both
/// `Locator::filter(options)` and the `Locator` constructor. Every field
/// maps directly to a corresponding injected-selector clause:
///
/// * `has_text` → ` >> internal:has-text=<escaped>`
/// * `has_not_text` → ` >> internal:has-not-text=<escaped>`
/// * `has` → ` >> internal:has=<JSON-encoded inner selector>`
/// * `has_not` → ` >> internal:has-not=<JSON-encoded inner selector>`
/// * `visible` → ` >> visible=true|false`
#[derive(Debug, Clone, Default)]
pub struct FilterOptions {
  /// Playwright: `hasText?: string | RegExp` — string is a
  /// case-insensitive substring match, regex matches element text.
  pub has_text: Option<StringOrRegex>,
  /// Playwright: `hasNotText?: string | RegExp`.
  pub has_not_text: Option<StringOrRegex>,
  pub has: Option<LocatorLike>,
  pub has_not: Option<LocatorLike>,
  /// When `Some(true)`, narrow to visible elements only. When `Some(false)`,
  /// narrow to non-visible elements. `None` means no visibility filter.
  pub visible: Option<bool>,
}

/// Lifecycle milestone for navigation waits — Playwright's
/// `'load' | 'domcontentloaded' | 'networkidle' | 'commit'`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
pub enum LoadState {
  #[default]
  #[serde(rename = "load")]
  Load,
  #[serde(rename = "domcontentloaded")]
  DomContentLoaded,
  #[serde(rename = "networkidle")]
  NetworkIdle,
  #[serde(rename = "commit")]
  Commit,
}

impl LoadState {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Load => "load",
      Self::DomContentLoaded => "domcontentloaded",
      Self::NetworkIdle => "networkidle",
      Self::Commit => "commit",
    }
  }
}

impl std::fmt::Display for LoadState {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.as_str())
  }
}

impl From<&str> for LoadState {
  /// Unknown strings fall back to `Load` — the same default the string
  /// field used before it became typed (bindings forward raw JS input).
  fn from(s: &str) -> Self {
    match s {
      "domcontentloaded" => Self::DomContentLoaded,
      "networkidle" => Self::NetworkIdle,
      "commit" => Self::Commit,
      _ => Self::Load,
    }
  }
}

/// Element state for `waitFor` / `waitForSelector` — Playwright's
/// `'attached' | 'detached' | 'visible' | 'hidden'`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WaitState {
  Attached,
  Detached,
  #[default]
  Visible,
  Hidden,
}

impl WaitState {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Attached => "attached",
      Self::Detached => "detached",
      Self::Visible => "visible",
      Self::Hidden => "hidden",
    }
  }
}

impl std::fmt::Display for WaitState {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.as_str())
  }
}

impl From<&str> for WaitState {
  /// Unknown strings fall back to `Visible` — the wait default.
  fn from(s: &str) -> Self {
    Self::try_from_str(s).unwrap_or_default()
  }
}

impl WaitState {
  /// Strict parse for boundaries that forward raw user strings (JS
  /// bindings) and must keep rejecting unknown states.
  ///
  /// # Errors
  ///
  /// Returns the offending input when it names no known state.
  pub fn try_from_str(s: &str) -> std::result::Result<Self, String> {
    match s {
      "attached" => Ok(Self::Attached),
      "detached" => Ok(Self::Detached),
      "visible" => Ok(Self::Visible),
      "hidden" => Ok(Self::Hidden),
      other => Err(other.to_string()),
    }
  }
}

/// Options for waiting operations.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WaitOptions {
  pub state: Option<WaitState>,
  pub timeout: Option<u64>,
  /// Playwright's `waitForSelector({ strict })`: more than one match is
  /// an error. Unset, a selector takes its first match unless the
  /// context has `strictSelectors`, and a locator is strict.
  pub strict: Option<bool>,
}

/// Playwright: `page.waitForURL(url, options?: { timeout?, waitUntil? })`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WaitForUrlOptions {
  pub timeout: Option<u64>,
  pub wait_until: Option<LoadState>,
}

/// Playwright: `frame.waitForNavigation(options?: { timeout?, url?,
/// waitUntil? })`.
#[derive(Clone, Default)]
pub struct WaitForNavigationOptions {
  pub timeout: Option<u64>,
  /// Only a navigation to a URL this matches ends the wait.
  pub url: Option<crate::url_matcher::UrlMatcher>,
  pub wait_until: Option<LoadState>,
}

/// Playwright: `frame.setContent(html, options?: { timeout?, waitUntil? })`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SetContentOptions {
  pub timeout: Option<u64>,
  pub wait_until: Option<LoadState>,
}

/// Playwright: `frame.addScriptTag(options?: { content?, path?, type?, url? })`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AddScriptTagOptions {
  /// Raw JavaScript for an inline script.
  pub content: Option<String>,
  /// A file whose content becomes an inline script.
  pub path: Option<std::path::PathBuf>,
  /// The script's `type`, `module` for an ES module.
  #[serde(rename = "type")]
  pub script_type: Option<String>,
  /// The `src` of an external script.
  pub url: Option<String>,
}

/// Playwright: `frame.addStyleTag(options?: { content?, path?, url? })`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AddStyleTagOptions {
  /// Raw CSS for an inline stylesheet.
  pub content: Option<String>,
  /// A file whose content becomes an inline stylesheet.
  pub path: Option<std::path::PathBuf>,
  /// The `href` of an external stylesheet.
  pub url: Option<String>,
}

/// Playwright: `page.waitForLoadState(state?, options?: { timeout? })`.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct WaitForLoadStateOptions {
  pub timeout: Option<u64>,
}

/// `LocatorEvaluateOptions` — only the `timeout` field today.
#[derive(Debug, Clone, Default)]
pub struct EvaluateOptions {
  pub timeout: Option<u64>,
}

/// Playwright's `TimeoutOptions`, the bag of `locator.textContent`,
/// `getAttribute`, `isChecked` and the other reads.
#[derive(Debug, Clone, Default)]
pub struct TimeoutOptions {
  /// How long to wait for the element (ms); `0` waits without limit.
  /// `None` is the page's default.
  pub timeout: Option<u64>,
}

/// The bag of `page.textContent(selector, options?)` and the other
/// selector reads: Playwright's `FrameTextContentOptions & TimeoutOptions`.
#[derive(Debug, Clone, Default)]
pub struct SelectorReadOptions {
  /// More than one match is an error. Unset, the context's
  /// `strictSelectors` decides.
  pub strict: Option<bool>,
  pub timeout: Option<u64>,
}

/// Rendering mode for `locator.ariaSnapshot` / `page.ariaSnapshot`.
/// Mirrors Playwright's `mode?: 'ai' | 'default'`
/// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:327`).
/// The vendored injected `AriaTreeOptions` also has `codegen` /
/// `autoexpect`, but those are not part of the public client surface.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AriaSnapshotMode {
  /// Playwright default — stable YAML without volatile refs.
  #[default]
  Default,
  /// AI-optimized — includes `[ref=eN]` labels + generic roles.
  Ai,
}

impl AriaSnapshotMode {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      AriaSnapshotMode::Default => "default",
      AriaSnapshotMode::Ai => "ai",
    }
  }

  /// Parse the public mode string; unknown values fall back to `default`
  /// (Playwright server uses `mode ?? 'default'`).
  #[must_use]
  pub fn from_opt_str(s: Option<&str>) -> Self {
    match s {
      Some("ai") => AriaSnapshotMode::Ai,
      _ => AriaSnapshotMode::Default,
    }
  }
}

/// Options for `page.content(options?)` / `frame.content(options?)`:
/// Playwright's `{ includeShadow?: boolean }`, which serializes open
/// shadow roots as declarative `<template shadowrootmode>` children.
#[derive(Debug, Clone, Default)]
pub struct ContentOptions {
  pub include_shadow: Option<bool>,
}

/// Options for `locator.ariaSnapshot(options?)`. Mirrors Playwright's
/// `TimeoutOptions & { mode?: 'ai' | 'default', depth?: number, boxes?: boolean }`
/// (`/tmp/playwright/packages/playwright-core/src/client/locator.ts:327`).
#[derive(Debug, Clone, Default)]
pub struct AriaSnapshotOptions {
  pub mode: Option<AriaSnapshotMode>,
  /// Subtree depth limit passed to the injected `generateAriaTree`.
  pub depth: Option<i32>,
  /// Append each element's bounding box as `[box=x,y,width,height]`
  /// (Playwright 1.60) — useful for AI consumption.
  pub boxes: Option<bool>,
  /// Actionability/resolution timeout (ms). `None` = page default.
  pub timeout: Option<u64>,
}

/// Full `PageScreenshotOptions` surface — 13 fields. Includes the
/// `LocatorScreenshotOptions` subset for `Locator.screenshot()` (which
/// omits `full_page` and `clip` — the locator takes the screenshot of its
/// own element, not a pixel rectangle).
///
/// Semantics per field:
/// - `animations` — `"disabled"` pauses CSS/Web Animations during capture;
///   `"allow"` (default) leaves them untouched. Finite animations are
///   fast-forwarded to completion (fires `transitionend`); infinite ones
///   revert to initial state.
/// - `caret` — `"hide"` (default) hides the text caret; `"initial"`
///   keeps it visible.
/// - `clip` — pixel rectangle relative to the viewport (not full page).
///   Only meaningful for `Page::screenshot`; ignored by `Locator`.
/// - `full_page` — capture the entire scrollable page. Only meaningful
///   for `Page::screenshot`.
/// - `mask` — locators whose matches are overlaid with `mask_color`
///   before capture. Mirrors Playwright's `mask?: Locator[]`; the
///   selector string is extracted from each locator before backend
///   dispatch.
/// - `mask_color` — CSS color for the mask overlay. Defaults to pink
///   `#FF00FF`.
/// - `omit_background` — transparent background when `true`. Ignored for
///   `jpeg` / `jpg` (no alpha channel).
/// - `path` — if set, the captured bytes are also written to disk.
/// - `quality` — 0–100 for `jpeg` / `webp`. Ignored for `png`.
/// - `scale` — `"css"` for 1 pixel per CSS pixel; `"device"` (default)
///   for 1 pixel per device pixel (Retina captures are 2× bigger).
/// - `style` — raw CSS injected via `addStyleTag` before capture and
///   removed afterwards. Pierces shadow DOM and applies to subframes.
/// - `timeout` — max ms for the capture. `0` = no timeout.
/// - `format` — `"png"` (default), `"jpeg"`, or `"webp"`. Renamed from
///   the JS `type` field because `type` is reserved in Rust. For CDP
///   both `jpeg` and `webp` honour `quality`; `webp` additionally
///   supports transparency.
#[derive(Debug, Clone, Default)]
pub struct ScreenshotOptions {
  pub animations: Option<AnimationsMode>,
  pub caret: Option<CaretMode>,
  pub clip: Option<ClipRect>,
  pub full_page: Option<bool>,
  pub format: Option<ScreenshotFormat>,
  pub mask: Vec<crate::locator::Locator>,
  pub mask_color: Option<String>,
  pub omit_background: Option<bool>,
  pub path: Option<std::path::PathBuf>,
  pub quality: Option<i64>,
  pub scale: Option<ScreenshotScale>,
  pub style: Option<String>,
  pub timeout: Option<u64>,
}

/// Screenshot image format — Playwright's `type?: 'png' | 'jpeg' | 'webp'`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScreenshotFormat {
  #[default]
  Png,
  Jpeg,
  Webp,
}

impl ScreenshotFormat {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Png => "png",
      Self::Jpeg => "jpeg",
      Self::Webp => "webp",
    }
  }
}

impl std::fmt::Display for ScreenshotFormat {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(self.as_str())
  }
}

impl From<&str> for ScreenshotFormat {
  /// `"jpeg"` / `"jpg"` select JPEG, `"webp"` WebP; anything else falls
  /// back to PNG — the same behavior the string field had.
  fn from(s: &str) -> Self {
    match s {
      "jpeg" | "jpg" => Self::Jpeg,
      "webp" => Self::Webp,
      _ => Self::Png,
    }
  }
}

/// CSS-animation handling during a screenshot — Playwright's
/// `animations?: 'disabled' | 'allow'`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AnimationsMode {
  #[default]
  Allow,
  Disabled,
}

impl AnimationsMode {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Allow => "allow",
      Self::Disabled => "disabled",
    }
  }
}

impl From<&str> for AnimationsMode {
  fn from(s: &str) -> Self {
    if s == "disabled" { Self::Disabled } else { Self::Allow }
  }
}

/// Text-caret handling during a screenshot — Playwright's
/// `caret?: 'hide' | 'initial'`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CaretMode {
  #[default]
  Hide,
  Initial,
}

impl CaretMode {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Hide => "hide",
      Self::Initial => "initial",
    }
  }
}

impl From<&str> for CaretMode {
  fn from(s: &str) -> Self {
    if s == "initial" { Self::Initial } else { Self::Hide }
  }
}

/// Screenshot pixel scale — Playwright's `scale?: 'css' | 'device'`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScreenshotScale {
  #[default]
  Device,
  Css,
}

impl ScreenshotScale {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Css => "css",
      Self::Device => "device",
    }
  }
}

impl From<&str> for ScreenshotScale {
  fn from(s: &str) -> Self {
    if s == "css" { Self::Css } else { Self::Device }
  }
}

/// Options for element screenshots: Playwright's
/// `LocatorScreenshotOptions` / `ElementHandleScreenshotOptions`, which
/// are [`ScreenshotOptions`] without `clip` and `fullPage`.
#[derive(Debug, Clone, Default)]
pub struct ElementScreenshotOptions {
  pub animations: Option<AnimationsMode>,
  pub caret: Option<CaretMode>,
  pub format: Option<ScreenshotFormat>,
  pub mask: Vec<crate::locator::Locator>,
  pub mask_color: Option<String>,
  /// Drop the default white backdrop so the capture keeps its
  /// transparency. Playwright's `omitBackground`; ignored for jpeg.
  pub omit_background: Option<bool>,
  pub path: Option<std::path::PathBuf>,
  pub quality: Option<i64>,
  pub scale: Option<ScreenshotScale>,
  pub style: Option<String>,
  pub timeout: Option<u64>,
}

/// Pixel-rectangle clip for [`ScreenshotOptions::clip`]. All values are in
/// CSS pixels relative to the viewport's top-left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipRect {
  pub x: f64,
  pub y: f64,
  pub width: f64,
  pub height: f64,
}

impl From<(f64, f64, f64, f64)> for ClipRect {
  fn from((x, y, width, height): (f64, f64, f64, f64)) -> Self {
    Self { x, y, width, height }
  }
}

/// Element bounding box in viewport coordinates.
#[derive(Debug, Clone, Copy)]
pub struct BoundingBox {
  pub x: f64,
  pub y: f64,
  pub width: f64,
  pub height: f64,
}

/// A 2D point, relative to the top-left corner of an element's padding box
/// (when used as [`DragAndDropOptions::source_position`] or
/// [`DragAndDropOptions::target_position`]), or in viewport coordinates in
/// other contexts. Used by `sourcePosition`, `targetPosition`, and click
/// `position`.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Deserialize)]
pub struct Point {
  pub x: f64,
  pub y: f64,
}

impl From<(f64, f64)> for Point {
  fn from((x, y): (f64, f64)) -> Self {
    Self { x, y }
  }
}

/// Milliseconds accepted by action-builder timeout/delay setters. Converts
/// from a bare `u64` (ms, Playwright convention) or a [`std::time::Duration`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutMs(pub u64);

impl From<u64> for TimeoutMs {
  fn from(ms: u64) -> Self {
    Self(ms)
  }
}

impl From<std::time::Duration> for TimeoutMs {
  fn from(d: std::time::Duration) -> Self {
    Self(u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
  }
}

/// Mouse button for click/dblclick/mousedown/mouseup. The `"left" |
/// "right" | "middle"` union.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MouseButton {
  /// Primary button (default).
  #[default]
  Left,
  /// Context-menu button.
  Right,
  /// Scroll wheel button.
  Middle,
}

impl<'de> serde::Deserialize<'de> for MouseButton {
  fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    let s = String::deserialize(d)?;
    Self::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("Unknown mouse button: {s}")))
  }
}

impl From<&str> for MouseButton {
  /// Unknown strings fall back to `Left` (the default button) — raw JS
  /// input flows through bindings unvalidated, as before.
  fn from(s: &str) -> Self {
    Self::parse(s).unwrap_or_default()
  }
}

impl MouseButton {
  /// CDP `Input.dispatchMouseEvent.button` wire value.
  #[must_use]
  pub fn as_cdp(self) -> &'static str {
    match self {
      Self::Left => "left",
      Self::Right => "right",
      Self::Middle => "middle",
    }
  }

  /// `BiDi` `input.performActions.pointerDown.button` integer: `0=left`,
  /// `1=middle`, `2=right` (per W3C `WebDriver BiDi` pointer input spec).
  #[must_use]
  pub fn as_bidi(self) -> u8 {
    match self {
      Self::Left => 0,
      Self::Middle => 1,
      Self::Right => 2,
    }
  }

  /// Legacy native-`WebKit` mouse-button byte ordering: `0=left,
  /// 1=right, 2=middle` — distinct from the CDP / `BiDi` pointer spec.
  /// The current Playwright `WebKit` backend dispatches mouse events with
  /// CDP-style string buttons (`backend/webkit/input.rs` uses
  /// `MouseButton::as_cdp`), so this numeric accessor is no longer on a
  /// live dispatch path; retained for the documented ordering and its
  /// regression test.
  #[must_use]
  pub fn as_webkit(self) -> u8 {
    match self {
      Self::Left => 0,
      Self::Right => 1,
      Self::Middle => 2,
    }
  }

  /// Parse from string form. Returns `None` on unknown input so callers
  /// can raise a typed `FerriError::InvalidArgument` at the binding
  /// boundary.
  #[must_use]
  pub fn parse(s: &str) -> Option<Self> {
    match s {
      "left" => Some(Self::Left),
      "right" => Some(Self::Right),
      "middle" => Some(Self::Middle),
      _ => None,
    }
  }
}

/// Single keyboard modifier. The `Alt | Control | ControlOrMeta | Meta |
/// Shift` union. `ControlOrMeta` resolves at call time — see
/// [`Self::cdp_bit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
  Alt,
  Control,
  ControlOrMeta,
  Meta,
  Shift,
}

impl<'de> serde::Deserialize<'de> for Modifier {
  fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
    let s = String::deserialize(d)?;
    Self::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("Unknown modifier: {s}")))
  }
}

impl Modifier {
  /// Parse from string form. Returns `None` on unknown input.
  #[must_use]
  pub fn parse(s: &str) -> Option<Self> {
    match s {
      "Alt" => Some(Self::Alt),
      "Control" => Some(Self::Control),
      "ControlOrMeta" => Some(Self::ControlOrMeta),
      "Meta" => Some(Self::Meta),
      "Shift" => Some(Self::Shift),
      _ => None,
    }
  }

  /// CDP `Input.dispatchMouseEvent.modifiers` bitmask bit.
  /// `Alt=1`, `Control=2`, `Meta=4`, `Shift=8` (per CDP docs).
  /// `ControlOrMeta` collapses to `Meta` on macOS, `Control` elsewhere.
  #[must_use]
  pub fn cdp_bit(self) -> u8 {
    match self {
      Self::Alt => 1,
      Self::Control => 2,
      Self::Meta => 4,
      Self::Shift => 8,
      Self::ControlOrMeta => {
        if cfg!(target_os = "macos") {
          4
        } else {
          2
        }
      },
    }
  }
}

/// Fold a list of modifiers into the CDP bitmask expected by
/// `Input.dispatchMouseEvent.modifiers`.
#[must_use]
pub fn modifiers_bitmask(mods: &[Modifier]) -> u32 {
  let mut m = 0u32;
  for md in mods {
    m |= u32::from(md.cdp_bit());
  }
  m
}

/// Full click option bag shared by Page/Locator/Frame click methods.
/// All three expose the same 10-field surface.
///
/// Whether a pointer action scrolls its target into view first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionScroll {
  #[default]
  Auto,
  None,
}

impl ActionScroll {
  /// Playwright's `'auto' | 'none'`.
  #[must_use]
  pub fn parse(value: &str) -> Option<Self> {
    match value {
      "auto" => Some(Self::Auto),
      "none" => Some(Self::None),
      _ => None,
    }
  }
}

mod action;
mod context;
mod launch;
#[cfg(test)]
mod tests;

pub use action::{
  BrowserCloseOptions, CheckOptions, ClickOptions, ContextCloseOptions, DblClickOptions, DispatchEventOptions,
  DragAndDropOptions, DropOptions, DropPayload, EmulateMediaOptions, FilePayload, FillOptions, HoverOptions,
  InputFiles, MediaOverride, PageCloseOptions, PdfMargin, PdfOptions, PdfSize, Polling, PressOptions,
  SelectOptionOptions, SelectOptionValue, SelectOptionValues, SetInputFilesOptions, TapOptions, TypeOptions,
  UnrouteBehavior, ViewportConfig, WaitForFunctionOptions, pdf_paper_format_size,
};
pub use context::{
  BrowserContextOptions, FrameSelector, Geolocation, HighlightStyle, HttpCredentials, HttpCredentialsSend,
  IndexedDbDatabase, IndexedDbIndex, IndexedDbRecord, IndexedDbStore, NameValue, NetworkConditions, OpfsEntry,
  OpfsEntryKind, OriginState, ProxyConfig, RecordHarContent, RecordHarMode, RecordHarOptions, ScreenSize,
  ServiceWorkerPolicy, StorageState, StorageStateInput, StorageStateOptions, ViewportOption, WebStorageKind,
  matching_http_credentials,
};
pub use launch::{
  AutoConnectOptions, BrowserKind, BrowserSelection, BrowserTypeOptions, ChromiumTransport, ConnectOptions,
  ConnectOverCdpOptions, GotoOptions, IgnoreDefaultArgs, InstanceOverrides, LaunchOptions,
  LaunchPersistentContextOptions, LaunchPlan, RecordVideoOptions, VideoSize, construct_url_with_base,
};

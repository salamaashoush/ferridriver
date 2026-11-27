//! Collected test/suite/fixture/hook snapshots exported to `ferridriver-test`.

use ferridriver_test::fixture::FixtureScope;
use ferridriver_test::fixture_graph::FixtureSlot;

use crate::error::ScriptError;

use super::registry::{CollectedAnnotation, CollectedSuiteMode, with_test_registry};
use super::step::fixture_table;

// ── Collection ───────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct CollectedTest {
  pub title: String,
  pub suite: Option<usize>,
  pub annotations: Vec<CollectedAnnotation>,
  pub timeout_ms: Option<u64>,
  pub retries: Option<u32>,
  pub requested: Option<Vec<String>>,
  pub fixture_set: usize,
  pub has_each_arg: bool,
  /// Bundled-output position; remap via the bundle's source map.
  pub line: u32,
  pub col: u32,
}

#[derive(Debug, Clone)]
pub struct CollectedSuite {
  pub name: String,
  pub parent: Option<usize>,
  /// `serial` | `parallel`
  pub mode: Option<String>,
  pub annotations: Vec<CollectedAnnotation>,
  pub use_options: Option<serde_json::Value>,
  pub retries: Option<u32>,
  pub timeout_ms: Option<u64>,
  pub line: u32,
  pub col: u32,
}

/// One `test.skip(callback)`-style suite modifier, as the glue sees it.
/// The callback stays in the VM; this carries the index's metadata so
/// the glue can decide which tests it applies to.
#[derive(Debug, Clone)]
pub struct CollectedModifier {
  /// `skip` | `fixme` | `fail` | `slow`
  pub kind: String,
  /// Declaring suite; `None` = file scope, which is per-FILE and needs
  /// the same source remap a root hook does.
  pub suite: Option<usize>,
  pub description: Option<String>,
  pub requested: Option<Vec<String>>,
  pub line: u32,
  pub col: u32,
}

#[derive(Debug, Clone)]
pub struct CollectedTestHook {
  pub kind: String,
  pub suite: Option<usize>,
  pub requested: Option<Vec<String>>,
  pub line: u32,
  pub col: u32,
}

#[derive(Debug, Clone)]
pub struct CollectedFixture {
  pub name: String,
  pub scope: FixtureScope,
  pub auto: bool,
  /// `[value, { option: true }]` form — overridable via `use` bags.
  pub option: bool,
  pub deps: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CollectedFileAnnotation {
  pub annotation: CollectedAnnotation,
  pub line: u32,
  pub col: u32,
}

#[derive(Debug, Clone)]
pub struct CollectedFileUse {
  pub options: serde_json::Value,
  pub line: u32,
  pub col: u32,
}

#[derive(Debug, Clone)]
pub struct CollectedFileConfigure {
  /// `serial` | `parallel`
  pub mode: Option<String>,
  pub retries: Option<u32>,
  pub timeout_ms: Option<u64>,
  pub line: u32,
  pub col: u32,
}

#[derive(Debug, Clone, Default)]
pub struct CollectedTests {
  pub tests: Vec<CollectedTest>,
  pub suites: Vec<CollectedSuite>,
  pub hooks: Vec<CollectedTestHook>,
  pub modifiers: Vec<CollectedModifier>,
  /// Static annotations declared at FILE scope (`test.skip(true)` with
  /// no enclosing describe). They apply to every test in that file.
  pub file_annotations: Vec<CollectedFileAnnotation>,
  pub fixtures: Vec<CollectedFixture>,
  pub fixture_sets: Vec<Vec<usize>>,
  pub file_use: Vec<CollectedFileUse>,
  pub file_configure: Vec<CollectedFileConfigure>,
  pub has_only: bool,
}

impl CollectedTests {
  /// The `test.extend` chain behind a fixture set, in extend order —
  /// the same input `resolve_custom_fixtures` builds from the live
  /// registry, so the glue's pool-request computation and the VM-side
  /// resolver can never disagree about which registration a name means.
  #[must_use]
  pub fn fixture_slots(&self, fixture_set: usize) -> Vec<FixtureSlot> {
    self
      .fixture_sets
      .get(fixture_set)
      .map(Vec::as_slice)
      .unwrap_or_default()
      .iter()
      .map(|&i| {
        let f = &self.fixtures[i];
        FixtureSlot {
          reg: i,
          name: f.name.clone(),
          deps: f.deps.clone(),
          auto: f.auto,
          scope: f.scope,
          option: f.option,
        }
      })
      .collect()
  }
}

fn mode_str(mode: Option<CollectedSuiteMode>) -> Option<String> {
  mode.map(|m| {
    match m {
      CollectedSuiteMode::Serial => "serial",
      CollectedSuiteMode::Parallel => "parallel",
    }
    .to_string()
  })
}

/// Snapshot the registry after the bundled test module evaluated.
///
/// # Errors
///
/// When the VM has shut down or the test registry is not installed in it.
pub async fn collect_tests(vm: &ferrijs::VmHandle) -> Result<CollectedTests, ScriptError> {
  crate::vm_with!(vm => |ctx| {
    with_test_registry(&ctx, |r| CollectedTests {
      tests: r
        .tests
        .iter()
        .map(|t| CollectedTest {
          title: t.title.clone(),
          suite: t.suite,
          annotations: t.annotations.clone(),
          timeout_ms: t.timeout_ms,
          retries: t.retries,
          requested: t.requested.clone(),
          fixture_set: t.fixture_set,
          has_each_arg: t.each_arg.is_some(),
          line: t.line,
          col: t.col,
        })
        .collect(),
      suites: r
        .suites
        .iter()
        .map(|s| CollectedSuite {
          name: s.name.clone(),
          parent: s.parent,
          mode: mode_str(s.mode),
          annotations: s.annotations.clone(),
          use_options: s.use_options.clone(),
          retries: s.retries,
          timeout_ms: s.timeout_ms,
          line: s.line,
          col: s.col,
        })
        .collect(),
      hooks: r
        .hooks
        .iter()
        .map(|h| CollectedTestHook {
          kind: h.kind.clone(),
          suite: h.suite,
          requested: h.requested.clone(),
          line: h.line,
          col: h.col,
        })
        .collect(),
      modifiers: r
        .modifiers
        .iter()
        .map(|m| CollectedModifier {
          kind: m.kind.clone(),
          suite: m.suite,
          description: m.description.clone(),
          requested: m.requested.clone(),
          line: m.line,
          col: m.col,
        })
        .collect(),
      file_annotations: r
        .file_annotations
        .iter()
        .map(|a| CollectedFileAnnotation {
          annotation: a.annotation.clone(),
          line: a.line,
          col: a.col,
        })
        .collect(),
      fixtures: fixture_table(r),
      fixture_sets: r.fixture_sets.clone(),
      file_use: r
        .file_use
        .iter()
        .map(|u| CollectedFileUse {
          options: u.options.clone(),
          line: u.line,
          col: u.col,
        })
        .collect(),
      file_configure: r
        .file_configure
        .iter()
        .map(|c| CollectedFileConfigure {
          mode: mode_str(c.mode),
          retries: c.retries,
          timeout_ms: c.timeout_ms,
          line: c.line,
          col: c.col,
        })
        .collect(),
      has_only: r.has_only,
    })
  })
  .await?
}

#[cfg(test)]
mod tests {
  use super::super::registry::{
    FixtureBox, FixtureReg, ParsedFixture, TestRegistry, append_fixture, interpolate_title, parse_destructured_keys,
  };
  use super::super::runner::fixture_step_title;
  use ferridriver_test::fixture::FixtureScope;

  fn reg(name: &str, timeout_ms: Option<u64>, title: Option<&str>, boxed: Option<FixtureBox>) -> FixtureReg {
    FixtureReg {
      name: name.to_string(),
      scope: FixtureScope::Test,
      auto: false,
      option: false,
      timeout_ms,
      title: title.map(str::to_string),
      boxed,
      factory: None,
      static_value: None,
      deps: Vec::new(),
    }
  }

  /// Upstream inherits the shadowed registration's whole option bag only
  /// when the override supplies NO bag, and withholds `box` even then
  /// (`common/fixtures.ts:151-153`). An override that DOES supply a bag
  /// is taken as written, so a `{ box: true }` override of a
  /// `{ timeout, title }` fixture keeps neither.
  #[test]
  fn a_fixture_override_inherits_by_upstream_s_asymmetry() {
    let mut r = TestRegistry::default();
    let mut visible: Vec<usize> = Vec::new();
    append_fixture(
      &mut r,
      &mut visible,
      ParsedFixture {
        reg: reg("auth", Some(250), Some("sign in"), Some(FixtureBox::Group)),
        explicit_options: true,
        option_specified: false,
      },
    )
    .expect("base registration");

    // No bag: timeout and title carry, `box` does not.
    let mut bare = visible.clone();
    append_fixture(
      &mut r,
      &mut bare,
      ParsedFixture {
        reg: reg("auth", None, None, None),
        explicit_options: false,
        option_specified: false,
      },
    )
    .expect("bare override");
    let inherited = &r.fixtures[*bare.last().expect("appended")];
    assert_eq!(inherited.timeout_ms, Some(250));
    assert_eq!(inherited.title.as_deref(), Some("sign in"));
    assert_eq!(inherited.boxed, None);

    // A bag of its own: nothing carries, not even the keys it omits.
    let mut bagged = visible.clone();
    append_fixture(
      &mut r,
      &mut bagged,
      ParsedFixture {
        reg: reg("auth", None, None, Some(FixtureBox::Group)),
        explicit_options: true,
        option_specified: false,
      },
    )
    .expect("bagged override");
    let written = &r.fixtures[*bagged.last().expect("appended")];
    assert_eq!(written.timeout_ms, None);
    assert_eq!(written.title, None);
    assert_eq!(written.boxed, Some(FixtureBox::Group));
  }

  #[test]
  fn a_fixture_step_is_titled_by_its_title_then_its_name() {
    // Upstream quotes whichever it used: `Fixture "sign in"`.
    assert_eq!(fixture_step_title(Some("sign in"), "auth"), "Fixture \"sign in\"");
    assert_eq!(fixture_step_title(None, "auth"), "Fixture \"auth\"");
  }

  #[test]
  fn destructured_keys_arrow_and_function_forms() {
    assert_eq!(
      parse_destructured_keys("async ({ page, context }) => {}"),
      Some(vec!["page".to_string(), "context".to_string()])
    );
    assert_eq!(
      parse_destructured_keys("({ page })=>page.title()"),
      Some(vec!["page".to_string()])
    );
    assert_eq!(
      parse_destructured_keys("async function named({ request, testInfo: info }) {}"),
      Some(vec!["request".to_string(), "testInfo".to_string()])
    );
    assert_eq!(
      parse_destructured_keys("({ page = null, browserName }) => {}"),
      Some(vec!["page".to_string(), "browserName".to_string()])
    );
    assert_eq!(parse_destructured_keys("({ ...rest }) => {}"), None);
    assert_eq!(parse_destructured_keys("(page) => {}"), None);
    assert_eq!(parse_destructured_keys("() => {}"), Some(Vec::new()));
    assert_eq!(parse_destructured_keys("async () => {}"), Some(Vec::new()));
  }

  #[test]
  fn destructured_keys_multiline() {
    assert_eq!(
      parse_destructured_keys("async ({\n  page,\n  context,\n}) => {}"),
      Some(vec!["page".to_string(), "context".to_string()])
    );
  }

  #[test]
  fn title_interpolation() {
    let row = serde_json::json!({ "name": "Alice", "n": 3, "nested": { "x": "y" } });
    assert_eq!(interpolate_title("greeting for $name", &row), "greeting for Alice");
    assert_eq!(interpolate_title("case $n of $nested.x", &row), "case 3 of y");
    assert_eq!(interpolate_title("missing $zap stays", &row), "missing $zap stays");
  }
}

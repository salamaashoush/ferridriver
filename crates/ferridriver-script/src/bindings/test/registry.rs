//! Test/suite/fixture/hook registry, location capture, destructured fixture
//! inference, registration parsing, and `test.extend` / `mergeTests` /
//! `defineFixtures`.

use std::cell::RefCell;
use std::sync::Arc;

use rquickjs::function::Rest;
use rquickjs::{Ctx, Function, JsLifetime, Object, Persistent, Value};
use rustc_hash::FxHashMap;

use ferridriver_test::fixture::FixtureScope;
use ferridriver_test::host::TestHostBridge;

use crate::bindings::convert::{serde_from_js, throw_named};
use crate::bindings::registry::{as_function, throw_script_error};
use crate::error::ScriptError;

use super::builders::make_test_object;

/// Prefix of the sentinel error a runtime `test.skip()` throws to abort
/// the body; the `ferridriver-test` worker recognizes it and marks the
/// test Skipped instead of Failed.
pub const TEST_SKIP_SENTINEL: &str = "__FERRIDRIVER_SKIP__:";

/// Suite execution mode requested via `describe.serial` /
/// `describe.parallel` / `describe.configure({ mode })`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectedSuiteMode {
  Serial,
  Parallel,
}

/// One registration-time annotation (`test.skip('t', fn)` forms,
/// `TestDetails` tags/annotations, suite-level `describe.skip`/`fixme`/
/// `only`). Lowered to core `TestAnnotation`s by the glue crate.
#[derive(Debug, Clone)]
pub struct CollectedAnnotation {
  /// `skip` | `fixme` | `fail` | `slow` | `only` | `tag` | `info`
  pub kind: String,
  /// Tag name for `tag`, annotation type for `info`, reason otherwise.
  pub value: Option<String>,
  /// `info` annotation description.
  pub description: Option<String>,
}

pub(crate) struct TestReg {
  pub(crate) title: String,
  pub(crate) suite: Option<usize>,
  pub(crate) func: Persistent<Function<'static>>,
  pub(crate) annotations: Vec<CollectedAnnotation>,
  pub(crate) timeout_ms: Option<u64>,
  pub(crate) retries: Option<u32>,
  /// Destructured fixture names from the body's parameter list. `None`
  /// when the body takes no destructuring pattern (conservative default
  /// applies in the glue).
  pub(crate) requested: Option<Vec<String>>,
  pub(crate) fixture_set: usize,
  /// The `test.each` row for this expansion, passed as the body's
  /// second argument at invocation.
  pub(crate) each_arg: Option<serde_json::Value>,
  /// Bundled-output location of the registration call (remapped to the
  /// original source by the glue via the bundle's source map).
  pub(crate) line: u32,
  pub(crate) col: u32,
}

pub(crate) struct SuiteReg {
  pub(crate) name: String,
  pub(crate) parent: Option<usize>,
  pub(crate) mode: Option<CollectedSuiteMode>,
  pub(crate) annotations: Vec<CollectedAnnotation>,
  pub(crate) use_options: Option<serde_json::Value>,
  pub(crate) retries: Option<u32>,
  pub(crate) timeout_ms: Option<u64>,
  pub(crate) line: u32,
  pub(crate) col: u32,
}

pub(crate) struct TestHookReg {
  /// `beforeAll` | `afterAll` | `beforeEach` | `afterEach`
  pub(crate) kind: String,
  pub(crate) suite: Option<usize>,
  /// The `test.extend` chain of the object the hook was registered on.
  /// A `beforeAll` runs with no test, so it is the only thing that can
  /// tell it which chain's fixtures it may ask for.
  pub(crate) fixture_set: usize,
  pub(crate) func: Persistent<Function<'static>>,
  pub(crate) requested: Option<Vec<String>>,
  pub(crate) line: u32,
  pub(crate) col: u32,
}

/// What `{ box }` asks for on a fixture.
///
/// Distinct from `test.step`'s `box`, which re-attributes an error to
/// the step's own call site. A fixture's decides whether the fixture
/// appears as a step at all, and under what grouping
/// (`playwright/src/worker/fixtureRunner.ts:47-53`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FixtureBox {
  /// `box: 'self'` — the fixture opens no step of its own.
  SelfOnly,
  /// `box: true` — the step exists but is grouped away from the test's
  /// own steps.
  Group,
}

pub(crate) struct FixtureReg {
  pub(crate) name: String,
  pub(crate) scope: FixtureScope,
  pub(crate) auto: bool,
  pub(crate) option: bool,
  /// `{ timeout }` — the setup's own budget, separate from the test's.
  pub(crate) timeout_ms: Option<u64>,
  /// `{ title }` — what the step is called instead of the fixture name.
  pub(crate) title: Option<String>,
  /// `{ box }`. Deliberately NOT inherited when a registration shadows
  /// another (`common/fixtures.ts:152`): an override is visible by
  /// default even when what it overrides was boxed.
  pub(crate) boxed: Option<FixtureBox>,
  pub(crate) factory: Option<Persistent<Function<'static>>>,
  pub(crate) static_value: Option<Persistent<Value<'static>>>,
  /// Destructured dependency names from the factory's first parameter.
  pub(crate) deps: Vec<String>,
}

/// A file-scope `test.use(bag)` — applies to every test in the calling
/// FILE, which is only known after source-map remap, so the raw bundled
/// location travels with the bag.
pub(crate) struct FileUseReg {
  pub(crate) options: serde_json::Value,
  pub(crate) line: u32,
  pub(crate) col: u32,
}

/// A file-scope `describe.configure({...})`.
pub(crate) struct FileConfigureReg {
  pub(crate) mode: Option<CollectedSuiteMode>,
  pub(crate) retries: Option<u32>,
  pub(crate) timeout_ms: Option<u64>,
  pub(crate) line: u32,
  pub(crate) col: u32,
}

/// A test-scoped custom fixture whose factory is suspended inside
/// `await use(value)` — resumed (LIFO) after the body settles.
pub(crate) struct PendingFixture {
  pub(crate) name: String,
  /// Resolver of the gate promise `use()` returned; calling it resumes
  /// the factory for teardown.
  pub(crate) gate_resolve: Option<Persistent<Function<'static>>>,
  /// Signals the factory future settled (teardown done / setup threw).
  pub(crate) done_rx: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
}

/// A worker-scoped custom fixture: value cached for every test in this
/// VM, factory suspended until end-of-run teardown. Keyed by
/// registration index, not name — an override and the super it shadows
/// share a name but are two distinct fixtures with two distinct values.
pub(crate) struct WorkerFixture {
  pub(crate) name: String,
  pub(crate) value: Persistent<Value<'static>>,
  pub(crate) gate_resolve: Option<Persistent<Function<'static>>>,
  pub(crate) done_rx: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
}

/// The test currently executing in this VM (one at a time per worker).
pub(crate) struct CurrentTest {
  pub(crate) world: Persistent<Object<'static>>,
  pub(crate) test_info: Persistent<Object<'static>>,
  pub(crate) bridge: Arc<dyn TestHostBridge>,
  pub(crate) pending: Vec<PendingFixture>,
  /// Lazily initialized on first `testInfo.annotations` access so tests that
  /// never touch `annotations` skip two JS array allocations and teardown fold.
  pub(crate) annotations: Option<AnnotationsMirror>,
}

/// `testInfo.annotations` as Playwright has it: one array the test may
/// push to. It mirrors what the host recorded (declared annotations,
/// `annotate()`, `test.skip()` and the like); entries a body pushed itself
/// are handed to the host when the test ends ([`fold_pushed_annotations`]).
#[derive(Clone)]
pub(crate) struct AnnotationsMirror {
  pub(super) array: Persistent<rquickjs::Array<'static>>,
  /// The entries copied from the host, by identity, so a fold leaves them
  /// out however the body reordered the array.
  pub(super) mirrored: Persistent<rquickjs::Array<'static>>,
}

impl AnnotationsMirror {
  pub(super) fn new(ctx: &Ctx<'_>) -> rquickjs::Result<Self> {
    Ok(Self {
      array: Persistent::save(ctx, rquickjs::Array::new(ctx.clone())?),
      mirrored: Persistent::save(ctx, rquickjs::Array::new(ctx.clone())?),
    })
  }

  /// The array, with whatever the host recorded since the last look
  /// appended.
  pub(super) fn sync<'js>(
    &self,
    ctx: &Ctx<'js>,
    bridge: &dyn TestHostBridge,
  ) -> rquickjs::Result<rquickjs::Array<'js>> {
    let array = self.array.clone().restore(ctx)?;
    let mirrored = self.mirrored.clone().restore(ctx)?;
    for (kind, description) in bridge.annotations().into_iter().skip(mirrored.len()) {
      let entry = Object::new(ctx.clone())?;
      entry.set("type", kind)?;
      if let Some(description) = description {
        entry.set("description", description)?;
      }
      array.set(array.len(), entry.clone())?;
      mirrored.set(mirrored.len(), entry)?;
    }
    Ok(array)
  }
}

/// Hand the host every annotation the running test pushed onto
/// `testInfo.annotations` itself.
pub(super) fn fold_pushed_annotations(ctx: &Ctx<'_>) {
  let Ok(Some((mirror, bridge))) = with_test_registry(ctx, |r| {
    r.current
      .as_ref()
      .and_then(|c| c.annotations.as_ref().map(|a| (a.clone(), Arc::clone(&c.bridge))))
  }) else {
    return;
  };
  let (Ok(array), Ok(mirrored)) = (mirror.sync(ctx, bridge.as_ref()), mirror.mirrored.clone().restore(ctx)) else {
    return;
  };
  let mirrored: Vec<Object<'_>> = mirrored.iter::<Object<'_>>().flatten().collect();
  for entry in array.iter::<Object<'_>>().flatten() {
    if mirrored.contains(&entry) {
      continue;
    }
    let Ok(type_name) = entry.get::<_, String>("type") else {
      continue;
    };
    bridge.annotate(type_name, entry.get::<_, Option<String>>("description").ok().flatten());
  }
}

/// A `test.skip(callback[, description])` registered at file or
/// describe scope — Playwright's `suite._modifiers`
/// (`common/testType.ts:232`). The callback is evaluated with the
/// test's fixtures and its truthy return applies the modifier.
pub(crate) struct ModifierReg {
  /// `skip` | `fixme` | `fail` | `slow`
  pub(crate) kind: String,
  /// The suite it was declared in; `None` = file scope.
  pub(crate) suite: Option<usize>,
  pub(crate) func: Persistent<Function<'static>>,
  pub(crate) description: Option<String>,
  /// Fixture names destructured from the callback's parameter.
  pub(crate) requested: Option<Vec<String>>,
  pub(crate) line: u32,
  pub(crate) col: u32,
}

/// A file-scope static annotation and where it was written.
pub(crate) struct FileAnnotationReg {
  pub(crate) annotation: CollectedAnnotation,
  pub(crate) line: u32,
  pub(crate) col: u32,
}

#[derive(Default)]
pub(crate) struct TestRegistry {
  pub(crate) tests: Vec<TestReg>,
  pub(crate) suites: Vec<SuiteReg>,
  pub(crate) hooks: Vec<TestHookReg>,
  pub(crate) fixtures: Vec<FixtureReg>,
  /// Fixture visibility sets built by `test.extend` chains. Set 0 is
  /// the base `test` (no custom fixtures); each extend appends a new
  /// set = parent set + the new registrations (later same-name entries
  /// shadow earlier ones).
  pub(crate) fixture_sets: Vec<Vec<usize>>,
  pub(crate) modifiers: Vec<ModifierReg>,
  /// `test.skip(true, 'reason')` at FILE scope — Playwright's
  /// `suite._staticAnnotations` on the file suite, which applies to
  /// every test in the file regardless of declaration order. The
  /// describe-scope form lands on [`SuiteReg::annotations`] instead.
  /// Carries its call site because one bundle holds many files, so the
  /// glue has to remap it the way it remaps a root hook.
  pub(crate) file_annotations: Vec<FileAnnotationReg>,
  pub(crate) file_use: Vec<FileUseReg>,
  pub(crate) file_configure: Vec<FileConfigureReg>,
  pub(crate) has_only: bool,
  /// Set once the "registered under a non-test host" diagnostic has been
  /// emitted, so a step file registering fifty tests says it once.
  pub(crate) warned_off_host: bool,
  /// Set once every extension has been installed. `defineFixtures`
  /// appends to the base set in place, and a `test.extend` chain COPIES
  /// the base set at the moment it is built — so a contribution made
  /// after a suite has started deriving chains would reach some of them
  /// and not others. Sealing turns that into a refusal.
  pub(crate) base_sealed: bool,
  /// Suite nesting during registration (indices into `suites`).
  pub(crate) describe_stack: Vec<usize>,
  pub(crate) current: Option<CurrentTest>,
  pub(crate) worker_fixtures: FxHashMap<usize, WorkerFixture>,
  /// Shared prototype carrying `testInfo` getters and methods, built once per
  /// VM so each test only populates its own plain data fields.
  pub(crate) test_info_proto: Option<Persistent<Object<'static>>>,
  /// Cached `QuickJS` representation of `TestInfoData::config`, reused across
  /// tests on the same worker while the config value is unchanged.
  pub(crate) cached_config: Option<(serde_json::Value, Persistent<Value<'static>>)>,
}

impl TestRegistry {
  pub(super) fn new() -> Self {
    Self {
      fixture_sets: vec![Vec::new()],
      ..Self::default()
    }
  }
}

/// Context userdata holding the test registry. Single-threaded VM ⇒
/// `RefCell`, never `Arc`/`Mutex`.
#[derive(JsLifetime)]
pub(super) struct TestRegistryUserData(pub(super) RefCell<TestRegistry>);

pub(crate) fn with_test_registry<R>(ctx: &Ctx<'_>, f: impl FnOnce(&mut TestRegistry) -> R) -> Result<R, ScriptError> {
  let ud = ctx
    .userdata::<TestRegistryUserData>()
    .ok_or_else(|| ScriptError::internal("test registry not installed".to_string()))?;
  let mut reg = ud.0.borrow_mut();
  Ok(f(&mut reg))
}

// ── Location capture ─────────────────────────────────────────────────

/// Bundled-output `line:col` of the innermost JS frame in a fresh stack
/// trace — the user's registration call site. The glue remaps the
/// position to the original `.ts`/`.js` via the bundle's source map.
pub(super) fn capture_location(ctx: &Ctx<'_>) -> (u32, u32) {
  crate::bindings::call_site::capture_frame(ctx).map_or((0, 0), |(_, line, col)| (line, col))
}

// ── Fixture inference ────────────────────────────────────────────────

/// Destructured keys of a function's first parameter, from its source
/// (`Function.prototype.toString`). `None` when the first parameter is
/// not an object pattern (or a rest element makes the set unknowable).
pub(crate) fn destructured_keys<'js>(ctx: &Ctx<'js>, func: &Function<'js>) -> Option<Vec<String>> {
  let src = function_source(ctx, func)?;
  parse_destructured_keys(&src)
}

pub(super) fn function_source<'js>(_ctx: &Ctx<'js>, func: &Function<'js>) -> Option<String> {
  let to_string: Function<'_> = func.as_object()?.get("toString").ok()?;
  to_string
    .call::<_, String>((rquickjs::function::This(func.clone()),))
    .ok()
}

pub(super) fn parse_destructured_keys(src: &str) -> Option<Vec<String>> {
  use std::sync::OnceLock;

  use regex::Regex;
  static EMPTY_RE: OnceLock<Option<Regex>> = OnceLock::new();
  static RE: OnceLock<Option<Regex>> = OnceLock::new();
  if let Some(re) = EMPTY_RE
    .get_or_init(|| Regex::new(r"(?s)^\s*(?:async\s+)?(?:function\b[^(]*)?\(\s*\)").ok())
    .as_ref()
    && re.is_match(src)
  {
    // `() => {}` — provably parameterless, so provably fixtureless.
    return Some(Vec::new());
  }
  let re = RE
    .get_or_init(|| Regex::new(r"(?s)^\s*(?:async\s+)?(?:function\b[^(]*)?\(\s*\{([^}]*)\}").ok())
    .as_ref()?;
  let caps = re.captures(src)?;
  let mut out = Vec::new();
  for raw in caps[1].split(',') {
    let part = raw.trim();
    if part.is_empty() {
      continue;
    }
    if part.starts_with("...") {
      return None;
    }
    let key = part
      .split(':')
      .next()
      .unwrap_or(part)
      .split('=')
      .next()
      .unwrap_or(part)
      .trim();
    if !key.is_empty() {
      out.push(key.to_string());
    }
  }
  Some(out)
}

// ── Registration parsing ─────────────────────────────────────────────

pub(super) struct ParsedDetails {
  pub(super) annotations: Vec<CollectedAnnotation>,
  pub(super) timeout_ms: Option<u64>,
  pub(super) retries: Option<u32>,
}

/// Parse a Playwright `TestDetails` bag: `{ tag?, annotation?, timeout?,
/// retries? }` where `tag` is `string | string[]` and `annotation` is
/// `{type, description?} | Array<...>`.
pub(super) fn parse_details<'js>(ctx: &Ctx<'js>, v: &Value<'js>) -> Result<ParsedDetails, rquickjs::Error> {
  let mut out = ParsedDetails {
    annotations: Vec::new(),
    timeout_ms: None,
    retries: None,
  };
  let Some(o) = v.as_object() else {
    return Ok(out);
  };
  if let Ok(t) = o.get::<_, Value<'_>>("tag")
    && !t.is_undefined()
    && !t.is_null()
  {
    let tags: Vec<String> = if let Some(s) = t.as_string() {
      vec![s.to_string()?]
    } else {
      serde_from_js(ctx, t)
        .map_err(|e| throw_script_error(ctx, &ScriptError::internal(format!("test details `tag`: {e}"))))?
    };
    for tag in tags {
      out.annotations.push(CollectedAnnotation {
        kind: "tag".to_string(),
        value: Some(tag),
        description: None,
      });
    }
  }
  if let Ok(a) = o.get::<_, Value<'_>>("annotation")
    && !a.is_undefined()
    && !a.is_null()
  {
    let list: Vec<serde_json::Value> = if a.is_array() {
      serde_from_js(ctx, a)
        .map_err(|e| throw_script_error(ctx, &ScriptError::internal(format!("test details `annotation`: {e}"))))?
    } else {
      vec![
        serde_from_js(ctx, a)
          .map_err(|e| throw_script_error(ctx, &ScriptError::internal(format!("test details `annotation`: {e}"))))?,
      ]
    };
    for ann in list {
      out.annotations.push(CollectedAnnotation {
        kind: "info".to_string(),
        value: ann.get("type").and_then(|v| v.as_str()).map(str::to_string),
        description: ann.get("description").and_then(|v| v.as_str()).map(str::to_string),
      });
    }
  }
  out.annotations.extend(lock_annotations(ctx, o)?);
  if let Ok(t) = o.get::<_, f64>("timeout") {
    out.timeout_ms = Some(crate::bindings::convert::f64_to_u64(t));
  }
  if let Ok(r) = o.get::<_, f64>("retries") {
    out.retries = Some(crate::bindings::convert::f64_to_u32(r));
  }
  Ok(out)
}

/// The `lock: string | string[]` of a details or `describe.configure`
/// bag, one annotation per name.
pub(super) fn lock_annotations<'js>(
  ctx: &Ctx<'js>,
  bag: &Object<'js>,
) -> Result<Vec<CollectedAnnotation>, rquickjs::Error> {
  let Ok(lock) = bag.get::<_, Value<'_>>("lock") else {
    return Ok(Vec::new());
  };
  if lock.is_undefined() || lock.is_null() {
    return Ok(Vec::new());
  }
  let names: Vec<String> = if let Some(name) = lock.as_string() {
    vec![name.to_string()?]
  } else {
    serde_from_js(ctx, lock)
      .map_err(|e| throw_script_error(ctx, &ScriptError::internal(format!("test details `lock`: {e}"))))?
  };
  Ok(
    names
      .into_iter()
      .map(|name| CollectedAnnotation {
        kind: "lock".to_string(),
        value: Some(name),
        description: None,
      })
      .collect(),
  )
}

pub(super) struct Registration<'js> {
  pub(super) title: String,
  pub(super) details: ParsedDetails,
  pub(super) body: Function<'js>,
}

/// Parse `(title, body)` / `(title, details, body)` — the shared shape
/// of `test(...)` and every registration-form modifier.
pub(super) fn parse_registration<'js>(
  ctx: &Ctx<'js>,
  args: &[Value<'js>],
) -> Result<Registration<'js>, rquickjs::Error> {
  let title = args
    .first()
    .and_then(Value::as_string)
    .ok_or_else(|| throw_script_error(ctx, &ScriptError::internal("test title must be a string".to_string())))?
    .to_string()?;
  let body = args.iter().skip(1).rev().find_map(as_function).ok_or_else(|| {
    throw_script_error(
      ctx,
      &ScriptError::internal(format!("test `{title}` has no function body")),
    )
  })?;
  let details = match args.get(1) {
    Some(v) if v.as_function().is_none() && v.as_object().is_some() => parse_details(ctx, v)?,
    _ => ParsedDetails {
      annotations: Vec::new(),
      timeout_ms: None,
      retries: None,
    },
  };
  Ok(Registration { title, details, body })
}

/// A `test()` registered under a host that never runs tests is a
/// mistake worth naming: the surface exists everywhere (an extension
/// builds fixture chains with it), but only `ferridriver test` collects
/// the registry, so under `mcp` / `bdd` / `script` the registration is
/// inert. Say so once per session rather than silently keeping it.
pub(super) fn warn_off_test_host(ctx: &Ctx<'_>, title: &str) {
  let host = crate::bindings::runtime::ensure_ferridriver(ctx)
    .ok()
    .and_then(|fd| fd.get::<_, String>("host").ok());
  let Some(host) = host else { return };
  if host == "test" {
    return;
  }
  let already = with_test_registry(ctx, |r| std::mem::replace(&mut r.warned_off_host, true)).unwrap_or(true);
  if !already {
    tracing::warn!(
      target: "ferridriver::script",
      host = host.as_str(),
      test = title,
      "test.registration.ignored: test() registered under a host that does not run tests; \
       the registration is kept but nothing will collect it"
    );
  }
}

pub(super) fn push_test<'js>(
  ctx: &Ctx<'js>,
  fixture_set: usize,
  extra: &[CollectedAnnotation],
  reg: Registration<'js>,
  each_arg: Option<serde_json::Value>,
  title_override: Option<String>,
) -> rquickjs::Result<()> {
  warn_off_test_host(ctx, &reg.title);
  if with_test_registry(ctx, |r| r.current.is_some()).map_err(|e| throw_script_error(ctx, &e))? {
    return Err(throw_script_error(
      ctx,
      &ScriptError::internal(format!(
        "test(\"{}\") was called while a test is running; tests are declared at the top level of a test file. \
         A plain script is one test already: name its parts with test.step()",
        reg.title
      )),
    ));
  }
  let (line, col) = capture_location(ctx);
  let requested = destructured_keys(ctx, &reg.body);
  let saved = Persistent::save(ctx, reg.body);
  let mut annotations = extra.to_vec();
  annotations.extend(reg.details.annotations);
  if annotations.iter().any(|a| a.kind == "only") {
    with_test_registry(ctx, |r| r.has_only = true).map_err(|e| throw_script_error(ctx, &e))?;
  }
  with_test_registry(ctx, |r| {
    r.tests.push(TestReg {
      title: title_override.unwrap_or(reg.title),
      suite: r.describe_stack.last().copied(),
      func: saved,
      annotations,
      timeout_ms: reg.details.timeout_ms,
      retries: reg.details.retries,
      requested,
      fixture_set,
      each_arg,
      line,
      col,
    });
  })
  .map_err(|e| throw_script_error(ctx, &e))
}

/// One registration-form modifier annotation (`test.skip('t', fn)`).
pub(super) fn modifier_annotation(kind: &str, reason: Option<String>) -> CollectedAnnotation {
  CollectedAnnotation {
    kind: kind.to_string(),
    value: reason,
    description: None,
  }
}

/// `test.skip`/`test.fixme`/`test.fail`/`test.slow`, in every form
/// Playwright accepts (`common/testType.ts:217-248`). Which one applies
/// is decided the way upstream decides it — by whether a file is still
/// being collected, not by the argument shape alone:
///
/// - `(title, [details,] body)` — registers an annotated test.
/// - collecting, `(callback[, description])` — a SUITE MODIFIER: the
///   callback is kept and evaluated per test with that test's fixtures.
/// - collecting, `([condition[, reason]])` — a static annotation on the
///   enclosing describe, or on the file when there is none. A falsy
///   condition registers nothing at all.
/// - running, `([condition[, reason]])` — the runtime form.
/// - running, `(callback, …)` — refused, with Playwright's message: the
///   fixtures a callback would read are already resolved by then, so
///   accepting it would silently do nothing.
pub(super) fn modifier_call<'js>(
  kind: &'static str,
  fixture_set: usize,
  ctx: &Ctx<'js>,
  args: &[Value<'js>],
) -> rquickjs::Result<()> {
  let is_registration =
    args.first().is_some_and(|v| v.as_string().is_some()) && args.iter().any(|v| v.as_function().is_some());
  if is_registration {
    let reg = parse_registration(ctx, args)?;
    return push_test(ctx, fixture_set, &[modifier_annotation(kind, None)], reg, None, None);
  }

  let collecting = with_test_registry(ctx, |r| r.current.is_none()).map_err(|e| throw_script_error(ctx, &e))?;
  let callback = args.first().and_then(as_function);

  if !collecting {
    if callback.is_some() {
      return Err(throw_script_error(
        ctx,
        &ScriptError::internal(format!(
          "test.{kind}() with a function can only be called inside describe block"
        )),
      ));
    }
    return runtime_modifier(ctx, kind, args);
  }

  let reason = modifier_description(args);

  if let Some(func) = callback {
    let (line, col) = capture_location(ctx);
    let requested = destructured_keys(ctx, &func);
    let saved = Persistent::save(ctx, func);
    return with_test_registry(ctx, |r| {
      r.modifiers.push(ModifierReg {
        kind: kind.to_string(),
        suite: r.describe_stack.last().copied(),
        func: saved,
        description: reason,
        requested,
        line,
        col,
      });
    })
    .map_err(|e| throw_script_error(ctx, &e));
  }

  // Static form. Playwright returns early on an explicitly falsy
  // condition (`testType.ts:234-235`), so `test.skip(false)` leaves no
  // annotation rather than an inert one a reporter would render.
  if args.first().is_some_and(|v| !truthy(v)) {
    return Ok(());
  }
  let annotation = modifier_annotation(kind, reason);
  let (line, col) = capture_location(ctx);
  with_test_registry(ctx, |r| match r.describe_stack.last().copied() {
    Some(idx) => r.suites[idx].annotations.push(annotation),
    None => r.file_annotations.push(FileAnnotationReg { annotation, line, col }),
  })
  .map_err(|e| throw_script_error(ctx, &e))
}

/// A modifier's description: the lone string in
/// `test.{skip,fixme,fail,slow}(description)`, otherwise the argument
/// after the condition or callback (`common/testType.ts::_modifier`).
pub(super) fn modifier_description(args: &[Value<'_>]) -> Option<String> {
  let at = usize::from(!(args.len() == 1 && args[0].as_string().is_some()));
  args
    .get(at)
    .and_then(|v| v.as_string())
    .and_then(|s| s.to_string().ok())
}

/// JS truthiness for a modifier condition. `test.skip(0)` and
/// `test.skip('')` are falsy upstream, so a bare `as_bool` (which
/// answers `None` for anything but a boolean) would turn them into
/// unconditional skips.
pub(super) fn truthy(v: &Value<'_>) -> bool {
  if let Some(b) = v.as_bool() {
    return b;
  }
  if let Some(n) = v.as_number() {
    return n != 0.0 && !n.is_nan();
  }
  if let Some(s) = v.as_string() {
    return s.to_string().map_or(true, |s| !s.is_empty());
  }
  !(v.is_null() || v.is_undefined())
}

/// The running test's bridge, or the Playwright-parity hard error when
/// no test is executing in this VM.
/// The current test's bridge, or `None` outside a test — for callers
/// that have a fallback rather than a requirement.
pub(crate) fn optional_bridge(ctx: &Ctx<'_>) -> Option<Arc<dyn TestHostBridge>> {
  with_test_registry(ctx, |r| r.current.as_ref().map(|c| Arc::clone(&c.bridge)))
    .ok()
    .flatten()
}

pub(crate) fn current_bridge(ctx: &Ctx<'_>, what: &str) -> rquickjs::Result<Arc<dyn TestHostBridge>> {
  with_test_registry(ctx, |r| r.current.as_ref().map(|c| Arc::clone(&c.bridge)))
    .map_err(|e| throw_script_error(ctx, &e))?
    .ok_or_else(|| {
      throw_script_error(
        ctx,
        &ScriptError::internal(format!("{what} can only be called while a test is running")),
      )
    })
}

/// Runtime form of `test.skip`/`fixme`/`fail`/`slow`:
/// `()` unconditional, `(condition[, reason])` conditional. `skip` and
/// `fixme` abort the body by throwing the worker-recognized sentinel;
/// `fail` flips the expected outcome; `slow` triples the timeout.
pub(super) fn runtime_modifier(ctx: &Ctx<'_>, kind: &str, args: &[Value<'_>]) -> rquickjs::Result<()> {
  let bridge = current_bridge(ctx, &format!("test.{kind}()"))?;
  // No argument at all is unconditional; otherwise JS falsiness decides,
  // so `test.skip(0)` is a no-op rather than an unconditional skip.
  let condition = args.first().is_none_or(truthy);
  let reason = modifier_description(args);
  if !condition {
    return Ok(());
  }
  bridge.annotate(kind.to_string(), reason.clone());
  match kind {
    "skip" | "fixme" => {
      bridge.set_skip(reason.clone());
      Err(throw_named(
        ctx,
        "Error",
        format!("{TEST_SKIP_SENTINEL}{}", reason.unwrap_or_default()),
      ))
    },
    "fail" => {
      bridge.set_expected_failure();
      Ok(())
    },
    "slow" => {
      bridge.set_slow();
      Ok(())
    },
    other => Err(throw_script_error(
      ctx,
      &ScriptError::internal(format!("unknown runtime modifier `{other}`")),
    )),
  }
}

/// `$key` title interpolation for `test.each` / `describe.each` rows.
pub(super) fn interpolate_title(template: &str, row: &serde_json::Value) -> String {
  use std::sync::OnceLock;

  use regex::Regex;
  static RE: OnceLock<Option<Regex>> = OnceLock::new();
  let Some(re) = RE
    .get_or_init(|| Regex::new(r"\$([A-Za-z_][A-Za-z0-9_.]*)").ok())
    .as_ref()
  else {
    return template.to_string();
  };
  re.replace_all(template, |caps: &regex::Captures<'_>| {
    let mut cur = row;
    for part in caps[1].split('.') {
      match cur.get(part) {
        Some(v) => cur = v,
        None => return caps[0].to_string(),
      }
    }
    match cur {
      serde_json::Value::String(s) => s.clone(),
      other => other.to_string(),
    }
  })
  .into_owned()
}

// ── test.extend fixture parsing ──────────────────────────────────────

/// A `test.extend` entry as written. `explicit_options` distinguishes
/// the `[value, {…}]` tuple form from the bare-value form: Playwright
/// INHERITS scope/auto/option from the registration being overridden
/// when no options bag is given, and rejects a bag that contradicts it
/// (`common/fixtures.ts::_appendFixtureList`).
pub(super) struct ParsedFixture {
  pub(super) reg: FixtureReg,
  pub(super) explicit_options: bool,
  pub(super) option_specified: bool,
}

pub(super) fn parse_fixture_entry<'js>(
  ctx: &Ctx<'js>,
  name: &str,
  v: Value<'js>,
) -> Result<ParsedFixture, rquickjs::Error> {
  let mut scope = FixtureScope::Test;
  let mut auto = false;
  let mut option = false;
  let mut timeout_ms: Option<u64> = None;
  let mut title: Option<String> = None;
  let mut boxed: Option<FixtureBox> = None;
  let mut option_specified = false;
  let (factory_val, opts): (Value<'js>, Option<Object<'js>>) = if let Some(arr) = v.as_array() {
    let val: Value<'js> = arr.get(0)?;
    let o: Option<Object<'js>> = arr.get::<Value<'js>>(1).ok().and_then(Value::into_object);
    (val, o)
  } else {
    (v, None)
  };
  if let Some(o) = &opts {
    if let Ok(s) = o.get::<_, String>("scope") {
      // `global` is a runner scope with no `test.extend` spelling in
      // Playwright, so a host may only ask for the two it has.
      scope = match FixtureScope::from_label(&s) {
        Some(s @ (FixtureScope::Worker | FixtureScope::Test)) => s,
        _ => {
          return Err(throw_script_error(
            ctx,
            &ScriptError::internal(format!(
              "fixture `{name}`: unknown scope `{s}` (expected \"test\" or \"worker\")"
            )),
          ));
        },
      };
    }
    auto = o.get::<_, bool>("auto").unwrap_or(false);
    option_specified = o
      .get::<_, Value<'js>>("option")
      .is_ok_and(|v| !v.is_undefined() && !v.is_null());
    option = o.get::<_, bool>("option").unwrap_or(false);
    timeout_ms = o
      .get::<_, Option<f64>>("timeout")
      .ok()
      .flatten()
      .map(crate::bindings::convert::f64_to_u64);
    title = o.get::<_, Option<String>>("title").ok().flatten();
    boxed = match o.get::<_, Value<'js>>("box") {
      Ok(v) if v.is_string() => match v.as_string().and_then(|s| s.to_string().ok()).as_deref() {
        Some("self") => Some(FixtureBox::SelfOnly),
        Some(other) => {
          return Err(throw_script_error(
            ctx,
            &ScriptError::internal(format!(
              "fixture `{name}`: unknown box `{other}` (expected true, false or \"self\")"
            )),
          ));
        },
        None => None,
      },
      Ok(v) if v.as_bool() == Some(true) => Some(FixtureBox::Group),
      _ => None,
    };
  }
  let (factory, static_value, deps) = match factory_val.as_function() {
    Some(f) => {
      let deps = destructured_keys(ctx, f).unwrap_or_default();
      (Some(Persistent::save(ctx, f.clone())), None, deps)
    },
    None => (None, Some(Persistent::save(ctx, factory_val)), Vec::new()),
  };
  Ok(ParsedFixture {
    reg: FixtureReg {
      name: name.to_string(),
      scope,
      auto,
      option,
      timeout_ms,
      title,
      boxed,
      factory,
      static_value,
      deps,
    },
    explicit_options: opts.is_some(),
    option_specified,
  })
}

/// Append one `test.extend` entry to a fixture set, applying
/// Playwright's override rules against the registration it shadows.
pub(super) fn append_fixture(
  r: &mut TestRegistry,
  visible: &mut Vec<usize>,
  parsed: ParsedFixture,
) -> Result<(), ScriptError> {
  let ParsedFixture {
    mut reg,
    explicit_options,
    option_specified,
  } = parsed;
  if let Some(&prev) = visible.iter().rev().find(|&&i| r.fixtures[i].name == reg.name) {
    let prev = &r.fixtures[prev];
    if explicit_options {
      if prev.scope != reg.scope {
        return Err(ScriptError::internal(format!(
          "Fixture \"{}\" has already been registered as a {{ scope: '{}' }} fixture.",
          reg.name,
          prev.scope.label()
        )));
      }
      if prev.auto != reg.auto {
        return Err(ScriptError::internal(format!(
          "Fixture \"{}\" has already been registered as a {{ auto: {} }} fixture.",
          reg.name, prev.auto
        )));
      }
      if option_specified && prev.option != reg.option {
        return Err(ScriptError::internal(format!(
          "Fixture \"{}\" has already been registered as a {{ option: {} }} fixture.",
          reg.name, prev.option
        )));
      }
    } else {
      // An override that supplies NO options bag inherits the whole bag
      // from what it shadows -- except `box`, which upstream is explicit
      // about withholding (`common/fixtures.ts:151-153`) so an override
      // is visible by default even when the registration it shadows was
      // boxed away. An override that DOES supply a bag inherits nothing:
      // upstream takes that object as written.
      reg.scope = prev.scope;
      reg.auto = prev.auto;
      reg.option = prev.option;
      reg.timeout_ms = prev.timeout_ms;
      reg.title.clone_from(&prev.title);
    }
  }
  r.fixtures.push(reg);
  visible.push(r.fixtures.len() - 1);
  Ok(())
}

// ── The fixture-set marker ───────────────────────────────────────────

/// Global-symbol key under which every `test` object records the fixture
/// set it is bound to. Playwright marks its test objects the same way
/// (`testTypeSymbol`, `common/testType.ts`), and for the same reasons:
/// `mergeTests` needs the chains behind its arguments, `test.extend`
/// needs to recognise a test object passed where fixtures belong, and a
/// symbol cannot collide with a suite's own properties.
pub(super) const TEST_TYPE_SYMBOL: &str = "ferridriver.testType";

pub(super) fn test_type_key<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Atom<'js>> {
  Ok(rquickjs::Symbol::new_global(ctx.clone(), TEST_TYPE_SYMBOL)?.as_atom())
}

/// The fixture set `value`'s `test` object is bound to, or `None` when
/// the value is not one of ours.
pub(crate) fn fixture_set_of<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Option<usize> {
  let obj = value.as_object()?;
  let marked: Value<'js> = obj.get(test_type_key(ctx).ok()?).ok()?;
  let index = marked.as_number()?;
  (index >= 0.0).then(|| crate::bindings::convert::f64_to_usize(index))
}

/// `mergeTests(...tests)` — one `test` whose fixtures are the union of
/// every argument's chain, with the fixtures they share registered once.
///
/// Playwright filters by the identity of the fixtures object each
/// `extend` was called with (`common/testType.ts:338`); a fixture set
/// here is a list of registration indices, so the same filter is index
/// identity. Dedup by NAME would be wrong in the same way it is
/// upstream: a base fixture appearing in both chains would become an
/// override of itself and resolve its own `super`.
pub(super) fn merge_tests<'js>(
  #[expect(
    clippy::needless_pass_by_value,
    reason = "rquickjs hands a native function its `Ctx` by value"
  )]
  ctx: Ctx<'js>,
  #[expect(
    clippy::needless_pass_by_value,
    reason = "rquickjs extracts a native function's JS arguments by value"
  )]
  tests: Rest<Value<'js>>,
) -> rquickjs::Result<Function<'js>> {
  let mut merged: Vec<usize> = Vec::new();
  for test in &tests.0 {
    let Some(set) = fixture_set_of(&ctx, test) else {
      return Err(throw_script_error(&ctx, &ScriptError::internal(
        "mergeTests() accepts \"test\" functions as parameters.\nDid you mean to call test.extend() with fixtures instead?"
          .to_string(),
      )));
    };
    let visible = with_test_registry(&ctx, |r| r.fixture_sets.get(set).cloned().unwrap_or_default())
      .map_err(|e| throw_script_error(&ctx, &e))?;
    for index in visible {
      if !merged.contains(&index) {
        merged.push(index);
      }
    }
  }
  let new_set = with_test_registry(&ctx, |r| {
    r.fixture_sets.push(merged);
    r.fixture_sets.len() - 1
  })
  .map_err(|e| throw_script_error(&ctx, &e))?;
  make_test_object(&ctx, new_set)
}

/// The base fixture chain: the set `ferridriver.test`, `_baseTest` and
/// every ambient `Given`/`When`/`Then` resolve from.
pub(super) const BASE_FIXTURE_SET: usize = 0;

/// `defineFixtures(fixtures)` — an extension package contributing onto
/// the BASE chain, so a suite that never imports the package still
/// receives the fixtures through its own `test`.
///
/// It appends to set 0 IN PLACE instead of building a new chain and
/// rebinding `ferridriver.test`. Rebinding cannot work: `@ferridriver/
/// test` is one module instance per VM and its export slots hold the
/// values copied when it evaluated, so an importer keeps the object it
/// linked against however the source property is reassigned afterwards
/// (pinned by `tests/extraction_context.rs`).
///
/// Packages compose in load order under `test.extend`'s own override
/// rules: a later same-name entry shadows the earlier one and resolves
/// it as its `super`. A chain a package already derived with
/// `test.extend` keeps the base it copied — which is why the base seals
/// once the last extension has been installed.
pub(super) fn define_fixtures<'js>(
  #[expect(
    clippy::needless_pass_by_value,
    reason = "rquickjs hands a native function its `Ctx` by value"
  )]
  ctx: Ctx<'js>,
  #[expect(
    clippy::needless_pass_by_value,
    reason = "rquickjs extracts a native function's JS arguments by value"
  )]
  fixtures: Object<'js>,
) -> rquickjs::Result<Value<'js>> {
  let permitted = ctx
    .userdata::<crate::bindings::registry::ExtensionPolicyUd>()
    .is_none_or(|p| p.0.fixtures);
  if !permitted {
    return Err(throw_script_error(
      &ctx,
      &crate::error::policy_error(
        "defineFixtures() is refused by the operator policy (`[extensions.policy] fixtures = false`), \
         which forbids packages from contributing fixtures onto the base `test` chain",
      ),
    ));
  }
  if with_test_registry(&ctx, |r| r.base_sealed).map_err(|e| throw_script_error(&ctx, &e))? {
    return Err(throw_script_error(
      &ctx,
      &ScriptError::internal(
        "defineFixtures() can only be called while an extension is loading. The base `test` chain \
         is sealed once every extension has been installed, because a `test.extend()` chain copies \
         the base chain at the moment it is built — a contribution made now would reach some \
         suites and not others. Use `test.extend()` to build a chain of your own instead.",
      ),
    ));
  }
  if fixture_set_of(&ctx, fixtures.as_value()).is_some() {
    return Err(throw_script_error(
      &ctx,
      &ScriptError::internal(
        "defineFixtures() accepts a fixtures object, not a test object.\nDid you mean to call \
         test.extend() with fixtures instead?",
      ),
    ));
  }

  let mut parsed = Vec::new();
  for key in fixtures.keys::<String>() {
    let key = key?;
    let v: Value<'js> = fixtures.get(key.as_str())?;
    parsed.push(parse_fixture_entry(&ctx, &key, v)?);
  }
  with_test_registry(&ctx, |r| {
    let mut visible = r.fixture_sets.get(BASE_FIXTURE_SET).cloned().unwrap_or_default();
    for entry in parsed {
      append_fixture(r, &mut visible, entry)?;
    }
    if let Some(base) = r.fixture_sets.get_mut(BASE_FIXTURE_SET) {
      *base = visible;
    }
    Ok(())
  })
  .map_err(|e| throw_script_error(&ctx, &e))?
  .map_err(|e: ScriptError| throw_script_error(&ctx, &e))?;

  crate::bindings::runtime::ensure_ferridriver(&ctx)?.get::<_, Value<'js>>("test")
}

/// Seal the base fixture chain — every extension has been installed, so
/// nothing may append to set 0 again.
///
/// # Errors
///
/// When the test surface is not installed in this context.
pub fn seal_base_fixtures(ctx: &Ctx<'_>) -> Result<(), ScriptError> {
  with_test_registry(ctx, |r| r.base_sealed = true)
}

/// Whether the extension-loading window has closed. `false` when the
/// test surface is not installed at all, which is the state the
/// extraction pass runs a non-Test host in.
///
/// The absent-surface case answers `false`.
#[must_use]
pub fn base_is_sealed(ctx: &Ctx<'_>) -> bool {
  with_test_registry(ctx, |r| r.base_sealed).unwrap_or(false)
}

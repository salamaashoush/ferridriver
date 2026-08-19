//! Playwright-shaped test registration surface for the shared QuickJS
//! engine (`import { test, describe, expect } from '@ferridriver/test'`).
//!
//! `test`/`describe` and every modifier (`skip`/`fixme`/`fail`/`slow`/
//! `only`/`each`/`use`/`extend`/hooks) are native Rust functions — no JS
//! glue. Registrations land in a Rust `TestRegistry` held as context
//! userdata (single-threaded VM, `RefCell`). Test bodies are kept as
//! `Persistent` functions; the test-runner glue crate snapshots the
//! registry after the bundled test module evaluates ([`collect_tests`])
//! and invokes bodies by registration index — the same architecture the
//! BDD step surface uses ([`super::bdd`]).
//!
//! No business logic here: filtering, retries, worker scheduling and
//! reporting all stay in the `ferridriver-test` core.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use rquickjs::function::Rest;
use rquickjs::prelude::Async;
use rquickjs::promise::MaybePromise;
use rquickjs::{CatchResultExt, Ctx, Function, JsLifetime, Object, Persistent, Promise, Value};
use rustc_hash::FxHashMap;

use ferridriver_test::fixture::FixtureScope;
use ferridriver_test::fixture_graph::{self, FixtureSlot};
use ferridriver_test::host::{RunTestSpec, TestHostBridge, TestInfoData, TestWorldData};
use ferridriver_test::model::StepLocation;
use ferridriver_test::step::{StepBodyError, StepExpectation, StepFrame, StepOptions, StepSpec};

use crate::bindings::call_site;
use crate::bindings::convert::{ferri_throw, serde_from_js};
use crate::bindings::registry::{as_function, rq, throw_script_error};
use crate::bindings::{install_browser_context_on, install_browser_on, install_page_on, install_request_on};
use crate::engine::caught_to_script_error;
use crate::error::ScriptError;

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
}

impl TestRegistry {
  fn new() -> Self {
    Self {
      fixture_sets: vec![Vec::new()],
      ..Self::default()
    }
  }
}

/// Context userdata holding the test registry. Single-threaded VM ⇒
/// `RefCell`, never `Arc`/`Mutex`.
struct TestRegistryUserData(RefCell<TestRegistry>);

// SAFETY: holds only `'static` data (`Persistent<…>` handles and owned
// values), so re-stating the unused `'js` lifetime is sound — same
// rationale as `RegistryUserData`.
#[allow(unsafe_code)]
unsafe impl JsLifetime<'_> for TestRegistryUserData {
  type Changed<'to> = TestRegistryUserData;
}

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
fn capture_location(ctx: &Ctx<'_>) -> (u32, u32) {
  super::call_site::capture_frame(ctx).map_or((0, 0), |(_, line, col)| (line, col))
}

// ── Fixture inference ────────────────────────────────────────────────

/// Destructured keys of a function's first parameter, from its source
/// (`Function.prototype.toString`). `None` when the first parameter is
/// not an object pattern (or a rest element makes the set unknowable).
pub(crate) fn destructured_keys<'js>(ctx: &Ctx<'js>, func: &Function<'js>) -> Option<Vec<String>> {
  let src = function_source(ctx, func)?;
  parse_destructured_keys(&src)
}

fn function_source<'js>(_ctx: &Ctx<'js>, func: &Function<'js>) -> Option<String> {
  let to_string: Function<'_> = func.as_object()?.get("toString").ok()?;
  to_string
    .call::<_, String>((rquickjs::function::This(func.clone()),))
    .ok()
}

fn parse_destructured_keys(src: &str) -> Option<Vec<String>> {
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

struct ParsedDetails {
  annotations: Vec<CollectedAnnotation>,
  timeout_ms: Option<u64>,
  retries: Option<u32>,
}

/// Parse a Playwright `TestDetails` bag: `{ tag?, annotation?, timeout?,
/// retries? }` where `tag` is `string | string[]` and `annotation` is
/// `{type, description?} | Array<...>`.
fn parse_details<'js>(ctx: &Ctx<'js>, v: &Value<'js>) -> Result<ParsedDetails, rquickjs::Error> {
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
      serde_from_js(ctx, t).map_err(|e| rq(&ScriptError::internal(format!("test details `tag`: {e}"))))?
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
      serde_from_js(ctx, a).map_err(|e| rq(&ScriptError::internal(format!("test details `annotation`: {e}"))))?
    } else {
      vec![serde_from_js(ctx, a).map_err(|e| rq(&ScriptError::internal(format!("test details `annotation`: {e}"))))?]
    };
    for ann in list {
      out.annotations.push(CollectedAnnotation {
        kind: "info".to_string(),
        value: ann.get("type").and_then(|v| v.as_str()).map(str::to_string),
        description: ann.get("description").and_then(|v| v.as_str()).map(str::to_string),
      });
    }
  }
  if let Ok(t) = o.get::<_, f64>("timeout") {
    out.timeout_ms = Some(t.max(0.0) as u64);
  }
  if let Ok(r) = o.get::<_, f64>("retries") {
    out.retries = Some(r.max(0.0) as u32);
  }
  Ok(out)
}

struct Registration<'js> {
  title: String,
  details: ParsedDetails,
  body: Function<'js>,
}

/// Parse `(title, body)` / `(title, details, body)` — the shared shape
/// of `test(...)` and every registration-form modifier.
fn parse_registration<'js>(ctx: &Ctx<'js>, args: &[Value<'js>]) -> Result<Registration<'js>, rquickjs::Error> {
  let title = args
    .first()
    .and_then(Value::as_string)
    .ok_or_else(|| rq(&ScriptError::internal("test title must be a string".to_string())))?
    .to_string()?;
  let body = args
    .iter()
    .skip(1)
    .rev()
    .find_map(as_function)
    .ok_or_else(|| rq(&ScriptError::internal(format!("test `{title}` has no function body"))))?;
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
fn warn_off_test_host(ctx: &Ctx<'_>, title: &str) {
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

fn push_test<'js>(
  ctx: &Ctx<'js>,
  fixture_set: usize,
  extra: &[CollectedAnnotation],
  reg: Registration<'js>,
  each_arg: Option<serde_json::Value>,
  title_override: Option<String>,
) -> rquickjs::Result<()> {
  warn_off_test_host(ctx, &reg.title);
  let (line, col) = capture_location(ctx);
  let requested = destructured_keys(ctx, &reg.body);
  let saved = Persistent::save(ctx, reg.body);
  let mut annotations = extra.to_vec();
  annotations.extend(reg.details.annotations);
  if annotations.iter().any(|a| a.kind == "only") {
    with_test_registry(ctx, |r| r.has_only = true).map_err(|e| rq(&e))?;
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
  .map_err(|e| rq(&e))
}

/// One registration-form modifier annotation (`test.skip('t', fn)`).
fn modifier_annotation(kind: &str, reason: Option<String>) -> CollectedAnnotation {
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
fn modifier_call<'js>(
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

  let collecting = with_test_registry(ctx, |r| r.current.is_none()).map_err(|e| rq(&e))?;
  let callback = args.first().and_then(as_function);

  if !collecting {
    if callback.is_some() {
      return Err(rq(&ScriptError::internal(format!(
        "test.{kind}() with a function can only be called inside describe block"
      ))));
    }
    return runtime_modifier(ctx, kind, args);
  }

  let reason = args
    .iter()
    .skip(1)
    .find_map(|v| v.as_string().and_then(|s| s.to_string().ok()));

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
    .map_err(|e| rq(&e));
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
  .map_err(|e| rq(&e))
}

/// JS truthiness for a modifier condition. `test.skip(0)` and
/// `test.skip('')` are falsy upstream, so a bare `as_bool` (which
/// answers `None` for anything but a boolean) would turn them into
/// unconditional skips.
fn truthy(v: &Value<'_>) -> bool {
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
    .map_err(|e| rq(&e))?
    .ok_or_else(|| {
      rq(&ScriptError::internal(format!(
        "{what} can only be called while a test is running"
      )))
    })
}

/// Runtime form of `test.skip`/`fixme`/`fail`/`slow`:
/// `()` unconditional, `(condition[, reason])` conditional. `skip` and
/// `fixme` abort the body by throwing the worker-recognized sentinel;
/// `fail` flips the expected outcome; `slow` triples the timeout.
fn runtime_modifier(ctx: &Ctx<'_>, kind: &str, args: &[Value<'_>]) -> rquickjs::Result<()> {
  let bridge = current_bridge(ctx, &format!("test.{kind}()"))?;
  // No argument at all is unconditional; otherwise JS falsiness decides,
  // so `test.skip(0)` is a no-op rather than an unconditional skip.
  let condition = args.first().is_none_or(truthy);
  let reason = args.iter().find_map(|v| v.as_string().and_then(|s| s.to_string().ok()));
  if !condition {
    return Ok(());
  }
  match kind {
    "skip" | "fixme" => {
      bridge.set_skip(reason.clone());
      Err(rquickjs::Error::new_from_js_message(
        "test",
        "skip",
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
    other => Err(rq(&ScriptError::internal(format!(
      "unknown runtime modifier `{other}`"
    )))),
  }
}

/// `$key` title interpolation for `test.each` / `describe.each` rows.
fn interpolate_title(template: &str, row: &serde_json::Value) -> String {
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
struct ParsedFixture {
  reg: FixtureReg,
  explicit_options: bool,
  option_specified: bool,
}

fn parse_fixture_entry<'js>(ctx: &Ctx<'js>, name: &str, v: Value<'js>) -> Result<ParsedFixture, rquickjs::Error> {
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
          return Err(rq(&ScriptError::internal(format!(
            "fixture `{name}`: unknown scope `{s}` (expected \"test\" or \"worker\")"
          ))));
        },
      };
    }
    auto = o.get::<_, bool>("auto").unwrap_or(false);
    option_specified = o
      .get::<_, Value<'js>>("option")
      .is_ok_and(|v| !v.is_undefined() && !v.is_null());
    option = o.get::<_, bool>("option").unwrap_or(false);
    timeout_ms = o.get::<_, Option<f64>>("timeout").ok().flatten().map(|ms| ms as u64);
    title = o.get::<_, Option<String>>("title").ok().flatten();
    boxed = match o.get::<_, Value<'js>>("box") {
      Ok(v) if v.is_string() => match v.as_string().and_then(|s| s.to_string().ok()).as_deref() {
        Some("self") => Some(FixtureBox::SelfOnly),
        Some(other) => {
          return Err(rq(&ScriptError::internal(format!(
            "fixture `{name}`: unknown box `{other}` (expected true, false or \"self\")"
          ))));
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
fn append_fixture(r: &mut TestRegistry, visible: &mut Vec<usize>, parsed: ParsedFixture) -> Result<(), ScriptError> {
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
      reg.scope = prev.scope;
      reg.auto = prev.auto;
      reg.option = prev.option;
    }
    // `timeout` and `title` carry across an override; `box` does NOT.
    // Upstream is explicit about the asymmetry
    // (`common/fixtures.ts:152`): an override should be visible by
    // default even when the registration it shadows was boxed away.
    if reg.timeout_ms.is_none() {
      reg.timeout_ms = prev.timeout_ms;
    }
    if reg.title.is_none() {
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
const TEST_TYPE_SYMBOL: &str = "ferridriver.testType";

fn test_type_key<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<rquickjs::Atom<'js>> {
  Ok(rquickjs::Symbol::new_global(ctx.clone(), TEST_TYPE_SYMBOL)?.as_atom())
}

/// The fixture set `value`'s `test` object is bound to, or `None` when
/// the value is not one of ours.
pub(crate) fn fixture_set_of<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Option<usize> {
  let obj = value.as_object()?;
  let marked: Value<'js> = obj.get(test_type_key(ctx).ok()?).ok()?;
  let index = marked.as_number()?;
  (index >= 0.0).then_some(index as usize)
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
fn merge_tests<'js>(ctx: Ctx<'js>, tests: Rest<Value<'js>>) -> rquickjs::Result<Function<'js>> {
  let mut merged: Vec<usize> = Vec::new();
  for test in &tests.0 {
    let Some(set) = fixture_set_of(&ctx, test) else {
      return Err(rq(&ScriptError::internal(
        "mergeTests() accepts \"test\" functions as parameters.\nDid you mean to call test.extend() with fixtures instead?"
          .to_string(),
      )));
    };
    let visible =
      with_test_registry(&ctx, |r| r.fixture_sets.get(set).cloned().unwrap_or_default()).map_err(|e| rq(&e))?;
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
  .map_err(|e| rq(&e))?;
  make_test_object(&ctx, new_set)
}

/// The base fixture chain: the set `ferridriver.test`, `_baseTest` and
/// every ambient `Given`/`When`/`Then` resolve from.
const BASE_FIXTURE_SET: usize = 0;

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
fn define_fixtures<'js>(ctx: Ctx<'js>, fixtures: Object<'js>) -> rquickjs::Result<Value<'js>> {
  let permitted = ctx
    .userdata::<crate::bindings::registry::ExtensionPolicyUd>()
    .is_none_or(|p| p.0.fixtures);
  if !permitted {
    return Err(throw_script_error(
      &ctx,
      &ScriptError::policy(
        "defineFixtures() is refused by the operator policy (`[extensions.policy] fixtures = false`), \
         which forbids packages from contributing fixtures onto the base `test` chain",
      ),
    ));
  }
  if with_test_registry(&ctx, |r| r.base_sealed).map_err(|e| rq(&e))? {
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
  .map_err(|e| rq(&e))?
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
pub fn base_is_sealed(ctx: &Ctx<'_>) -> bool {
  with_test_registry(ctx, |r| r.base_sealed).unwrap_or(false)
}

// ── The test / describe object builders ──────────────────────────────

/// Build a `test` function object bound to one fixture set. `test.extend`
/// builds a fresh object over the extended set, so the full surface
/// (including further `.extend`) chains naturally.
fn make_test_object<'js>(ctx: &Ctx<'js>, fixture_set: usize) -> rquickjs::Result<Function<'js>> {
  let set = fixture_set;
  let test_fn = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<()> {
      let reg = parse_registration(&ctx, &args.0)?;
      push_test(&ctx, set, &[], reg, None, None)
    },
  )?;
  test_fn.set_name("test")?;
  let obj = test_fn
    .as_object()
    .ok_or_else(|| rq(&ScriptError::internal("test function has no object form".to_string())))?;

  for kind in ["skip", "fixme", "fail", "slow"] {
    let f = Function::new(ctx.clone(), move |ctx: Ctx<'js>, args: Rest<Value<'js>>| {
      modifier_call(kind, set, &ctx, &args.0)
    })?;
    obj.set(kind, f)?;
  }

  let only = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<()> {
      let reg = parse_registration(&ctx, &args.0)?;
      push_test(&ctx, set, &[modifier_annotation("only", None)], reg, None, None)
    },
  )?;
  obj.set("only", only)?;

  let each = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, rows: Value<'js>| -> rquickjs::Result<Function<'js>> {
      let rows: Vec<serde_json::Value> = serde_from_js(&ctx, rows)
        .map_err(|e| rq(&ScriptError::internal(format!("test.each rows must be an array: {e}"))))?;
      Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<()> {
          for row in &rows {
            let reg = parse_registration(&ctx, &args.0)?;
            let title = interpolate_title(&reg.title, row);
            push_test(&ctx, set, &[], reg, Some(row.clone()), Some(title))?;
          }
          Ok(())
        },
      )
    },
  )?;
  obj.set("each", each)?;

  for kind in ["beforeAll", "afterAll", "beforeEach", "afterEach"] {
    let f = Function::new(
      ctx.clone(),
      move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<()> {
        // `(fn)` or `(title, fn)` — the optional title is display-only.
        let func = args
          .0
          .iter()
          .find_map(as_function)
          .ok_or_else(|| rq(&ScriptError::internal(format!("test.{kind} has no function body"))))?;
        let (line, col) = capture_location(&ctx);
        let requested = destructured_keys(&ctx, &func);
        let saved = Persistent::save(&ctx, func);
        with_test_registry(&ctx, |r| {
          r.hooks.push(TestHookReg {
            kind: kind.to_string(),
            suite: r.describe_stack.last().copied(),
            fixture_set,
            func: saved,
            requested,
            line,
            col,
          });
        })
        .map_err(|e| rq(&e))
      },
    )?;
    obj.set(kind, f)?;
  }

  let use_fn = Function::new(ctx.clone(), |ctx: Ctx<'js>, bag: Value<'js>| -> rquickjs::Result<()> {
    let options: serde_json::Value = serde_from_js(&ctx, bag).map_err(|e| {
      rq(&ScriptError::internal(format!(
        "test.use options must be a plain object: {e}"
      )))
    })?;
    if !options.is_object() {
      return Err(rq(&ScriptError::internal(
        "test.use options must be a plain object".to_string(),
      )));
    }
    let (line, col) = capture_location(&ctx);
    with_test_registry(&ctx, |r| match r.describe_stack.last().copied() {
      Some(idx) => merge_use(&mut r.suites[idx].use_options, &options),
      None => r.file_use.push(FileUseReg { options, line, col }),
    })
    .map_err(|e| rq(&e))
  })?;
  obj.set("use", use_fn)?;

  let set_timeout = Function::new(ctx.clone(), |ctx: Ctx<'js>, ms: f64| -> rquickjs::Result<()> {
    current_bridge(&ctx, "test.setTimeout()")?.set_timeout_override(ms.max(0.0) as u64);
    Ok(())
  })?;
  obj.set("setTimeout", set_timeout)?;

  let info = Function::new(ctx.clone(), |ctx: Ctx<'js>| -> rquickjs::Result<Object<'js>> {
    let saved = with_test_registry(&ctx, |r| r.current.as_ref().map(|c| c.test_info.clone())).map_err(|e| rq(&e))?;
    let saved = saved.ok_or_else(|| {
      rq(&ScriptError::internal(
        "test.info() can only be called while a test is running".to_string(),
      ))
    })?;
    saved
      .restore(&ctx)
      .map_err(|e| rq(&ScriptError::internal(e.to_string())))
  })?;
  obj.set("info", info)?;

  obj.set("step", make_step_fn(ctx)?)?;

  let extend = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, fixtures: Object<'js>| -> rquickjs::Result<Function<'js>> {
      if fixture_set_of(&ctx, fixtures.as_value()).is_some() {
        return Err(rq(&ScriptError::internal(
          "test.extend() accepts fixtures object, not a test object.\nDid you mean to call mergeTests()?".to_string(),
        )));
      }
      let mut new_regs = Vec::new();
      for key in fixtures.keys::<String>() {
        let key = key?;
        let v: Value<'js> = fixtures.get(key.as_str())?;
        new_regs.push(parse_fixture_entry(&ctx, &key, v)?);
      }
      let new_set = with_test_registry(&ctx, |r| {
        let mut visible = r.fixture_sets.get(set).cloned().unwrap_or_default();
        for parsed in new_regs {
          append_fixture(r, &mut visible, parsed)?;
        }
        r.fixture_sets.push(visible);
        Ok(r.fixture_sets.len() - 1)
      })
      .map_err(|e| rq(&e))?
      .map_err(|e: ScriptError| rq(&e))?;
      make_test_object(&ctx, new_set)
    },
  )?;
  obj.set("extend", extend)?;

  let describe = make_describe_object(ctx)?;
  obj.set("describe", describe)?;

  // Non-enumerable, non-writable: a spread of `test` must not copy it,
  // and a suite must not be able to forge a chain by assigning it.
  obj.prop(test_type_key(ctx)?, rquickjs::object::Property::from(set as f64))?;

  Ok(test_fn)
}

/// Shallow `Object.assign` merge of a `use` bag into a target slot.
fn merge_use(target: &mut Option<serde_json::Value>, incoming: &serde_json::Value) {
  match target {
    Some(serde_json::Value::Object(t)) => {
      if let serde_json::Value::Object(inc) = incoming {
        for (k, v) in inc {
          t.insert(k.clone(), v.clone());
        }
      }
    },
    _ => *target = Some(incoming.clone()),
  }
}

fn register_suite(
  ctx: &Ctx<'_>,
  name: &str,
  mode: Option<CollectedSuiteMode>,
  annotations: Vec<CollectedAnnotation>,
) -> rquickjs::Result<usize> {
  let (line, col) = capture_location(ctx);
  if annotations.iter().any(|a| a.kind == "only") {
    with_test_registry(ctx, |r| r.has_only = true).map_err(|e| rq(&e))?;
  }
  with_test_registry(ctx, |r| {
    let parent = r.describe_stack.last().copied();
    r.suites.push(SuiteReg {
      name: name.to_string(),
      parent,
      mode,
      annotations,
      use_options: None,
      retries: None,
      timeout_ms: None,
      line,
      col,
    });
    let idx = r.suites.len() - 1;
    r.describe_stack.push(idx);
    idx
  })
  .map_err(|e| rq(&e))
}

fn pop_suite(ctx: &Ctx<'_>) {
  let _ = with_test_registry(ctx, |r| {
    r.describe_stack.pop();
  });
}

/// `describe(name, fn)` with a mode / annotation preset — the body runs
/// synchronously with the new suite on the registration stack.
fn describe_call<'js>(
  ctx: &Ctx<'js>,
  args: &[Value<'js>],
  mode: Option<CollectedSuiteMode>,
  annotations: Vec<CollectedAnnotation>,
) -> rquickjs::Result<()> {
  let name = args
    .first()
    .and_then(Value::as_string)
    .ok_or_else(|| rq(&ScriptError::internal("describe title must be a string".to_string())))?
    .to_string()?;
  let body = args.iter().skip(1).find_map(as_function).ok_or_else(|| {
    rq(&ScriptError::internal(format!(
      "describe `{name}` has no function body"
    )))
  })?;
  register_suite(ctx, &name, mode, annotations)?;
  let result = body.call::<_, ()>(());
  pop_suite(ctx);
  result
}

fn make_describe_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
  let describe_fn = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<()> { describe_call(&ctx, &args.0, None, Vec::new()) },
  )?;
  describe_fn.set_name("describe")?;
  let obj = describe_fn.as_object().ok_or_else(|| {
    rq(&ScriptError::internal(
      "describe function has no object form".to_string(),
    ))
  })?;

  for (name, mode) in [
    ("serial", CollectedSuiteMode::Serial),
    ("parallel", CollectedSuiteMode::Parallel),
  ] {
    let f = Function::new(ctx.clone(), move |ctx: Ctx<'js>, args: Rest<Value<'js>>| {
      describe_call(&ctx, &args.0, Some(mode), Vec::new())
    })?;
    obj.set(name, f)?;
  }

  for kind in ["skip", "fixme", "only"] {
    let f = Function::new(ctx.clone(), move |ctx: Ctx<'js>, args: Rest<Value<'js>>| {
      describe_call(&ctx, &args.0, None, vec![modifier_annotation(kind, None)])
    })?;
    obj.set(kind, f)?;
  }

  let each = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, rows: Value<'js>| -> rquickjs::Result<Function<'js>> {
      let rows: Vec<serde_json::Value> = serde_from_js(&ctx, rows).map_err(|e| {
        rq(&ScriptError::internal(format!(
          "describe.each rows must be an array: {e}"
        )))
      })?;
      Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> rquickjs::Result<()> {
          let template = args
            .0
            .first()
            .and_then(Value::as_string)
            .ok_or_else(|| {
              rq(&ScriptError::internal(
                "describe.each title must be a string".to_string(),
              ))
            })?
            .to_string()?;
          let body = args.0.iter().skip(1).find_map(as_function).ok_or_else(|| {
            rq(&ScriptError::internal(format!(
              "describe.each `{template}` has no function body"
            )))
          })?;
          for row in &rows {
            let title = interpolate_title(&template, row);
            register_suite(&ctx, &title, None, Vec::new())?;
            let row_js = crate::bindings::convert::json_to_js(&ctx, row)?;
            let result = body.call::<_, ()>((row_js,));
            pop_suite(&ctx);
            result?;
          }
          Ok(())
        },
      )
    },
  )?;
  obj.set("each", each)?;

  let configure = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, opts: Object<'js>| -> rquickjs::Result<()> {
      let mode = match opts.get::<_, String>("mode") {
        Ok(m) => Some(match m.as_str() {
          "serial" => CollectedSuiteMode::Serial,
          "parallel" => CollectedSuiteMode::Parallel,
          "default" => return Ok(()),
          other => {
            return Err(rq(&ScriptError::internal(format!(
              "describe.configure: unknown mode `{other}`"
            ))));
          },
        }),
        Err(_) => None,
      };
      let retries = opts.get::<_, f64>("retries").ok().map(|r| r.max(0.0) as u32);
      let timeout_ms = opts.get::<_, f64>("timeout").ok().map(|t| t.max(0.0) as u64);
      let (line, col) = capture_location(&ctx);
      with_test_registry(&ctx, |r| match r.describe_stack.last().copied() {
        Some(idx) => {
          let s = &mut r.suites[idx];
          if mode.is_some() {
            s.mode = mode;
          }
          if retries.is_some() {
            s.retries = retries;
          }
          if timeout_ms.is_some() {
            s.timeout_ms = timeout_ms;
          }
        },
        None => r.file_configure.push(FileConfigureReg {
          mode,
          retries,
          timeout_ms,
          line,
          col,
        }),
      })
      .map_err(|e| rq(&e))
    },
  )?;
  obj.set("configure", configure)?;

  Ok(describe_fn)
}

/// Install the Playwright-shaped test surface: the `TestRegistry`
/// userdata plus the base `test` / `describe` objects and `mergeTests`
/// on the `ferridriver` global (consumed by the `@ferridriver/test`
/// native module — they are NOT bare globals). Idempotent; called once
/// at `Session::create`, for EVERY host: a fixture chain an extension
/// or a step file builds must exist wherever it is imported, and only
/// the Test host consumes what registering leaves behind.
pub fn install_test(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  if ctx.userdata::<TestRegistryUserData>().is_some() {
    return Ok(());
  }
  let _ = ctx.store_userdata(TestRegistryUserData(RefCell::new(TestRegistry::new())));

  let test = make_test_object(ctx, BASE_FIXTURE_SET)?;
  let describe = make_describe_object(ctx)?;
  let fd = crate::bindings::runtime::ensure_ferridriver(ctx)?;
  // Playwright exports the unextended root as `_baseTest`; it is this
  // very object, not a copy, so `mergeTests(_baseTest, x)` sees the
  // shared ancestor it has to dedup against.
  //
  // Both are read-only: they ARE the base chain's identity, and an
  // importer links to the object it found here. Letting a package
  // reassign either would give it a way to look like it had replaced
  // the base chain while every module that already imported kept the
  // original — the failure `defineFixtures` exists to avoid.
  fd.prop("baseTest", rquickjs::object::Property::from(test.clone()).enumerable())?;
  fd.prop("test", rquickjs::object::Property::from(test).enumerable())?;
  fd.set("describe", describe)?;
  fd.set("mergeTests", Function::new(ctx.clone(), merge_tests)?)?;
  crate::bindings::define_config::install(ctx)?;
  fd.set("selectors", make_selectors_object(ctx)?)?;
  let define_fixtures_fn = Function::new(ctx.clone(), define_fixtures)?;
  define_fixtures_fn.set_name("defineFixtures")?;
  ctx.globals().set("defineFixtures", define_fixtures_fn.clone())?;
  fd.set("defineFixtures", define_fixtures_fn)?;
  Ok(())
}

/// Playwright's `selectors`
/// (`packages/playwright-core/src/client/selectors.ts`): `register(name,
/// script, options)` and `setTestIdAttribute(attributeName)`.
///
/// Both rules are core's — the registry, the duplicate-name message, the
/// `Function | string | { path, content }` lowering — so this only
/// decides which of those shapes the caller passed.
fn make_selectors_object<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;

  let register = Function::new(
    ctx.clone(),
    |ctx: Ctx<'js>, name: String, script: Value<'js>, options: rquickjs::function::Opt<Object<'js>>| {
      let lowered = if let Some(func) = script.as_function() {
        let source: String = func
          .as_object()
          .ok_or_else(|| {
            rq(&ScriptError::internal(
              "selectors.register: script must be a function".to_string(),
            ))
          })?
          .get::<_, Function<'js>>("toString")?
          .call((rquickjs::function::This(func.clone()),))?;
        ferridriver::selectors::SelectorScript::Function(source)
      } else if let Some(source) = script.as_string() {
        ferridriver::selectors::SelectorScript::Source(source.to_string()?)
      } else if let Some(bag) = script.as_object() {
        match (
          bag.get::<_, Option<String>>("content")?,
          bag.get::<_, Option<String>>("path")?,
        ) {
          (Some(content), _) => ferridriver::selectors::SelectorScript::Source(content),
          (None, Some(path)) => ferridriver::selectors::SelectorScript::Path(std::path::PathBuf::from(path)),
          (None, None) => {
            return Err(rq(&ScriptError::internal(
              "Either path or content property must be present".to_string(),
            )));
          },
        }
      } else {
        return Err(rq(&ScriptError::internal(
          "selectors.register: script must be a function, a string, or { path } / { content }".to_string(),
        )));
      };

      let content_script = options
        .0
        .map(|o| o.get::<_, Option<bool>>("contentScript"))
        .transpose()?
        .flatten()
        .unwrap_or(false);
      let source = ferridriver::selectors::evaluation_script(&lowered).map_err(|e| ferri_throw(&ctx, &e))?;
      ferridriver::selectors::register_selector_engine(&name, &source, content_script)
        .map_err(|e| ferri_throw(&ctx, &e))?;
      // Playwright's `register` is `Promise<void>`; a suite may `.then`
      // it, so hand back a real promise rather than `undefined`.
      let (promise, resolve, _reject) = rquickjs::Promise::new(&ctx)?;
      resolve.call::<_, ()>(())?;
      Ok::<_, rquickjs::Error>(promise)
    },
  )?;
  obj.set("register", register)?;

  let set_test_id_attribute = Function::new(ctx.clone(), |name: String| {
    ferridriver::selectors::set_default_test_id_attribute(&name);
  })?;
  obj.set("setTestIdAttribute", set_test_id_attribute)?;
  Ok(obj)
}

// ── Invocation ───────────────────────────────────────────────────────

/// Name of the error `stepInfo.skip()` throws to unwind its own body.
/// Swallowed at the step boundary — the step is reported skipped, the
/// test carries on. Playwright's `StepSkipError`.
const STEP_SKIP_ERROR: &str = "StepSkipError";

/// `test.step(title, body, { box, location, timeout })` and
/// `test.step.skip(...)` — opens a live reporter/trace step around the
/// body, returns the body's resolved value, re-throws its error after
/// closing the step.
///
/// Every rule (which location the step reports, what `box` re-attributes
/// to, the timeout message, when the body is skipped) is
/// `ferridriver_test::step`'s; this only lowers the JS option bag and
/// mirrors `TestStepInfo` onto a JS object.
fn make_step_fn<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
  let step = step_callable(ctx, StepExpectation::Pass)?;
  let skip = step_callable(ctx, StepExpectation::Skip)?;
  if let Some(obj) = step.as_object() {
    obj.set("skip", skip)?;
  }
  Ok(step)
}

fn step_callable<'js>(ctx: &Ctx<'js>, expectation: StepExpectation) -> rquickjs::Result<Function<'js>> {
  Function::new(
    ctx.clone(),
    Async(
      move |ctx: Ctx<'js>,
            frames: call_site::CallFrames,
            title: String,
            f: Function<'js>,
            options: rquickjs::function::Opt<Value<'js>>|
            -> Pin<Box<dyn Future<Output = rquickjs::Result<Value<'js>>> + 'js>> {
        // The option bag and the stack are read here, on the calling
        // stack: an `Async` body first runs when the VM executor polls
        // it, by which time the caller's frames are gone.
        let options = parse_step_options(&ctx, options.0.as_ref());
        Box::pin(async move {
          let bridge = current_bridge(&ctx, "test.step()")?;
          let spec = StepSpec::new(title)
            .with_options(options?)
            .with_frames(
              frames
                .0
                .into_iter()
                .map(|(_, line, column)| StepFrame::Host { line, column })
                .collect(),
            )
            .expecting(expectation);

          let ctx_for_body = ctx.clone();
          let bridge_for_body = Arc::clone(&bridge);
          let outcome = ferridriver_test::step::run(&*bridge, spec, move |run| async move {
            let ctx = ctx_for_body;
            let step_info = build_step_info(&ctx, &bridge_for_body, &run).map_err(|e| body_error(&ctx, e))?;
            if !run.should_run_body() {
              return Ok(Value::new_undefined(ctx.clone()));
            }
            let called: rquickjs::Result<MaybePromise<'_>> = f.call((step_info.clone(),));
            let settled = match called {
              Ok(mp) => mp.into_future::<Value<'js>>().await,
              Err(e) => Err(e),
            };
            collect_step_annotations(&step_info, &run);
            match settled {
              Ok(v) => Ok(v),
              Err(e) => {
                let failure = body_error(&ctx, e);
                if failure.name == STEP_SKIP_ERROR {
                  return Ok(Value::new_undefined(ctx.clone()));
                }
                Err(failure)
              },
            }
          })
          .await;

          match outcome {
            Ok(v) => Ok(v),
            // Re-throw as a real `Error` carrying the original name and
            // message. `rquickjs::Error::new_from_js_message` would
            // render as "Error converting from js 'bdd' into type
            // 'Error': ...", and that prefix reaches the user through
            // every report the run writes.
            Err(e) => Err(crate::bindings::convert::throw_named_with_stack(
              &ctx,
              e.name(),
              e.message().to_string(),
              e.stack().map(ToString::to_string),
            )),
          }
        })
      },
    ),
  )
}

/// A JS exception, in the host-neutral shape core reports steps with.
fn body_error(ctx: &Ctx<'_>, error: rquickjs::Error) -> StepBodyError {
  match error {
    rquickjs::Error::Exception => {
      let caught = ctx.catch();
      let exception = caught.as_exception();
      StepBodyError {
        name: exception
          .and_then(|ex| ex.as_object().get::<_, String>("name").ok())
          .filter(|n| !n.is_empty())
          .unwrap_or_else(|| "Error".to_string()),
        message: exception
          .and_then(rquickjs::Exception::message)
          .unwrap_or_else(|| "step failed".to_string()),
        stack: exception.and_then(rquickjs::Exception::stack),
      }
    },
    other => StepBodyError::new("Error", other.to_string()),
  }
}

/// `{ box?, location?, timeout? }` — Playwright's option bag
/// (`packages/playwright/types/test.d.ts:6728`).
fn parse_step_options<'js>(ctx: &Ctx<'js>, options: Option<&Value<'js>>) -> rquickjs::Result<StepOptions> {
  let Some(options) = options.and_then(Value::as_object) else {
    return Ok(StepOptions::default());
  };
  let mut parsed = StepOptions {
    box_step: options.get::<_, Option<bool>>("box")?.unwrap_or(false),
    location: None,
    timeout: options
      .get::<_, Option<f64>>("timeout")?
      .filter(|ms| *ms > 0.0)
      .map(|ms| std::time::Duration::from_millis(ms as u64)),
  };
  if let Some(location) = options.get::<_, Option<Object<'js>>>("location")? {
    let file: String = location
      .get("file")
      .map_err(|_| rq(&ScriptError::internal("test.step: location needs a `file`".to_string())))?;
    parsed.location = Some(StepLocation {
      file,
      line: location.get::<_, Option<f64>>("line")?.unwrap_or(0.0) as u32,
      column: location.get::<_, Option<f64>>("column")?.unwrap_or(0.0) as u32,
    });
  }
  let _ = ctx;
  Ok(parsed)
}

/// Playwright's `TestStepInfo` (`types/test.d.ts:10700-10816`):
/// `attach`, both `skip` forms, and `titlePath`. `annotations` is on
/// upstream's implementation but not in its public interface; it is
/// mirrored here as a real array so a push reaches the step's record.
fn build_step_info<'js>(
  ctx: &Ctx<'js>,
  bridge: &Arc<dyn TestHostBridge>,
  run: &Arc<ferridriver_test::step::StepRun>,
) -> rquickjs::Result<Object<'js>> {
  let obj = Object::new(ctx.clone())?;
  obj.set("titlePath", run.title_path().to_vec())?;
  obj.set("annotations", rquickjs::Array::new(ctx.clone())?)?;

  let skip_run = Arc::clone(run);
  let skip = Function::new(ctx.clone(), move |ctx: Ctx<'js>, args: Rest<Value<'js>>| {
    // `skip()` unconditional; `skip(condition, description?)`.
    if let Some(condition) = args.0.first()
      && !condition
        .as_bool()
        .unwrap_or(!condition.is_null() && !condition.is_undefined())
    {
      return Ok(());
    }
    let description = args
      .0
      .get(1)
      .and_then(Value::as_string)
      .and_then(|s| s.to_string().ok());
    skip_run.record_skip(description.clone());
    Err::<(), _>(crate::bindings::convert::throw_named(
      &ctx,
      STEP_SKIP_ERROR,
      description.unwrap_or_else(|| "step skipped".to_string()),
    ))
  })?;
  obj.set("skip", skip)?;

  let attach_bridge = Arc::clone(bridge);
  let step_id = run.step_id().to_string();
  let attach = Function::new(
    ctx.clone(),
    Async(
      move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> Pin<Box<dyn Future<Output = rquickjs::Result<()>> + 'js>> {
        let bridge = Arc::clone(&attach_bridge);
        let step_id = step_id.clone();
        Box::pin(async move {
          let (name, content_type, source) = parse_attach_args(&ctx, &args.0)?;
          let bytes = match source {
            AttachSource::Bytes(b) => b,
            AttachSource::Path(p) => tokio::fs::read(&p)
              .await
              .map_err(|e| rq(&ScriptError::internal(format!("stepInfo.attach: reading {p}: {e}"))))?,
          };
          bridge.attach(name, content_type, bytes, Some(step_id)).await;
          Ok(())
        })
      },
    ),
  )?;
  obj.set("attach", attach)?;
  Ok(obj)
}

/// Fold whatever the body pushed onto `stepInfo.annotations` into the
/// step's record, so a reporter sees it.
fn collect_step_annotations<'js>(step_info: &Object<'js>, run: &Arc<ferridriver_test::step::StepRun>) {
  let Ok(annotations) = step_info.get::<_, rquickjs::Array<'js>>("annotations") else {
    return;
  };
  for entry in annotations.iter::<Object<'js>>().flatten() {
    let Ok(type_name) = entry.get::<_, String>("type") else {
      continue;
    };
    run.annotate(type_name, entry.get::<_, Option<String>>("description").ok().flatten());
  }
}

fn se(e: impl std::fmt::Display) -> ScriptError {
  ScriptError::internal(e.to_string())
}

fn build_test_info<'js>(
  ctx: &Ctx<'js>,
  info: &TestInfoData,
  bridge: &Arc<dyn TestHostBridge>,
) -> Result<Object<'js>, ScriptError> {
  let obj = Object::new(ctx.clone()).map_err(se)?;
  obj.set("title", info.title.clone()).map_err(se)?;
  obj.set("titlePath", info.title_path.clone()).map_err(se)?;
  obj.set("file", info.file.clone()).map_err(se)?;
  obj.set("line", info.line).map_err(se)?;
  obj.set("column", info.column).map_err(se)?;
  obj.set("retry", info.retry).map_err(se)?;
  obj.set("workerIndex", info.worker_index).map_err(se)?;
  obj.set("parallelIndex", info.parallel_index).map_err(se)?;
  obj.set("repeatEachIndex", info.repeat_each_index).map_err(se)?;
  obj.set("timeout", info.timeout_ms).map_err(se)?;
  obj.set("expectedStatus", info.expected_status.clone()).map_err(se)?;
  obj.set("tags", info.tags.clone()).map_err(se)?;
  obj.set("outputDir", info.output_dir.clone()).map_err(se)?;
  obj.set("snapshotDir", info.snapshot_dir.clone()).map_err(se)?;
  obj.set("snapshotSuffix", info.snapshot_suffix.clone()).map_err(se)?;
  obj
    .set(
      "config",
      crate::bindings::convert::json_to_js(ctx, &info.config).map_err(se)?,
    )
    .map_err(se)?;
  match &info.project_name {
    Some(name) => {
      let project = Object::new(ctx.clone()).map_err(se)?;
      project.set("name", name.clone()).map_err(se)?;
      obj.set("project", project).map_err(se)?;
    },
    None => obj.set("project", Value::new_null(ctx.clone())).map_err(se)?,
  }

  // Live getters backed by the bridge: annotations grows via
  // `annotate()`, attachmentCount via `attach()`.
  install_bridge_getter(ctx, &obj, "annotations", {
    let bridge = Arc::clone(bridge);
    move |ctx: &Ctx<'js>| -> rquickjs::Result<Value<'js>> {
      let arr = rquickjs::Array::new(ctx.clone())?;
      for (i, (kind, description)) in bridge.annotations().into_iter().enumerate() {
        let a = Object::new(ctx.clone())?;
        a.set("type", kind)?;
        if let Some(d) = description {
          a.set("description", d)?;
        }
        arr.set(i, a)?;
      }
      Ok(arr.into_value())
    }
  })
  .map_err(se)?;
  install_bridge_getter(ctx, &obj, "attachmentCount", {
    let bridge = Arc::clone(bridge);
    move |ctx: &Ctx<'js>| -> rquickjs::Result<Value<'js>> {
      rquickjs::IntoJs::into_js(bridge.attachment_count() as u32, ctx)
    }
  })
  .map_err(se)?;
  install_bridge_getter(ctx, &obj, "errors", {
    let bridge = Arc::clone(bridge);
    move |ctx: &Ctx<'js>| -> rquickjs::Result<Value<'js>> { rquickjs::IntoJs::into_js(bridge.errors(), ctx) }
  })
  .map_err(se)?;

  // testInfo.attach(name, contentType, body[, opts]) — positional — or
  // Playwright's testInfo.attach(name, { body?, contentType?, path? }).
  let attach_bridge = Arc::clone(bridge);
  let attach = Function::new(
    ctx.clone(),
    Async(
      move |ctx: Ctx<'js>, args: Rest<Value<'js>>| -> Pin<Box<dyn Future<Output = rquickjs::Result<()>> + 'js>> {
        let bridge = Arc::clone(&attach_bridge);
        Box::pin(async move {
          let (name, content_type, source) = parse_attach_args(&ctx, &args.0)?;
          let bytes = match source {
            AttachSource::Bytes(b) => b,
            AttachSource::Path(p) => tokio::fs::read(&p)
              .await
              .map_err(|e| rq(&ScriptError::internal(format!("testInfo.attach: reading {p}: {e}"))))?,
          };
          bridge.attach(name, content_type, bytes, None).await;
          Ok(())
        })
      },
    ),
  )
  .map_err(se)?;
  obj.set("attach", attach).map_err(se)?;

  let annotate_bridge = Arc::clone(bridge);
  let annotate = Function::new(
    ctx.clone(),
    move |kind: String, description: rquickjs::function::Opt<String>| {
      annotate_bridge.annotate(kind, description.0);
    },
  )
  .map_err(se)?;
  obj.set("annotate", annotate).map_err(se)?;

  for kind in ["skip", "fixme", "fail", "slow"] {
    let f = Function::new(ctx.clone(), move |ctx: Ctx<'js>, args: Rest<Value<'js>>| {
      runtime_modifier(&ctx, kind, &args.0)
    })
    .map_err(se)?;
    obj.set(kind, f).map_err(se)?;
  }
  let set_timeout = Function::new(ctx.clone(), |ctx: Ctx<'js>, ms: f64| -> rquickjs::Result<()> {
    current_bridge(&ctx, "testInfo.setTimeout()")?.set_timeout_override(ms.max(0.0) as u64);
    Ok(())
  })
  .map_err(se)?;
  obj.set("setTimeout", set_timeout).map_err(se)?;

  let output_bridge = Arc::clone(bridge);
  let output_path = Function::new(ctx.clone(), move |parts: Rest<String>| {
    output_bridge.output_path(&parts.0)
  })
  .map_err(se)?;
  obj.set("outputPath", output_path).map_err(se)?;
  let snap_bridge = Arc::clone(bridge);
  // Playwright: `snapshotPath(...name: string[])` and
  // `snapshotPath(name, { kind })` (`worker/testInfo.ts:644-662`) — a
  // trailing object is the options bag, everything before it is a path.
  let snapshot_path = Function::new(ctx.clone(), move |args: Rest<Value<'js>>| -> rquickjs::Result<String> {
    let mut parts: Vec<String> = Vec::with_capacity(args.0.len());
    let mut kind = "snapshot".to_string();
    let last = args.0.len().saturating_sub(1);
    for (index, value) in args.0.iter().enumerate() {
      if index == last
        && let Some(obj) = value.as_object()
        && !value.is_string()
      {
        if let Some(k) = obj.get::<_, Option<String>>("kind")? {
          kind = k;
        }
        break;
      }
      if let Some(s) = value.as_string() {
        parts.push(s.to_string()?);
      }
    }
    snap_bridge
      .snapshot_path(&parts, &kind)
      .map_err(|message| rq(&ScriptError::internal(message)))
  })
  .map_err(se)?;
  obj.set("snapshotPath", snapshot_path).map_err(se)?;

  Ok(obj)
}

fn install_bridge_getter<'js>(
  ctx: &Ctx<'js>,
  target: &Object<'js>,
  name: &str,
  getter: impl Fn(&Ctx<'js>) -> rquickjs::Result<Value<'js>> + 'js,
) -> rquickjs::Result<()> {
  let object_global: Object<'js> = ctx.globals().get("Object")?;
  let define_property: Function<'js> = object_global.get("defineProperty")?;
  let getter_fn = Function::new(ctx.clone(), move |ctx: Ctx<'js>| getter(&ctx))?;
  let descriptor = Object::new(ctx.clone())?;
  descriptor.set("get", getter_fn)?;
  descriptor.set("configurable", true)?;
  let _: Value<'js> = define_property.call((target.clone(), name, descriptor))?;
  Ok(())
}

enum AttachSource {
  Bytes(Vec<u8>),
  Path(String),
}

fn parse_attach_args<'js>(ctx: &Ctx<'js>, args: &[Value<'js>]) -> rquickjs::Result<(String, String, AttachSource)> {
  let name = args
    .first()
    .and_then(Value::as_string)
    .ok_or_else(|| {
      rq(&ScriptError::internal(
        "testInfo.attach: name must be a string".to_string(),
      ))
    })?
    .to_string()?;
  // Positional: (name, contentType, body[, opts]).
  if let Some(ct) = args.get(1).and_then(Value::as_string) {
    let content_type = ct.to_string()?;
    let body = args
      .get(2)
      .ok_or_else(|| rq(&ScriptError::internal("testInfo.attach: missing body".to_string())))?;
    let bytes = ferridriver_jsstd::node::bytes::value_to_bytes(ctx, body, None)?;
    return Ok((name, content_type, AttachSource::Bytes(bytes)));
  }
  // Option bag: (name, { body?, contentType?, path? }).
  let Some(opts) = args.get(1).and_then(Value::as_object) else {
    return Err(rq(&ScriptError::internal(
      "testInfo.attach: second argument must be a content type or an options object".to_string(),
    )));
  };
  let content_type = opts
    .get::<_, Value<'js>>("contentType")
    .ok()
    .and_then(|v| v.as_string().and_then(|s| s.to_string().ok()));
  if let Ok(path) = opts.get::<_, String>("path") {
    let content_type = content_type.unwrap_or_else(|| "application/octet-stream".to_string());
    return Ok((name, content_type, AttachSource::Path(path)));
  }
  let body: Value<'js> = opts.get("body").map_err(|_| {
    rq(&ScriptError::internal(
      "testInfo.attach: options need `body` or `path`".to_string(),
    ))
  })?;
  let default_ct = if body.as_string().is_some() {
    "text/plain"
  } else {
    "application/octet-stream"
  };
  let bytes = ferridriver_jsstd::node::bytes::value_to_bytes(ctx, &body, None)?;
  Ok((
    name,
    content_type.unwrap_or_else(|| default_ct.to_string()),
    AttachSource::Bytes(bytes),
  ))
}

/// Build the per-test fixtures object (`{ page, context, request,
/// browser, browserName, headless, isMobile, hasTouch, testInfo, ...
/// custom }`) and register it as the VM's current test.
pub(crate) fn set_current_test(
  ctx: &Ctx<'_>,
  vm: &crate::vm::VmHandle,
  world: &TestWorldData,
  bridge: Arc<dyn TestHostBridge>,
) -> Result<(), ScriptError> {
  let obj = Object::new(ctx.clone()).map_err(se)?;
  obj.set("browserName", world.browser_name.clone()).map_err(se)?;
  obj.set("headless", world.headless).map_err(se)?;
  obj.set("isMobile", world.is_mobile).map_err(se)?;
  obj.set("hasTouch", world.has_touch).map_err(se)?;
  obj.set("baseURL", world.base_url.clone()).map_err(se)?;
  if let Some(page) = &world.page {
    install_page_on(ctx, &obj, Arc::clone(page), vm.clone()).map_err(se)?;
  }
  if let Some(c) = &world.context {
    install_browser_context_on(ctx, &obj, Arc::clone(c)).map_err(se)?;
  }
  if let Some(r) = &world.request {
    install_request_on(ctx, &obj, Arc::clone(r)).map_err(se)?;
  }
  if let Some(b) = &world.browser {
    install_browser_on(ctx, &obj, Arc::clone(b)).map_err(se)?;
  }
  let info_obj = build_test_info(ctx, &world.info, &bridge)?;
  obj.set("testInfo", info_obj.clone()).map_err(se)?;
  // The `expect` block this test's project resolved to — every bare
  // `expect(...)` the body makes starts from its timeout.
  crate::bindings::expect::set_expect_defaults(ctx, Arc::clone(&world.expect));

  let world_saved = Persistent::save(ctx, obj);
  let info_saved = Persistent::save(ctx, info_obj);
  with_test_registry(ctx, |r| {
    r.current = Some(CurrentTest {
      world: world_saved,
      test_info: info_saved,
      bridge,
      pending: Vec::new(),
    });
  })
}

/// The `test.extend` chain for a fixture set, in extend order — the
/// input to [`fixture_graph`]'s Playwright-shaped resolution.
pub(crate) fn fixture_slots(reg: &TestRegistry, fixture_set: usize) -> Vec<FixtureSlot> {
  reg
    .fixture_sets
    .get(fixture_set)
    .map(Vec::as_slice)
    .unwrap_or_default()
    .iter()
    .map(|&i| {
      let f = &reg.fixtures[i];
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

/// Snapshot of the `test.extend` chains registered in this VM, indexed
/// by fixture set — the input [`dominant_fixture_set`] reasons over.
pub(crate) fn fixture_set_table(reg: &TestRegistry) -> Vec<Vec<usize>> {
  reg.fixture_sets.clone()
}

/// Where the test surface's registries stand — `(tests, fixtures)`.
pub(crate) fn registry_marks(ctx: &Ctx<'_>) -> Result<(usize, usize), ScriptError> {
  with_test_registry(ctx, |r| (r.tests.len(), r.fixtures.len()))
}

/// Test titles and fixture names registered since those marks.
pub(crate) fn registrations_since(
  ctx: &Ctx<'_>,
  tests: usize,
  fixtures: usize,
) -> Result<(Vec<String>, Vec<String>), ScriptError> {
  with_test_registry(ctx, |r| {
    (
      r.tests
        .get(tests..)
        .unwrap_or_default()
        .iter()
        .map(|t| t.title.clone())
        .collect(),
      r.fixtures
        .get(fixtures..)
        .unwrap_or_default()
        .iter()
        .map(|f| f.name.clone())
        .collect(),
    )
  })
}

/// Positional registrations on the test surface: tests, suites, hooks
/// and fixtures. Consumers address all of them by index, so a partial
/// registration is not something a host can skip past.
pub(crate) fn registration_count(ctx: &Ctx<'_>) -> Result<usize, ScriptError> {
  with_test_registry(ctx, |r| {
    r.tests.len() + r.suites.len() + r.hooks.len() + r.fixtures.len() + r.modifiers.len()
  })
}

/// Every fixture registration in this VM, in registration order — the
/// table [`fixture_set_table`]'s indices point into.
pub(crate) fn fixture_table(reg: &TestRegistry) -> Vec<CollectedFixture> {
  reg
    .fixtures
    .iter()
    .map(|f| CollectedFixture {
      name: f.name.clone(),
      scope: f.scope,
      auto: f.auto,
      option: f.option,
      deps: f.deps.clone(),
    })
    .collect()
}

/// Clear the current-test slot (the BDD host ends a scenario without
/// going through [`run_test`]).
pub(crate) fn clear_current_test(ctx: &Ctx<'_>) -> Result<(), ScriptError> {
  with_test_registry(ctx, |r| r.current = None)
}

/// The fixtures object of the running test / scenario, once
/// [`set_current_test`] built it.
pub(crate) fn current_world_object<'js>(ctx: &Ctx<'js>) -> Result<Object<'js>, ScriptError> {
  let saved = with_test_registry(ctx, |r| r.current.as_ref().map(|c| c.world.clone()))?
    .ok_or_else(|| ScriptError::internal("no current test".to_string()))?;
  saved.restore(ctx).map_err(se)
}

/// Resolve the custom fixtures a test (plus its each-hooks) needs, in
/// dependency order, running `use()`-handshake factories to their
/// suspension point. Worker-scoped fixtures are set up once per VM and
/// reused.
/// Playwright's per-function unknown-parameter check
/// (`common/fixtures.ts:250-256`), applied before anything is resolved.
///
/// The built-in set is core's, not this world's key set: a `beforeAll`
/// world carries no `page`, and answering `beforeAll hook has unknown
/// parameter "page"` for a name the runtime plainly knows would be a
/// worse lie than the `undefined` this replaces. A name of the wrong
/// SCOPE is a different failure and is deliberately not decided here.
///
/// Playwright raises these as load errors, failing every test in the
/// file; ferridriver raises them per function, so only the test that
/// asks for the name fails. Recorded in `docs/playwright-compat.md`.
pub(crate) fn validate_requested_names<'js>(
  ctx: &Ctx<'js>,
  world_obj: &Object<'js>,
  fixture_set: usize,
  functions: &[(String, Vec<String>)],
) -> Result<(), ScriptError> {
  let slots = with_test_registry(ctx, |r| fixture_slots(r, fixture_set))?;
  let is_builtin = |name: &str| {
    ferridriver_test::fixture_graph::BUILTIN_FIXTURES.contains(&name) || world_obj.contains_key(name).unwrap_or(false)
  };
  for (prefix, requested) in functions {
    fixture_graph::validate_requested(&slots, requested, &is_builtin, prefix).map_err(ScriptError::internal)?;
  }
  Ok(())
}

pub(crate) async fn resolve_custom_fixtures<'js>(
  ctx: &Ctx<'js>,
  world_obj: &Object<'js>,
  fixture_set: usize,
  requested: &[String],
  use_options: &serde_json::Value,
  source_label: &str,
) -> Result<(), ScriptError> {
  let slots = with_test_registry(ctx, |r| fixture_slots(r, fixture_set))?;
  // A name the runtime already put on the fixtures object IS the base
  // implementation an override shadows (`page`, `context`, `request`,
  // `browser`, the config scalars). That is the only thing separating a
  // legitimate override from a self-reference with nothing under it.
  let is_builtin = |name: &str| world_obj.contains_key(name).unwrap_or(false);
  let ordered = fixture_graph::resolution_order(&slots, requested, &is_builtin).map_err(ScriptError::internal)?;

  for pos in ordered {
    resolve_one_fixture(ctx, world_obj, slots[pos].reg, use_options, source_label).await?;
  }
  Ok(())
}

async fn resolve_one_fixture<'js>(
  ctx: &Ctx<'js>,
  world_obj: &Object<'js>,
  reg_idx: usize,
  use_options: &serde_json::Value,
  source_label: &str,
) -> Result<(), ScriptError> {
  enum Plan {
    CachedWorker(Persistent<Value<'static>>),
    Static(Option<Persistent<Value<'static>>>),
    Factory {
      factory: Persistent<Function<'static>>,
      worker_scoped: bool,
    },
  }
  let (name, option, plan, setup) = with_test_registry(ctx, |r| {
    let f = &r.fixtures[reg_idx];
    let plan = if let Some(cached) = r.worker_fixtures.get(&reg_idx) {
      Plan::CachedWorker(cached.value.clone())
    } else if let Some(factory) = &f.factory {
      Plan::Factory {
        factory: factory.clone(),
        worker_scoped: f.scope == FixtureScope::Worker,
      }
    } else {
      Plan::Static(f.static_value.clone())
    };
    (
      f.name.clone(),
      f.option,
      plan,
      FixtureSetup {
        timeout_ms: f.timeout_ms,
        title: f.title.clone(),
        boxed: f.boxed,
      },
    )
  })?;

  // An option fixture named in a `use` bag takes that value whatever
  // its declaration was: Playwright replaces the registration's `fn`
  // with the override, and a value that came out of a config file is
  // never a factory (`common/fixtures.ts:98-101`).
  let override_v = option.then(|| use_options.get(&name)).flatten();

  match (plan, override_v) {
    (Plan::CachedWorker(value), _) => {
      let v = value.restore(ctx).map_err(se)?;
      world_obj.set(name.as_str(), v).map_err(se)?;
      Ok(())
    },
    (_, Some(json)) => {
      let v = crate::bindings::convert::json_to_js(ctx, json).map_err(se)?;
      world_obj.set(name.as_str(), v).map_err(se)?;
      Ok(())
    },
    (Plan::Static(value), None) => {
      let v: Value<'_> = match value {
        Some(saved) => saved.restore(ctx).map_err(se)?,
        None => Value::new_undefined(ctx.clone()),
      };
      world_obj.set(name.as_str(), v).map_err(se)?;
      Ok(())
    },
    (Plan::Factory { factory, worker_scoped }, None) => {
      setup
        .around(ctx, &name, || {
          run_fixture_factory(ctx, world_obj, reg_idx, &name, factory, worker_scoped, source_label)
        })
        .await
    },
  }
}

/// What a fixture's step is called: `{ title }` when it has one, the
/// fixture's own name otherwise, quoted inside `Fixture "…"`
/// (`worker/fixtureRunner.ts:48-50`).
fn fixture_step_title(title: Option<&str>, name: &str) -> String {
  format!("Fixture \"{}\"", title.unwrap_or(name))
}

/// The `{ timeout, title, box }` half of a fixture's option bag.
struct FixtureSetup {
  timeout_ms: Option<u64>,
  title: Option<String>,
  boxed: Option<FixtureBox>,
}

impl FixtureSetup {
  /// Run a fixture's setup as Playwright runs it: inside a step titled
  /// `Fixture "name"`, under its own timeout.
  ///
  /// `box: 'self'` opens no step — the fixture still runs, it just does
  /// not appear — and `box: true` keeps the step but marks it grouped,
  /// which is what keeps a framework's own fixtures out of the way of a
  /// test's steps (`worker/fixtureRunner.ts:47-53`).
  async fn around<'js, F, Fut>(&self, ctx: &Ctx<'js>, name: &str, body: F) -> Result<(), ScriptError>
  where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), ScriptError>>,
  {
    let run = async {
      match self.timeout_ms {
        Some(ms) if ms > 0 => match tokio::time::timeout(std::time::Duration::from_millis(ms), body()).await {
          Ok(r) => r,
          Err(_) => Err(ScriptError::internal(format!(
            "fixture `{name}` setup timed out after {ms}ms"
          ))),
        },
        _ => body().await,
      }
    };
    if self.boxed == Some(FixtureBox::SelfOnly) {
      return run.await;
    }
    let Some(bridge) = optional_bridge(ctx) else {
      // No test is running (collection, a worker-scope setup outside a
      // test): the fixture still resolves, it just has nothing to
      // attach a step to.
      return run.await;
    };
    let mut spec = ferridriver_test::step::StepSpec::new(fixture_step_title(self.title.as_deref(), name));
    spec.category = ferridriver_test::model::StepCategory::Fixture;
    if self.boxed == Some(FixtureBox::Group) {
      // Upstream marks a boxed fixture's step `group: 'configuration'`
      // for one a user registered (`worker/fixtureRunner.ts:52`).
      // ferridriver has no framework-registered boxed fixture, so that
      // is the only group reachable here.
      spec.metadata = Some(serde_json::json!({ "group": "configuration" }));
    }
    ferridriver_test::step::run(&*bridge, spec, move |_run| async move {
      run
        .await
        .map_err(|e| ferridriver_test::step::StepBodyError::new("Error", e.message))
    })
    .await
    .map_err(|f| ScriptError::internal(f.message()))
  }
}

/// Run one `use()`-handshake factory to its suspension point: call
/// `factory(fixtures, use)`, drive it on the VM schedular, and wait
/// until it either calls `use(value)` (setup done, factory parked on
/// the gate promise) or settles without doing so (error).
async fn run_fixture_factory<'js>(
  ctx: &Ctx<'js>,
  world_obj: &Object<'js>,
  reg_idx: usize,
  name: &str,
  factory: Persistent<Function<'static>>,
  worker_scoped: bool,
  source_label: &str,
) -> Result<(), ScriptError> {
  let factory = factory.restore(ctx).map_err(se)?;
  let (setup_tx, mut setup_rx) = tokio::sync::oneshot::channel::<()>();
  let setup_slot: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>> = Arc::new(Mutex::new(Some(setup_tx)));

  let use_name = name.to_string();
  let use_fn = Function::new(
    ctx.clone(),
    move |ctx: Ctx<'js>, value: Value<'js>| -> rquickjs::Result<Promise<'js>> {
      let (gate, resolve, _reject) = Promise::new(&ctx)?;
      let resolve_saved = Persistent::save(&ctx, resolve);
      let value_saved = Persistent::save(&ctx, value.clone());
      let world = with_test_registry(&ctx, |r| {
        if worker_scoped {
          r.worker_fixtures.insert(
            reg_idx,
            WorkerFixture {
              name: use_name.clone(),
              value: value_saved,
              gate_resolve: Some(resolve_saved),
              done_rx: None,
            },
          );
        } else if let Some(c) = r.current.as_mut() {
          c.pending.push(PendingFixture {
            name: use_name.clone(),
            gate_resolve: Some(resolve_saved),
            done_rx: None,
          });
        }
        r.current.as_ref().map(|c| c.world.clone())
      })
      .map_err(|e| rq(&e))?;
      if let Some(w) = world {
        let w = w.restore(&ctx)?;
        w.set(use_name.as_str(), value)?;
      }
      if let Some(tx) = setup_slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
      {
        let _ = tx.send(());
      }
      Ok(gate)
    },
  )
  .map_err(se)?;

  let called: rquickjs::Result<MaybePromise<'_>> = factory.call((world_obj.clone(), use_fn));
  let mp = called.catch(ctx).map_err(|e| {
    let inner = caught_to_script_error(e, source_label);
    ScriptError::internal(format!("fixture `{name}` setup failed: {}", inner.message))
  })?;
  let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
  let fut = mp.into_future::<Value<'_>>();
  let ctx2 = ctx.clone();
  let label = source_label.to_string();
  ctx.spawn(async move {
    let r = match fut.await.catch(&ctx2) {
      Ok(_) => Ok(()),
      Err(e) => Err(caught_to_script_error(e, &label).message),
    };
    let _ = done_tx.send(r);
  });

  let setup_outcome = tokio::select! {
    setup = &mut setup_rx => setup,
    done = &mut done_rx => {
      return match done {
        Ok(Err(msg)) => Err(ScriptError::internal(format!("fixture `{name}` setup failed: {msg}"))),
        _ => Err(ScriptError::internal(format!(
          "fixture `{name}` factory finished without calling use()"
        ))),
      };
    },
  };
  if setup_outcome.is_err() {
    // The setup channel closed without a send: the factory settled (or
    // was collected) without ever calling `use()` — the completion
    // channel tells which.
    return match done_rx.await {
      Ok(Err(msg)) => Err(ScriptError::internal(format!("fixture `{name}` setup failed: {msg}"))),
      _ => Err(ScriptError::internal(format!(
        "fixture `{name}` factory finished without calling use()"
      ))),
    };
  }

  // Park the completion receiver for teardown.
  with_test_registry(ctx, |r| {
    if worker_scoped {
      if let Some(w) = r.worker_fixtures.get_mut(&reg_idx) {
        w.done_rx = Some(done_rx);
      }
    } else if let Some(c) = r.current.as_mut()
      && let Some(p) = c.pending.iter_mut().rev().find(|p| p.name == name)
    {
      p.done_rx = Some(done_rx);
    }
  })
}

/// Resume suspended test-scoped fixture factories (LIFO) and await
/// their teardown halves. Returns the first teardown error.
pub(crate) async fn teardown_test_fixtures(ctx: &Ctx<'_>) -> Result<(), ScriptError> {
  let mut first_err: Option<ScriptError> = None;
  loop {
    let entry = with_test_registry(ctx, |r| r.current.as_mut().and_then(|c| c.pending.pop()))?;
    let Some(mut entry) = entry else { break };
    if let Some(resolve) = entry.gate_resolve.take() {
      let resolve = resolve.restore(ctx).map_err(se)?;
      let called: rquickjs::Result<()> = resolve.call((Value::new_undefined(ctx.clone()),));
      if let Err(e) = called.catch(ctx) {
        let msg = caught_to_script_error(e, "fixture teardown").message;
        first_err
          .get_or_insert_with(|| ScriptError::internal(format!("fixture `{}` teardown failed: {msg}", entry.name)));
        continue;
      }
    }
    if let Some(done_rx) = entry.done_rx.take()
      && let Ok(Err(msg)) = done_rx.await
    {
      first_err
        .get_or_insert_with(|| ScriptError::internal(format!("fixture `{}` teardown failed: {msg}", entry.name)));
    }
  }
  match first_err {
    Some(e) => Err(e),
    None => Ok(()),
  }
}

/// Resume every suspended worker-scoped fixture factory — the glue
/// calls this once per worker session after the run completes.
pub async fn teardown_worker_fixtures(vm: &crate::vm::VmHandle) -> Result<(), ScriptError> {
  crate::vm_with!(vm => |ctx| {
    let mut first_err: Option<ScriptError> = None;
    loop {
      // Highest registration index first: within an extend chain that is
      // reverse setup order, so an override is torn down before the
      // super it shadows (hash order would be arbitrary).
      let entry = with_test_registry(&ctx, |r| {
        let key = r.worker_fixtures.keys().copied().max();
        key.and_then(|k| r.worker_fixtures.remove(&k))
      })?;
      let Some(mut fixture) = entry else { break };
      let name = fixture.name.clone();
      if let Some(resolve) = fixture.gate_resolve.take() {
        let resolve = resolve.restore(&ctx).map_err(se)?;
        let called: rquickjs::Result<()> = resolve.call((Value::new_undefined(ctx.clone()),));
        if let Err(e) = called.catch(&ctx) {
          let msg = caught_to_script_error(e, "fixture teardown").message;
          first_err.get_or_insert_with(|| ScriptError::internal(format!("fixture `{name}` teardown failed: {msg}")));
          continue;
        }
      }
      if let Some(done_rx) = fixture.done_rx.take()
        && let Ok(Err(msg)) = done_rx.await
      {
        first_err.get_or_insert_with(|| ScriptError::internal(format!("fixture `{name}` teardown failed: {msg}")));
      }
    }
    match first_err {
      Some(e) => Err(e),
      None => Ok(()),
    }
  })
  .await?
}

/// Names of custom fixtures a test + its each-hooks request (their
/// requested lists intersected with the fixture set happens in
/// [`resolve_custom_fixtures`]).
/// What each function of this test asks for, labelled the way
/// Playwright labels it in the unknown-parameter message
/// (`common/poolBuilder.ts:66-71`: `'Test'` for the body,
/// `hook.type + ' hook'` for each hook).
fn requested_per_function(reg: &TestRegistry, spec: &RunTestSpec) -> Vec<(String, Vec<String>)> {
  let mut out = vec![(
    "Test".to_string(),
    reg.tests[spec.test_idx].requested.clone().unwrap_or_default(),
  )];
  for &h in spec.hooks_before.iter().chain(spec.hooks_after.iter()) {
    let hook = &reg.hooks[h];
    out.push((
      format!("{} hook", hook.kind),
      hook.requested.clone().unwrap_or_default(),
    ));
  }
  for &m in &spec.modifiers {
    let modifier = &reg.modifiers[m];
    out.push((
      format!("{} modifier", modifier.kind),
      modifier.requested.clone().unwrap_or_default(),
    ));
  }
  out
}

fn requested_names(reg: &TestRegistry, spec: &RunTestSpec) -> Vec<String> {
  let mut names: Vec<String> = Vec::new();
  let mut add = |req: &Option<Vec<String>>| {
    if let Some(list) = req {
      for n in list {
        if !names.contains(n) {
          names.push(n.clone());
        }
      }
    }
  };
  add(&reg.tests[spec.test_idx].requested);
  for &h in spec.hooks_before.iter().chain(spec.hooks_after.iter()) {
    add(&reg.hooks[h].requested);
  }
  for &m in &spec.modifiers {
    add(&reg.modifiers[m].requested);
  }
  names
}

/// Execute one registered test end-to-end inside the worker's VM:
/// build the fixtures object, resolve custom fixtures, run each-hooks
/// and the body, tear down, and clear the current-test slot. The
/// returned error carries the raw (bundled) stack — the glue remaps it.
pub async fn run_test(
  vm: &crate::vm::VmHandle,
  spec: RunTestSpec,
  world: TestWorldData,
  bridge: Arc<dyn TestHostBridge>,
) -> Result<(), ScriptError> {
  let route_vm = vm.clone();
  crate::vm_with!(vm => |ctx| {
    let test_count = with_test_registry(&ctx, |r| r.tests.len())?;
    if spec.test_idx >= test_count {
      return Err(ScriptError::internal(format!(
        "test index {} out of range ({} registered)",
        spec.test_idx, test_count
      )));
    }
    set_current_test(&ctx, &route_vm, &world, Arc::clone(&bridge))?;

    let result = drive_current_test(&ctx, &spec, &world).await;

    let teardown = teardown_test_fixtures(&ctx).await;
    let _ = with_test_registry(&ctx, |r| r.current = None);
    match (result, teardown) {
      (Err(e), _) | (Ok(()), Err(e)) => Err(e),
      (Ok(()), Ok(())) => Ok(()),
    }
  })
  .await?
}

async fn drive_current_test(ctx: &Ctx<'_>, spec: &RunTestSpec, world: &TestWorldData) -> Result<(), ScriptError> {
  let (world_obj, info_obj) = with_test_registry(ctx, |r| {
    r.current
      .as_ref()
      .map(|c| (c.world.clone(), c.test_info.clone()))
      .ok_or_else(|| ScriptError::internal("no current test".to_string()))
  })??;
  let world_obj = world_obj.restore(ctx).map_err(se)?;
  let info_obj = info_obj.restore(ctx).map_err(se)?;

  let (fixture_set, each_arg, func) = with_test_registry(ctx, |r| {
    let t = &r.tests[spec.test_idx];
    (t.fixture_set, t.each_arg.clone(), t.func.clone())
  })?;
  let per_function = with_test_registry(ctx, |r| requested_per_function(r, spec))?;
  validate_requested_names(ctx, &world_obj, fixture_set, &per_function)?;
  let custom = with_test_registry(ctx, |r| requested_names(r, spec))?;
  resolve_custom_fixtures(
    ctx,
    &world_obj,
    fixture_set,
    &custom,
    &world.use_options,
    &spec.source_label,
  )
  .await?;

  let mut first_err: Option<ScriptError> = None;

  // Modifiers first, then hooks — Playwright's order
  // (`worker/workerMain.ts:556`). A `skip` here must stop the test
  // before its beforeEach hooks run, but afterEach still runs, so the
  // failure travels the same road a hook failure does.
  for &m in &spec.modifiers {
    match apply_suite_modifier(ctx, m, &world_obj, &info_obj, &spec.source_label).await {
      Ok(()) => {},
      Err(e) => {
        first_err = Some(e);
        break;
      },
    }
  }

  for &h in &spec.hooks_before {
    if let Err(e) = invoke_hook_fn(ctx, h, &world_obj, &info_obj, &spec.source_label).await {
      first_err = Some(e);
      break;
    }
  }

  if first_err.is_none() {
    let func = func.restore(ctx).map_err(se)?;
    let second: Value<'_> = match &each_arg {
      Some(row) => crate::bindings::convert::json_to_js(ctx, row).map_err(se)?,
      None => info_obj.clone().into_value(),
    };
    let called: rquickjs::Result<MaybePromise<'_>> = func.call((world_obj.clone(), second));
    match called.catch(ctx) {
      Ok(mp) => {
        if let Err(e) = mp.into_future::<Value<'_>>().await.catch(ctx) {
          first_err = Some(caught_to_script_error(e, &spec.source_label));
        }
      },
      Err(e) => first_err = Some(caught_to_script_error(e, &spec.source_label)),
    }
  }

  // afterEach hooks always run (Playwright); the first error wins.
  for &h in &spec.hooks_after {
    if let Err(e) = invoke_hook_fn(ctx, h, &world_obj, &info_obj, &spec.source_label).await {
      first_err.get_or_insert(e);
    }
  }

  match first_err {
    Some(e) => Err(e),
    None => Ok(()),
  }
}

/// Evaluate one suite modifier with the test's fixtures and apply it.
///
/// Playwright wraps the callback so its result becomes
/// `testInfo._modifier(type, location, [!!result, description])`
/// (`worker/workerMain.ts:545-548`) — the truthy return is the
/// condition, and the description came from the registration. A
/// `skip`/`fixme` therefore aborts the test exactly as the runtime form
/// does, through the sentinel the worker recognises.
async fn apply_suite_modifier<'js>(
  ctx: &Ctx<'js>,
  modifier_idx: usize,
  world_obj: &Object<'js>,
  info_obj: &Object<'js>,
  source_label: &str,
) -> Result<(), ScriptError> {
  let (kind, description, func) = with_test_registry(ctx, |r| {
    r.modifiers
      .get(modifier_idx)
      .map(|m| (m.kind.clone(), m.description.clone(), m.func.clone()))
      .ok_or_else(|| ScriptError::internal(format!("modifier index {modifier_idx} out of range")))
  })??;
  let func = func.restore(ctx).map_err(se)?;
  let called: rquickjs::Result<MaybePromise<'js>> = func.call((world_obj.clone(), info_obj.clone()));
  let mp = called.catch(ctx).map_err(|e| caught_to_script_error(e, source_label))?;
  let result: Value<'js> = mp
    .into_future()
    .await
    .catch(ctx)
    .map_err(|e| caught_to_script_error(e, source_label))?;
  if !truthy(&result) {
    return Ok(());
  }
  let bridge = with_test_registry(ctx, |r| r.current.as_ref().map(|c| Arc::clone(&c.bridge)))?
    .ok_or_else(|| ScriptError::internal("no current test for a suite modifier".to_string()))?;
  match kind.as_str() {
    "skip" | "fixme" => {
      bridge.set_skip(description.clone());
      Err(ScriptError::internal(format!(
        "{TEST_SKIP_SENTINEL}{}",
        description.unwrap_or_default()
      )))
    },
    "fail" => {
      bridge.set_expected_failure();
      Ok(())
    },
    "slow" => {
      bridge.set_slow();
      Ok(())
    },
    other => Err(ScriptError::internal(format!("unknown suite modifier `{other}`"))),
  }
}

async fn invoke_hook_fn<'js>(
  ctx: &Ctx<'js>,
  hook_idx: usize,
  world_obj: &Object<'js>,
  info_obj: &Object<'js>,
  source_label: &str,
) -> Result<(), ScriptError> {
  let func = with_test_registry(ctx, |r| {
    r.hooks
      .get(hook_idx)
      .map(|h| h.func.clone())
      .ok_or_else(|| ScriptError::internal(format!("hook index {hook_idx} out of range")))
  })??;
  let func = func.restore(ctx).map_err(se)?;
  let called: rquickjs::Result<MaybePromise<'_>> = func.call((world_obj.clone(), info_obj.clone()));
  let mp = called.catch(ctx).map_err(|e| caught_to_script_error(e, source_label))?;
  mp.into_future::<Value<'_>>()
    .await
    .catch(ctx)
    .map(|_| ())
    .map_err(|e| caught_to_script_error(e, source_label))
}

/// Execute one `beforeAll`/`afterAll` hook with its own fixtures
/// object and current-test slot (so `test.info()`/steps work inside).
pub async fn run_standalone_hook(
  vm: &crate::vm::VmHandle,
  hook_idx: usize,
  world: TestWorldData,
  bridge: Arc<dyn TestHostBridge>,
  source_label: String,
) -> Result<(), ScriptError> {
  let route_vm = vm.clone();
  crate::vm_with!(vm => |ctx| {
    set_current_test(&ctx, &route_vm, &world, Arc::clone(&bridge))?;
    let (world_obj, info_obj) = with_test_registry(&ctx, |r| {
      r.current
        .as_ref()
        .map(|c| (c.world.clone(), c.test_info.clone()))
        .ok_or_else(|| ScriptError::internal("no current test".to_string()))
    })??;
    let world_obj = world_obj.restore(&ctx).map_err(se)?;
    let info_obj = info_obj.restore(&ctx).map_err(se)?;

    let (requested, fixture_set, kind) = with_test_registry(&ctx, |r| {
      let hook = r.hooks.get(hook_idx);
      let req = hook.and_then(|h| h.requested.clone()).unwrap_or_default();
      // The chain the hook was registered on, so a `beforeAll` declared
      // on an extended object can ask for that chain's fixtures.
      (
        req,
        hook.map_or(0, |h| h.fixture_set),
        hook.map_or_else(String::new, |h| h.kind.clone()),
      )
    })?;
    validate_requested_names(
      &ctx,
      &world_obj,
      fixture_set,
      &[(format!("{kind} hook"), requested.clone())],
    )?;
    let result = async {
      resolve_custom_fixtures(&ctx, &world_obj, fixture_set, &requested, &world.use_options, &source_label).await?;
      invoke_hook_fn(&ctx, hook_idx, &world_obj, &info_obj, &source_label).await
    }
    .await;

    let teardown = teardown_test_fixtures(&ctx).await;
    let _ = with_test_registry(&ctx, |r| r.current = None);
    match (result, teardown) {
      (Err(e), _) | (Ok(()), Err(e)) => Err(e),
      (Ok(()), Ok(())) => Ok(()),
    }
  })
  .await?
}

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
  /// the same input [`resolve_custom_fixtures`] builds from the live
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
pub async fn collect_tests(vm: &crate::vm::VmHandle) -> Result<CollectedTests, ScriptError> {
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
  use super::{fixture_step_title, interpolate_title, parse_destructured_keys};

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

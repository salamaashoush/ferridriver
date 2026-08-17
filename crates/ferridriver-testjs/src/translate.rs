//! Translate a [`CollectedTests`] snapshot into a core [`TestPlan`]:
//! remap registration locations to source files, resolve suite chains
//! (mode, annotations, `use` bags, hook lists), and wrap each body in
//! a `TestFn` that dispatches into the per-worker `QuickJS` session.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ferridriver_script::{CollectedAnnotation, CollectedTests, CompiledBundle};
use ferridriver_test::config::TestConfig;
use ferridriver_test::model::{
  ExpectedStatus, HookDef, HookKind, SuiteDef, SuiteMode, TestAnnotation, TestCase, TestFailure, TestFn, TestId,
  TestInfo, TestPlan, TestPlanBuilder,
};

use crate::TsTestSource;
use ferridriver_test::host::{InfoBridge, static_annotation_pairs};
use ferridriver_test::host::{RunTestSpec, TestInfoData, TestWorldData};

/// Resolved-per-suite chain data (ancestors included).
struct SuiteChain {
  /// `None` for file scope.
  path: Vec<String>,
  mode: Option<SuiteMode>,
  annotations: Vec<CollectedAnnotation>,
  use_options: Option<serde_json::Value>,
  retries: Option<u32>,
  timeout_ms: Option<u64>,
}

fn remap_file(bundle: &CompiledBundle, cwd: &Path, line: u32, col: u32) -> Option<(String, u32)> {
  let (src, src_line, _src_col) = bundle.remap(line, col)?;
  let abs = resolve_source(cwd, &src);
  let rel = abs
    .strip_prefix(cwd)
    .map_or_else(|_| abs.display().to_string(), |r| r.display().to_string());
  Some((rel, src_line))
}

pub(crate) use ferridriver_script::resolve_source;

fn merge_bag(base: &mut Option<serde_json::Value>, incoming: &serde_json::Value) {
  match base {
    Some(serde_json::Value::Object(b)) => {
      if let serde_json::Value::Object(inc) = incoming {
        for (k, v) in inc {
          b.insert(k.clone(), v.clone());
        }
      }
    },
    _ => *base = Some(incoming.clone()),
  }
}

fn lower_annotation(a: &CollectedAnnotation) -> Option<TestAnnotation> {
  match a.kind.as_str() {
    "skip" => Some(TestAnnotation::Skip {
      reason: a.value.clone(),
      condition: None,
    }),
    "fixme" => Some(TestAnnotation::Fixme {
      reason: a.value.clone(),
      condition: None,
    }),
    "slow" => Some(TestAnnotation::Slow {
      reason: a.value.clone(),
      condition: None,
    }),
    "only" => Some(TestAnnotation::Only),
    "tag" => a.value.clone().map(TestAnnotation::Tag),
    "info" => Some(TestAnnotation::Info {
      type_name: a.value.clone().unwrap_or_default(),
      description: a.description.clone().unwrap_or_default(),
    }),
    // Registration-time `fail` flips ExpectedStatus instead.
    _ => None,
  }
}

/// Pool fixture names for a JS-side requested name; scalars and custom
/// fixtures have no pool entry of their own.
fn pool_fixtures_for(name: &str) -> &'static [&'static str] {
  match name {
    "page" => &["page"],
    "context" => &["context"],
    "request" => &["request"],
    "browser" => &["browser"],
    "testInfo" => &["test_info"],
    _ => &[],
  }
}

const DEFAULT_REQUESTS: &[&str] = &["browser", "context", "page", "test_info", "request"];

/// Compute the core fixture requests for a test: the union over the
/// body's and each-hooks' destructured names plus the transitive deps
/// of every custom fixture involved. `None` anywhere ⇒ the
/// conservative default (everything).
fn fixture_requests(collected: &CollectedTests, test_idx: usize, hook_idxs: &[usize]) -> Vec<String> {
  let test = &collected.tests[test_idx];
  let mut names: Vec<String> = Vec::new();
  let mut conservative = false;
  let mut add_requested = |req: &Option<Vec<String>>, conservative: &mut bool| match req {
    Some(list) => {
      for n in list {
        if !names.contains(n) {
          names.push(n.clone());
        }
      }
    },
    None => *conservative = true,
  };
  add_requested(&test.requested, &mut conservative);
  for &h in hook_idxs {
    add_requested(&collected.hooks[h].requested, &mut conservative);
  }
  if conservative {
    return DEFAULT_REQUESTS.iter().map(|s| (*s).to_string()).collect();
  }

  // Every custom fixture the test pulls in (auto ones included), then
  // the deps of those that resolve to no registration — those are the
  // ones the pool has to provide. Resolution is super-scoped, so a
  // `page` override depending on `{ page }` asks the pool for the
  // built-in page rather than looping.
  let slots = collected.fixture_slots(test.fixture_set);
  let Ok(needed) = ferridriver_test::fixture_graph::resolution_order(&slots, &names, &|_| true) else {
    // Malformed graph (cycle / self-reference with no base). Request
    // everything so the VM-side resolver is the one that reports it.
    return DEFAULT_REQUESTS.iter().map(|s| (*s).to_string()).collect();
  };
  for pos in needed {
    for dep in &slots[pos].deps {
      if ferridriver_test::fixture_graph::resolve_dep(&slots, dep, Some(pos)).is_none() && !names.contains(dep) {
        names.push(dep.clone());
      }
    }
  }

  let mut out: Vec<String> = Vec::new();
  // The bridge and testInfo always need test_info.
  out.push("test_info".to_string());
  for n in &names {
    for pool in pool_fixtures_for(n) {
      if !out.contains(&(*pool).to_string()) {
        out.push((*pool).to_string());
      }
    }
  }
  out
}

/// Suite chain (self + ancestors, outer→inner) for a collected suite.
fn chain_for(collected: &CollectedTests, suite: Option<usize>) -> SuiteChain {
  let mut idxs: Vec<usize> = Vec::new();
  let mut cur = suite;
  while let Some(i) = cur {
    idxs.push(i);
    cur = collected.suites[i].parent;
  }
  idxs.reverse(); // outer → inner

  let mut chain = SuiteChain {
    path: Vec::new(),
    mode: None,
    annotations: Vec::new(),
    use_options: None,
    retries: None,
    timeout_ms: None,
  };
  for &i in &idxs {
    let s = &collected.suites[i];
    chain.path.push(s.name.clone());
    if let Some(mode) = &s.mode {
      chain.mode = Some(match mode.as_str() {
        "serial" => SuiteMode::Serial,
        _ => SuiteMode::Parallel,
      });
    }
    chain.annotations.extend(s.annotations.iter().cloned());
    if let Some(bag) = &s.use_options {
      merge_bag(&mut chain.use_options, bag);
    }
    if s.retries.is_some() {
      chain.retries = s.retries;
    }
    if s.timeout_ms.is_some() {
      chain.timeout_ms = s.timeout_ms;
    }
  }
  chain
}

/// Hook indices applying to a test: file-root hooks (same file) then
/// suite-chain hooks outer→inner for `beforeEach`; reversed for
/// `afterEach`.
fn hooks_for(
  collected: &CollectedTests,
  bundle: &CompiledBundle,
  cwd: &Path,
  test_file: &str,
  suite: Option<usize>,
) -> (Vec<usize>, Vec<usize>) {
  // Ancestor chain, outer → inner (None = root scope).
  let mut scopes: Vec<Option<usize>> = vec![None];
  let mut idxs: Vec<usize> = Vec::new();
  let mut cur = suite;
  while let Some(i) = cur {
    idxs.push(i);
    cur = collected.suites[i].parent;
  }
  idxs.reverse();
  scopes.extend(idxs.into_iter().map(Some));

  let mut before: Vec<usize> = Vec::new();
  let mut after: Vec<usize> = Vec::new();
  for scope in &scopes {
    for (h_idx, h) in collected.hooks.iter().enumerate() {
      if h.suite != *scope {
        continue;
      }
      // Root hooks apply per FILE: a top-level beforeEach in a.test.ts
      // must not run for b.test.ts's tests bundled alongside it.
      if scope.is_none() {
        let hook_file = remap_file(bundle, cwd, h.line, h.col).map(|(f, _)| f);
        if hook_file.as_deref() != Some(test_file) {
          continue;
        }
      }
      match h.kind.as_str() {
        "beforeEach" => before.push(h_idx),
        "afterEach" => after.push(h_idx),
        _ => {},
      }
    }
  }
  after.reverse(); // inner → outer
  (before, after)
}

/// File-scope `test.use` bags + `describe.configure` for one file.
struct FileScope {
  use_options: Option<serde_json::Value>,
  mode: Option<SuiteMode>,
  retries: Option<u32>,
  timeout_ms: Option<u64>,
}

fn file_scope(collected: &CollectedTests, bundle: &CompiledBundle, cwd: &Path, file: &str) -> FileScope {
  let mut out = FileScope {
    use_options: None,
    mode: None,
    retries: None,
    timeout_ms: None,
  };
  for u in &collected.file_use {
    if remap_file(bundle, cwd, u.line, u.col).map(|(f, _)| f).as_deref() == Some(file) {
      merge_bag(&mut out.use_options, &u.options);
    }
  }
  for c in &collected.file_configure {
    if remap_file(bundle, cwd, c.line, c.col).map(|(f, _)| f).as_deref() == Some(file) {
      if let Some(mode) = &c.mode {
        out.mode = Some(match mode.as_str() {
          "serial" => SuiteMode::Serial,
          _ => SuiteMode::Parallel,
        });
      }
      if c.retries.is_some() {
        out.retries = c.retries;
      }
      if c.timeout_ms.is_some() {
        out.timeout_ms = c.timeout_ms;
      }
    }
  }
  out
}

/// Everything a test body closure needs, resolved at translation time.
struct TestFnParams {
  test_idx: usize,
  sessions: Arc<crate::SessionPool>,
  bundle: Arc<CompiledBundle>,
  cwd: Arc<std::path::PathBuf>,
  world_use: Arc<serde_json::Value>,
  static_annotations: Arc<Vec<(String, Option<String>)>>,
  tags: Arc<Vec<String>>,
  title_path: Arc<Vec<String>>,
  file: Arc<String>,
  title: Arc<String>,
  browser_config: ferridriver_test::config::BrowserConfig,
  base_url: Option<String>,
  expected_status: ExpectedStatus,
  requests: Vec<String>,
  hooks_before: Vec<usize>,
  hooks_after: Vec<usize>,
}

/// Resolve the pool fixtures the body requested into a [`TestWorldData`].
async fn build_world_data(
  pool: &ferridriver_test::FixturePool,
  test_info: &Arc<TestInfo>,
  p: &TestFnParams,
) -> Result<TestWorldData, TestFailure> {
  let mut world = ferridriver_test::host::world_data(ferridriver_test::host::WorldMeta {
    test_info,
    title: p.title.as_str(),
    title_path: &p.title_path,
    file: p.file.as_str(),
    line: u32::try_from(test_info.test_id.line.unwrap_or(0)).unwrap_or(0),
    tags: &p.tags,
    expected_status: p.expected_status,
    browser_config: &p.browser_config,
    base_url: p.base_url.as_deref(),
    use_options: (*p.world_use).clone(),
  });
  for name in &p.requests {
    match name.as_str() {
      "page" => {
        world.page = Some(
          pool
            .get("page")
            .await
            .map_err(|e| TestFailure::wrap("fixture 'page' failed", e))?,
        );
      },
      "context" => {
        world.context = Some(
          pool
            .get("context")
            .await
            .map_err(|e| TestFailure::wrap("fixture 'context' failed", e))?,
        );
      },
      "request" => {
        world.request = Some(
          pool
            .get("request")
            .await
            .map_err(|e| TestFailure::wrap("fixture 'request' failed", e))?,
        );
      },
      "browser" => {
        world.browser = Some(
          pool
            .get("browser")
            .await
            .map_err(|e| TestFailure::wrap("fixture 'browser' failed", e))?,
        );
      },
      _ => {},
    }
  }
  Ok(world)
}

fn make_test_fn(p: TestFnParams) -> TestFn {
  let p = Arc::new(p);
  Arc::new(move |pool| {
    let p = Arc::clone(&p);
    let spec = RunTestSpec {
      test_idx: p.test_idx,
      hooks_before: p.hooks_before.clone(),
      hooks_after: p.hooks_after.clone(),
      source_label: p.file.as_str().to_string(),
    };
    Box::pin(async move {
      let test_info: Arc<TestInfo> = pool
        .get("test_info")
        .await
        .map_err(|e| TestFailure::wrap("fixture 'test_info' failed", e))?;

      let session = p
        .sessions
        .get(test_info.worker_index)
        .await
        .map_err(|e| TestFailure::from(format!("test session load failed: {e}")))?;

      let modifiers = Arc::new(ferridriver_test::model::TestModifiers::default());
      pool.inject("__test_modifiers", Arc::clone(&modifiers));

      let world = build_world_data(&pool, &test_info, &p).await?;

      let base_timeout = test_info.timeout;
      let bridge = Arc::new(
        InfoBridge::new(
          Arc::clone(&test_info),
          modifiers,
          Arc::new(session.session().deadline()),
          Arc::new(ferridriver_script::BundleSourceMap::new(
            Arc::clone(&p.bundle),
            Arc::clone(&p.cwd),
          )),
          Arc::clone(&p.cwd),
          base_timeout,
          (*p.static_annotations).clone(),
        )
        // A spec names every `describe` separately, so a step's own title
        // path continues that rather than the coarser one a `TestId` can
        // rebuild from its joined suite id.
        .with_title_path((*p.title_path).clone()),
      );

      // What the body prints is this test's output, from here until the
      // binding drops.
      let console = session.capture_console(Arc::clone(&test_info));
      session.session().arm_deadline(base_timeout);
      let result = ferridriver_script::run_test(&session.vm_handle(), spec, world, bridge.clone() as _).await;
      session.session().disarm_deadline();
      drop(console);
      bridge.flush().await;

      match result {
        Ok(()) => Ok(()),
        Err(e) => Err(TestFailure {
          message: p.bundle.format_error(&e),
          stack: e.stack.clone(),
          diff: None,
          screenshot: None,
        }),
      }
    })
  })
}

/// Shared translation inputs.
struct PlanCx<'a> {
  source: &'a TsTestSource,
  config: &'a TestConfig,
  cwd: &'a Path,
  cwd_arc: Arc<std::path::PathBuf>,
  config_use: serde_json::Value,
  sessions: Arc<crate::SessionPool>,
}

/// Annotation/use-bag/timing metadata resolved for one test.
struct TestMeta {
  annotations: Vec<TestAnnotation>,
  expected_status: ExpectedStatus,
  use_bag: Option<serde_json::Value>,
  world_use: serde_json::Value,
  /// Only what the spec itself asked for. A test that names no timeout
  /// leaves this unset so the runner applies the config's — the run's
  /// config, which is not always the one discovery was done under (a UI
  /// run sends its own `timeout`).
  timeout_ms: Option<u64>,
  retries: Option<u32>,
}

/// `@word` tokens in a title are tags, the way `{ tag: [...] }` is
/// (Playwright: `testType.ts` reads both), so `--tag @smoke` and the
/// UI's tag filter see a test titled `logs in @smoke`.
fn title_tags(titles: &[String]) -> Vec<TestAnnotation> {
  titles
    .iter()
    .flat_map(|title| title.split_whitespace())
    .filter(|word| word.len() > 1 && word.starts_with('@'))
    .map(|tag| TestAnnotation::Tag(tag.to_string()))
    .collect()
}

fn resolve_meta(cx: &PlanCx<'_>, chain: &SuiteChain, fscope: &FileScope, test_idx: usize) -> TestMeta {
  let test = &cx.source.collected.tests[test_idx];
  let mut annotations: Vec<TestAnnotation> = Vec::new();
  let mut expected_status = ExpectedStatus::Pass;
  let mut seen_tags: Vec<String> = Vec::new();
  for tag in title_tags(&chain.path)
    .into_iter()
    .chain(title_tags(std::slice::from_ref(&test.title)))
  {
    if let TestAnnotation::Tag(name) = &tag
      && !seen_tags.contains(name)
    {
      seen_tags.push(name.clone());
      annotations.push(tag);
    }
  }
  // Suite chain first (skip/fixme/only propagate), then the test's
  // own. Registration-time `fail` flips the expectation.
  for a in chain.annotations.iter().chain(test.annotations.iter()) {
    if a.kind == "fail" {
      expected_status = ExpectedStatus::Fail;
      continue;
    }
    if let Some(lowered) = lower_annotation(a) {
      annotations.push(lowered);
    }
  }

  // Effective use bag: config ⊕ file ⊕ suite chain (inner wins).
  let mut use_bag: Option<serde_json::Value> = None;
  if let Some(f) = &fscope.use_options {
    merge_bag(&mut use_bag, f);
  }
  if let Some(sb) = &chain.use_options {
    merge_bag(&mut use_bag, sb);
  }
  let mut world_use = if cx.config_use.is_object() {
    cx.config_use.clone()
  } else {
    serde_json::json!({})
  };
  if let Some(bag) = &use_bag
    && let (serde_json::Value::Object(w), serde_json::Value::Object(b)) = (&mut world_use, bag)
  {
    for (k, v) in b {
      w.insert(k.clone(), v.clone());
    }
  }

  TestMeta {
    annotations,
    expected_status,
    use_bag,
    world_use,
    timeout_ms: test.timeout_ms.or(chain.timeout_ms).or(fscope.timeout_ms),
    retries: test.retries.or(chain.retries).or(fscope.retries),
  }
}

/// One collected test lowered to a core [`TestCase`], with its suite
/// registered on the builder.
fn lower_test(
  cx: &PlanCx<'_>,
  test_idx: usize,
  builder: &mut TestPlanBuilder,
  registered_suites: &mut Vec<String>,
) -> anyhow::Result<TestCase> {
  let collected = &cx.source.collected;
  let bundle = &cx.source.bundle;
  let test = &collected.tests[test_idx];
  let (file, line) = remap_file(bundle, cx.cwd, test.line, test.col)
    .ok_or_else(|| anyhow::anyhow!("test `{}` has no source-mapped location", test.title))?;
  let fscope = file_scope(collected, bundle, cx.cwd, &file);
  let chain = chain_for(collected, test.suite);

  // Suite identity: one core suite per (file, describe path).
  let (suite_id, suite_name, suite_mode) = if chain.path.is_empty() {
    (file.clone(), file.clone(), fscope.mode.unwrap_or_default())
  } else {
    let name = chain.path.join(" > ");
    (
      format!("{file}::{name}"),
      name,
      chain.mode.or(fscope.mode).unwrap_or_default(),
    )
  };
  if !registered_suites.contains(&suite_id) {
    registered_suites.push(suite_id.clone());
    builder.add_suite(SuiteDef {
      id: suite_id.clone(),
      name: suite_name,
      file: file.clone(),
      mode: suite_mode,
    });
  }

  let meta = resolve_meta(cx, &chain, &fscope, test_idx);
  let (hooks_before, hooks_after) = hooks_for(collected, bundle, cx.cwd, &file, test.suite);
  let requests = fixture_requests(
    collected,
    test_idx,
    &[hooks_before.as_slice(), hooks_after.as_slice()].concat(),
  );

  let id = TestId {
    file: file.clone(),
    suite: Some(suite_id),
    name: test.title.clone(),
    line: Some(line as usize),
    column: None,
  };
  let title_path: Vec<String> = {
    let mut path = vec![file.clone()];
    path.extend(chain.path.iter().cloned());
    path.push(test.title.clone());
    path
  };
  let tags: Vec<String> = meta
    .annotations
    .iter()
    .filter_map(|a| match a {
      TestAnnotation::Tag(t) => Some(t.clone()),
      _ => None,
    })
    .collect();

  let test_fn = make_test_fn(TestFnParams {
    test_idx,
    sessions: Arc::clone(&cx.sessions),
    bundle: Arc::clone(bundle),
    cwd: Arc::clone(&cx.cwd_arc),
    world_use: Arc::new(meta.world_use),
    static_annotations: Arc::new(static_annotation_pairs(&meta.annotations)),
    tags: Arc::new(tags),
    title_path: Arc::new(title_path),
    file: Arc::new(file),
    title: Arc::new(test.title.clone()),
    browser_config: cx.config.browser.clone(),
    base_url: cx.config.base_url.clone(),
    expected_status: meta.expected_status,
    requests: requests.clone(),
    hooks_before,
    hooks_after,
  });

  Ok(TestCase {
    id,
    test_fn,
    fixture_requests: requests,
    annotations: meta.annotations,
    timeout: meta.timeout_ms.map(Duration::from_millis),
    retries: meta.retries,
    expected_status: meta.expected_status,
    use_options: meta.use_bag,
  })
}

/// Register `beforeAll`/`afterAll` hooks as suite-scoped core hooks
/// running standalone (own fixtures object, no test context).
fn lower_all_hooks(
  source: &TsTestSource,
  config: &TestConfig,
  cwd: &Path,
  cwd_arc: &Arc<std::path::PathBuf>,
  sessions: &Arc<crate::SessionPool>,
  builder: &mut TestPlanBuilder,
) {
  let collected = &source.collected;
  let bundle = &source.bundle;
  for (h_idx, h) in collected.hooks.iter().enumerate() {
    let kind = match h.kind.as_str() {
      "beforeAll" | "afterAll" => h.kind.clone(),
      _ => continue,
    };
    let Some((file, _)) = remap_file(bundle, cwd, h.line, h.col) else {
      continue;
    };
    let suite_id = match h.suite {
      Some(sidx) => {
        let chain = chain_for(collected, Some(sidx));
        format!("{file}::{}", chain.path.join(" > "))
      },
      None => file.clone(),
    };
    let bundle_fn = Arc::clone(bundle);
    let cwd_fn = Arc::clone(cwd_arc);
    let sessions_fn = Arc::clone(sessions);
    let browser_config = config.browser.clone();
    let hook_base_url = config.base_url.clone();
    let label = file.clone();
    let hook_fn: ferridriver_test::model::SuiteHookFn = Arc::new(move |pool| {
      let bundle = Arc::clone(&bundle_fn);
      let cwd = Arc::clone(&cwd_fn);
      let sessions = Arc::clone(&sessions_fn);
      let browser_config = browser_config.clone();
      let base_url = hook_base_url.clone();
      let label = label.clone();
      Box::pin(async move {
        let session = sessions
          .get(0)
          .await
          .map_err(|e| TestFailure::from(format!("test session load failed: {e}")))?;
        let test_info = Arc::new(TestInfo::new_anonymous());
        let modifiers = Arc::new(ferridriver_test::model::TestModifiers::default());
        let browser = pool.get("browser").await.ok();
        // The suite pool carries the worker's per-project test_info
        // (config_snapshot = merged project config); the captured
        // `browser_config` is the root config fallback.
        let effective_browser = pool
          .try_get_cached::<TestInfo>("test_info")
          .and_then(|ti| ti.config_snapshot.as_ref().map(|cfg| cfg.browser.clone()))
          .unwrap_or(browser_config);
        let world = TestWorldData {
          page: None,
          context: None,
          request: None,
          browser,
          browser_name: effective_browser.browser.clone(),
          headless: effective_browser.headless,
          is_mobile: false,
          has_touch: false,
          base_url,
          use_options: serde_json::json!({}),
          info: TestInfoData {
            title: "beforeAll/afterAll hook".to_string(),
            timeout_ms: 30_000,
            expected_status: "passed".to_string(),
            ..TestInfoData::default()
          },
        };
        let bridge = Arc::new(InfoBridge::new(
          test_info,
          modifiers,
          Arc::new(session.session().deadline()),
          Arc::new(ferridriver_script::BundleSourceMap::new(
            Arc::clone(&bundle),
            cwd.clone(),
          )),
          cwd.clone(),
          Duration::from_secs(30),
          Vec::new(),
        ));
        ferridriver_script::run_standalone_hook(&session.vm_handle(), h_idx, world, bridge as _, label.clone())
          .await
          .map_err(|e| TestFailure {
            message: bundle.format_error(&e),
            stack: e.stack.clone(),
            diff: None,
            screenshot: None,
          })
      })
    });
    builder.add_hook(HookDef {
      suite_id,
      kind: match kind.as_str() {
        "beforeAll" => HookKind::BeforeAll(hook_fn),
        _ => HookKind::AfterAll(hook_fn),
      },
    });
  }
}

/// Build the [`TestPlan`] for a loaded test source.
///
/// # Errors
///
/// Fails when a registration has no source-mapped location (a bundle
/// without a source map).
pub fn translate_tests(
  source: &TsTestSource,
  config: &TestConfig,
  cwd: &Path,
  sessions: &Arc<crate::SessionPool>,
) -> anyhow::Result<TestPlan> {
  let cx = PlanCx {
    source,
    config,
    cwd,
    cwd_arc: Arc::new(cwd.to_path_buf()),
    // Effective config `use` bag every test starts from.
    config_use: serde_json::to_value(&config.browser.use_options).unwrap_or(serde_json::Value::Null),
    sessions: Arc::clone(sessions),
  };
  let mut builder = TestPlanBuilder::new();
  let mut registered_suites: Vec<String> = Vec::new();
  for test_idx in 0..source.collected.tests.len() {
    let case = lower_test(&cx, test_idx, &mut builder, &mut registered_suites)?;
    builder.add_test(case);
  }
  lower_all_hooks(source, config, cwd, &cx.cwd_arc, &cx.sessions, &mut builder);
  Ok(builder.build())
}

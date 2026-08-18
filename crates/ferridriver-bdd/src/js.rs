//! JavaScript step definitions driven by the shared QuickJS engine.
//!
//! `ferridriver-script` owns the VM and every binding (`page`,
//! `locator`, ...); this module loads cucumber-js-shaped `.js` step
//! files into that VM as ES modules (so shared `import './helpers.js'`
//! works) and drives them from the Rust BDD core
//! (`feature`/`scenario`/`filter`/`registry`). No matching, outline
//! expansion or tag logic lives here.
//!
//! A scenario runs against one object that is both the cucumber World
//! and the test's fixture bag: this module decides which `test.extend`
//! chain it resolves from (the union of what its matched steps and
//! applicable hooks were bound to, via
//! `ferridriver_test::fixture_graph::dominant_fixture_set`) and which
//! names to set up (what those bodies destructured), then hands both to
//! `begin_scenario`. The step registry is per-VM JavaScript state, so
//! one engine session is created per `ferridriver-test` worker:
//! scenarios run in parallel across workers, each VM driving its own
//! scenarios, and a `{ scope: "worker" }` fixture is shared by every
//! scenario that worker runs.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use ferridriver_script::{
  CompiledBundle, HookArg, InMemoryVars, JsArg, PathSandbox, RunContext, ScenarioSpec, ScriptAttachment,
  ScriptEngineConfig, Session, StepOutcome, VmHandle, begin_scenario, bundle_and_compile, collect_registry,
  drain_attachments, end_scenario, eval_bundle, invoke_hook, invoke_step, is_source_file, set_hook_world,
  walk_source_files,
};
use ferridriver_test::FixturePool;
use ferridriver_test::fixture_graph::dominant_fixture_set;
use ferridriver_test::host::TestWorldData;
use ferridriver_test::host::{InfoBridge, static_annotation_pairs};
use ferridriver_test::model::{AttachmentBody, StepCategory, TestFixtures, TestInfo};
use tokio::sync::OnceCell;

use crate::feature::FeatureSet;
use crate::filter::TagExpression;
use crate::param_type::CustomParamType;
use crate::registry::StepRegistry;
use crate::scenario::ScenarioExecution;
use crate::step::{MatchError, StepError, StepFixtures, StepHandler, StepKind, StepLocation, StepMatch, StepParam};
use crate::world::BrowserWorld;

const JS_STEP_LOCATION: &str = "<js-step>";

const DEFAULT_STEP_GLOBS: &[&str] = &[
  "steps/**/*.js",
  "steps/**/*.ts",
  "step_definitions/**/*.js",
  "step_definitions/**/*.ts",
];

/// Status of one step in a JS-driven scenario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsStepStatus {
  Passed,
  Failed(String),
  Skipped,
  Undefined(String),
  Pending,
}

/// Result of one step.
#[derive(Debug, Clone)]
pub struct JsStepResult {
  pub keyword: String,
  pub text: String,
  pub line: usize,
  pub duration: Duration,
  pub status: JsStepStatus,
}

/// Result of one scenario.
#[derive(Debug, Clone)]
pub struct JsScenarioResult {
  pub name: String,
  pub tags: Vec<String>,
  pub steps: Vec<JsStepResult>,
  pub passed: bool,
}

/// One registered JS hook, with the fixture chain and names its body
/// resolves from (`bindSteps(test)` registrations; the ambient
/// `Before`/`After` resolve from the base chain).
struct HookEntry {
  idx: usize,
  kind: String,
  tags: Option<TagExpression>,
  fixture_set: usize,
  requested: Option<Vec<String>>,
}

/// A loaded JS step suite bound to one shared-engine session (one per
/// `ferridriver-test` worker).
pub struct JsBddSession {
  session: Session,
  registry: Arc<StepRegistry>,
  hooks: Vec<HookEntry>,
  bundle: Arc<CompiledBundle>,
  cwd: Arc<PathBuf>,
  /// The `test.extend` chains the step bundle registered, indexed by
  /// fixture set — what a scenario's chain is picked out of.
  fixture_sets: Vec<Vec<usize>>,
  /// Cucumber `--world-parameters` exposed to every scenario as
  /// `this.parameters`.
  world_parameters: serde_json::Value,
}

/// Everything a worker's step VM is built from, beyond the compiled
/// step bundle itself. Carried as one value because it is decided once
/// per run and every worker must be built identically — a session that
/// differs from its siblings is a scenario that passes on one worker
/// and fails on another.
#[derive(Clone)]
pub struct BddSessionSetup {
  /// Cucumber `--world-parameters`, exposed as `this.parameters`.
  pub world_parameters: serde_json::Value,
  /// Config `use` keys no built-in option claims. Decided against the
  /// chains this VM registered, once it has evaluated.
  pub open_use_keys: Arc<Vec<String>>,
  /// Extensions, compiled and gated by the loader every host shares.
  /// Installed as bytecode, exactly as the MCP and script hosts install
  /// them — never bundled into the step module.
  pub extensions: Arc<Vec<ferridriver_script::ExtensionBinding>>,
  /// Include ferridriver's built-in Rust step library.
  pub builtin_steps: bool,
}

impl Default for BddSessionSetup {
  fn default() -> Self {
    Self {
      world_parameters: serde_json::Value::Null,
      open_use_keys: Arc::new(Vec::new()),
      extensions: Arc::new(Vec::new()),
      builtin_steps: true,
    }
  }
}

/// Discover step entry files for the given globs (relative globs are
/// resolved against `cwd`). Empty globs fall back to the cucumber-js
/// defaults. `.ts`/`.tsx` are included — rolldown transpiles them.
pub fn discover_step_files(globs: &[String], cwd: &Path) -> Vec<PathBuf> {
  let patterns: Vec<String> = if globs.is_empty() {
    DEFAULT_STEP_GLOBS.iter().map(|s| (*s).to_string()).collect()
  } else {
    globs.to_vec()
  };
  let mut files = Vec::new();
  for pat in patterns {
    let full = if Path::new(&pat).is_absolute() {
      pat.clone()
    } else {
      cwd.join(&pat).to_string_lossy().into_owned()
    };
    if let Ok(entries) = glob::glob(&full) {
      for entry in entries.flatten() {
        if is_source_file(&entry) {
          files.push(entry);
        }
      }
    }
  }
  files.sort();
  files.dedup();
  files
}

/// Discover extension entry files. Each path is a single source file or
/// a directory scanned **recursively** for
/// [`ferridriver_script::SOURCE_EXTENSIONS`] files (same rule the MCP
/// plugin loader uses, so one extension serves both hosts). A file the
/// user named explicitly is taken as-is regardless of extension.
pub fn discover_extension_files(specs: &[ferridriver_script::ExtensionSpec]) -> Vec<PathBuf> {
  let (files, errors) = ferridriver_script::discover::resolve_extension_specs_with_bases(specs);
  for (spec, err) in errors {
    tracing::warn!(extension = %spec, error = %err.message, "extension discovery failed; skipping");
  }
  files
}

/// Discover the step entry files and rolldown-bundle + tree-shake +
/// transpile them (plus `node_modules` / shared utils) into one module
/// compiled to bytecode, once, before workers spawn.
pub async fn bundle_steps(globs: &[String], cwd: &Path) -> anyhow::Result<Arc<CompiledBundle>> {
  bundle_steps_with(globs, &[], cwd).await
}

/// Like [`bundle_steps`] but with the configured `extensions` taken into
/// account.
///
/// Extensions are NOT bundled in any more: they are compiled and gated
/// like every other host's (`ferridriver_script::extension_load`) and
/// installed into the session as bytecode. What survives here is the
/// overlap rule — a file reachable from BOTH the step globs and an
/// extension entry must be bundled ONCE, or its steps register twice
/// and every scenario using them fails Ambiguous. The extension entry
/// wins, because that is the copy the session installs.
pub async fn bundle_steps_with(
  globs: &[String],
  extensions: &[ferridriver_script::ExtensionSpec],
  cwd: &Path,
) -> anyhow::Result<Arc<CompiledBundle>> {
  let mut files = discover_step_files(globs, cwd);
  let extension_files = discover_extension_files(extensions);
  let before = files.len();
  files.retain(|f| !extension_files.contains(f));
  if files.len() < before {
    tracing::warn!(
      target: "ferridriver::bdd",
      dropped = before - files.len(),
      "bdd.steps.claimed_by_extension: a step file is also an extension entry; \
       it is loaded once, as the extension, so its steps register once"
    );
  }
  files.sort();
  files.dedup();
  if files.is_empty() && extension_files.is_empty() {
    let pats: Vec<&str> = if globs.is_empty() {
      DEFAULT_STEP_GLOBS.to_vec()
    } else {
      globs.iter().map(String::as_str).collect()
    };
    anyhow::bail!(
      "no step or extension files found (globs: {:?}, extensions: {:?}, cwd: {})",
      pats,
      extensions,
      cwd.display()
    );
  }
  if files.is_empty() {
    // Every step file was claimed by an extension entry. The session
    // still evaluates a step module, so give it an empty one rather than
    // teaching every caller that the bundle is optional.
    let bundle = ferridriver_script::compile_bundled_source("export {};\n", "ferridriver-bdd-steps.js", None)
      .await
      .map_err(|e| anyhow::anyhow!("empty step bundle: {}", e.message))?;
    return Ok(Arc::new(bundle));
  }
  let bundle = bundle_and_compile(&files, cwd)
    .await
    .map_err(|e| anyhow::anyhow!("bundle step files: {}", e.message))?;
  Ok(Arc::new(bundle))
}

/// Forward the scenario's queued `this.attach`/`this.log` attachments
/// into the test result so the messages / HTML / Allure reporters
/// surface them (the Cucumber screenshot-/text-on-failure idiom). The
/// name is derived from the media type (Cucumber attachments are
/// unnamed).
async fn forward_attachments(test_info: &TestInfo, atts: Vec<ScriptAttachment>) {
  for a in atts {
    let name = if a.media_type.starts_with("image/") {
      "screenshot"
    } else if a.media_type.starts_with("text/x.cucumber.log") {
      "log"
    } else {
      "attachment"
    };
    test_info
      .attach(name.to_string(), a.media_type, AttachmentBody::Bytes(a.bytes))
      .await;
  }
}

impl JsBddSession {
  #[must_use]
  pub fn registry(&self) -> Arc<StepRegistry> {
    Arc::clone(&self.registry)
  }

  /// Drain attachments queued by `this.attach`/`this.log` during the
  /// just-run scenario (clears the queue for the next scenario).
  pub async fn drain_attachments(&self) -> Vec<ScriptAttachment> {
    drain_attachments(&self.session.vm_handle()).await.unwrap_or_default()
  }

  /// Discover, bundle and load step files in one call (convenience for
  /// single-session callers / tests). Production uses [`bundle_steps`]
  /// once + [`JsBddSession::load`] per worker.
  pub async fn from_globs(globs: &[String], cwd: &Path) -> anyhow::Result<Self> {
    let bundle = bundle_steps(globs, cwd).await?;
    Self::load(bundle, cwd, &BddSessionSetup::default()).await
  }

  /// Create a shared-engine session and link the precompiled bundled
  /// step module (one `Module::load`, no parse, no resolver — imports
  /// are inlined by rolldown). Builds the Rust step registry from what
  /// the module registered.
  pub async fn load(bundle: Arc<CompiledBundle>, cwd: &Path, setup: &BddSessionSetup) -> anyhow::Result<Self> {
    let world_parameters = setup.world_parameters.clone();
    let open_use_keys: &[String] = &setup.open_use_keys;
    let sandbox =
      Arc::new(PathSandbox::new(cwd).map_err(|e| anyhow::anyhow!("sandbox {}: {}", cwd.display(), e.message))?);
    let run_ctx = RunContext {
      vars: Arc::new(InMemoryVars::new()),
      sandbox,
      artifacts: None,
      page: None,
      browser_context: None,
      request: None,
      browser: None,
      extensions: (*setup.extensions).clone(),
      host: ferridriver_script::ExtensionHost::Bdd,
      // `[scripting]` caps threaded from resolved config by the
      // `ferridriver bdd` entry (`set_bdd_script_caps`), exactly like
      // the MCP server. Unset (macro/harness path with no config) ⇒
      // locked down — the safe default.
      caps: BDD_SCRIPT_CAPS.get().cloned().unwrap_or_default(),
      session: None,
    };

    let engine_config = ScriptEngineConfig {
      sidecars: BDD_SIDECARS.get().cloned().unwrap_or_default(),
      ..Default::default()
    };
    let session = Session::create(engine_config, &run_ctx)
      .await
      .map_err(|e| anyhow::anyhow!("session create: {}", e.message))?;

    // Link the single bundled module (top-level Given/When/Then run).
    let vm = session.vm_handle();
    eval_bundle(&vm, &bundle)
      .await
      .map_err(|e| anyhow::anyhow!("step bundle failed to load: {}", bundle.format_error(&e)))?;
    let snapshot = collect_registry(&vm)
      .await
      .map_err(|e| anyhow::anyhow!("collect registry: {}", e.message))?;

    // The chains exist now, so the config's open `use` keys can be
    // decided — the same check the spec host runs after collection.
    if !open_use_keys.is_empty() {
      let chains: Vec<_> = (0..snapshot.fixture_sets.len())
        .map(|set| snapshot.fixture_slots(set))
        .collect();
      ferridriver_test::fixture_graph::validate_use_keys(open_use_keys.iter().map(String::as_str), &chains)
        .map_err(|e| anyhow::anyhow!(e))?;
    }

    let mut registry = if setup.builtin_steps {
      StepRegistry::build()
    } else {
      StepRegistry::empty()
    };
    for pt in &snapshot.param_types {
      registry
        .register_param_type(CustomParamType {
          name: pt.name.clone(),
          regex: pt.regexp.clone(),
          transformer: None,
        })
        .map_err(|e| anyhow::anyhow!("defineParameterType `{}`: {}", pt.name, e))?;
    }
    for (idx, step) in snapshot.steps.iter().enumerate() {
      let kind = match step.kind.as_str() {
        "Given" => StepKind::Given,
        "When" => StepKind::When,
        "Then" => StepKind::Then,
        _ => StepKind::Step,
      };
      let handler = js_step_handler(vm.clone(), idx, Arc::clone(&bundle));
      let loc = StepLocation {
        file: JS_STEP_LOCATION,
        line: 0,
      };
      registry
        .register_js(
          kind,
          &step.pattern,
          step.is_regex,
          handler,
          loc,
          StepFixtures {
            // An ambient `Given`/`When`/`Then` belongs to the base
            // chain: it is the one an extension's contributed fixtures
            // land in, so an unmodified suite still receives them.
            set: step.fixture_set.unwrap_or(0),
            names: step.requested.clone(),
          },
        )
        .map_err(|e| anyhow::anyhow!("register step `{}`: {}", step.pattern, e))?;
    }

    let hooks = snapshot
      .hooks
      .iter()
      .enumerate()
      .map(|(i, h)| HookEntry {
        idx: i,
        kind: h.hook_type.clone(),
        tags: h.tags.as_deref().and_then(|t| TagExpression::parse(t).ok()),
        fixture_set: h.fixture_set.unwrap_or(0),
        requested: h.requested.clone(),
      })
      .collect();

    let session = Self {
      session,
      registry: Arc::new(registry),
      hooks,
      bundle,
      cwd: Arc::new(cwd.to_path_buf()),
      fixture_sets: snapshot.fixture_sets,
      world_parameters,
    };
    // `BeforeAll` runs before any scenario exists, so it gets the
    // world-shaped object rather than a fixture bag.
    set_hook_world(&vm, &session.world_parameters)
      .await
      .map_err(|e| anyhow::anyhow!("world for BeforeAll: {}", e.message))?;
    session
      .run_hooks("BeforeAll", None, None)
      .await
      .map_err(|e| anyhow::anyhow!(e))?;
    Ok(session)
  }

  /// The hooks of one kind that apply to `tags`, in run order (reverse
  /// registration for the `After*` family, as cucumber-js does).
  fn applicable_hooks(&self, kind: &str, tags: Option<&[String]>) -> Vec<&HookEntry> {
    let mut hooks: Vec<&HookEntry> = self
      .hooks
      .iter()
      .filter(|h| h.kind == kind)
      .filter(|h| match (h.tags.as_ref(), tags) {
        (Some(expr), Some(t)) => expr.matches(t),
        (Some(_), None) => false,
        (None, _) => true,
      })
      .collect();
    if kind == "After" || kind == "AfterAll" || kind == "AfterStep" {
      hooks.reverse();
    }
    hooks
  }

  async fn run_hooks(&self, kind: &str, tags: Option<&[String]>, arg: Option<&HookArg>) -> Result<(), String> {
    let vm = self.session.vm_handle();
    let indices: Vec<usize> = self.applicable_hooks(kind, tags).iter().map(|h| h.idx).collect();
    for idx in indices {
      if let Err(e) = invoke_hook(&vm, idx, arg, &self.bundle.module_name).await {
        return Err(self.bundle.format_error(&e));
      }
    }
    Ok(())
  }

  /// Run-level `AfterAll` hooks (once per worker session).
  pub async fn after_all(&self) -> Result<(), String> {
    let vm = self.session.vm_handle();
    set_hook_world(&vm, &self.world_parameters)
      .await
      .map_err(|e| format!("world for AfterAll: {}", e.message))?;
    self.run_hooks("AfterAll", None, None).await
  }

  /// Resume every suspended worker-scoped fixture factory of this
  /// session's VM (the teardown half of each `use()`), after the last
  /// scenario the worker will run.
  pub async fn teardown_worker_fixtures(&self) -> Result<(), String> {
    ferridriver_script::teardown_worker_fixtures(&self.session.vm_handle())
      .await
      .map_err(|e| self.bundle.format_error(&e))
  }

  /// Which fixture chain this scenario resolves against, and every name
  /// its steps and hooks destructure off it. Auto fixtures of the chain
  /// are added by the resolver, so they need not be listed here.
  fn scenario_fixtures(
    &self,
    matches: &[Result<StepMatch<'_>, MatchError>],
    tags: &[String],
  ) -> Result<(usize, Vec<String>), String> {
    let mut sets: Vec<usize> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut take = |set: usize, requested: Option<&Vec<String>>| {
      if !sets.contains(&set) {
        sets.push(set);
      }
      for n in requested.into_iter().flatten() {
        if !names.contains(n) {
          names.push(n.clone());
        }
      }
    };
    for m in matches.iter().flatten() {
      if let Some(f) = &m.def.fixtures {
        take(f.set, f.names.as_ref());
      }
    }
    for kind in ["Before", "After", "BeforeStep", "AfterStep"] {
      for hook in self.applicable_hooks(kind, Some(tags)) {
        take(hook.fixture_set, hook.requested.as_ref());
      }
    }
    let set = dominant_fixture_set(&self.fixture_sets, &sets)?;
    Ok((set, names))
  }

  /// The fixtures + `testInfo` a scenario runs against — lowered by the
  /// same core helper the Playwright-spec host uses, so a step body
  /// sees exactly what a test body sees. The `use` bag is the project's
  /// `use` block overlaid with the scenario's own `@use(...)` tags.
  fn world_data(scenario: &ScenarioExecution, fixtures: &TestFixtures) -> TestWorldData {
    let test_info = &fixtures.test_info;
    let config_use = test_info
      .config_snapshot
      .as_ref()
      .and_then(|c| serde_json::to_value(&c.browser.use_options).ok());
    let use_options = ferridriver_test::host::merge_use_options(
      config_use.as_ref(),
      crate::translate::scenario_use_options(scenario).as_ref(),
    );
    let file = scenario.feature_path.display().to_string();
    let mut world = ferridriver_test::host::world_data(ferridriver_test::host::WorldMeta {
      test_info,
      title: &scenario.name,
      title_path: &test_info.title_path,
      file: &file,
      line: u32::try_from(crate::translate::scenario_line(scenario).unwrap_or(0)).unwrap_or(0),
      tags: &scenario.tags,
      // A scenario expected to fail is expressed as an annotation the
      // runner acts on, not as an inverted step outcome.
      expected_status: ferridriver_test::model::ExpectedStatus::Pass,
      browser_config: &fixtures.browser_config,
      base_url: test_info.config_snapshot.as_ref().and_then(|c| c.base_url.as_deref()),
      use_options,
    });
    world.page = Some(Arc::clone(&fixtures.page));
    world.context = Some(Arc::clone(&fixtures.context));
    world.request = Some(Arc::clone(&fixtures.request));
    world.browser = Some(Arc::clone(&fixtures.browser));
    world
  }

  /// Execute one expanded scenario: bind its World from the fixtures,
  /// run `Before` hooks, the steps, then `After` hooks.
  pub async fn run_scenario(&self, scenario: &ScenarioExecution, world: &mut BrowserWorld) -> JsScenarioResult {
    let vm = self.session.vm_handle();

    // Mirror the Rust executor: scope `world.resolve_fixture_path(...)`
    // to the feature file's directory so steps like
    // `I mock requests to "..." with fixture "mocks/page.html"` resolve
    // relative to the feature, not the process cwd. Without this the
    // JS-driven path resolves against cwd and every fixture-file step
    // errors with `No such file or directory`.
    if let Some(dir) = scenario.feature_path.parent() {
      world.set_feature_dir(dir.to_path_buf());
    }

    // Match every step up front: a scenario's fixture chain is decided
    // by what its steps destructure, and that has to be known before
    // the first `Before` hook runs. The matches are then consumed by
    // the execution loop below, so no step is matched twice.
    let matches: Vec<Result<StepMatch<'_>, MatchError>> = scenario
      .steps
      .iter()
      .map(|step| self.registry.find_match(&step.text))
      .collect();

    let fixtures = world.fixtures();
    let (fixture_set, requested) = match self.scenario_fixtures(&matches, &scenario.tags) {
      Ok(v) => v,
      Err(msg) => return Self::world_failure(scenario, msg),
    };
    let spec = ScenarioSpec {
      world: Self::world_data(scenario, fixtures),
      parameters: self.world_parameters.clone(),
      fixture_set,
      requested,
      source_label: self.bundle.module_name.clone(),
    };
    let bridge = Arc::new(InfoBridge::new(
      Arc::clone(&fixtures.test_info),
      Arc::clone(&fixtures.modifiers),
      Arc::new(self.session.deadline()),
      Arc::new(ferridriver_script::BundleSourceMap::new(
        Arc::clone(&self.bundle),
        Arc::clone(&self.cwd),
      )),
      Arc::clone(&self.cwd),
      fixtures.test_info.timeout,
      static_annotation_pairs(&crate::translate::scenario_annotations(scenario)),
    ));

    if let Err(e) = begin_scenario(&vm, spec, bridge.clone() as _).await {
      let message = self.bundle.format_error(&e);
      // A factory that failed halfway may have parked earlier ones at
      // their `use()`; resume them, or they leak into the next scenario
      // with nothing left holding their teardown.
      if let Err(teardown) = end_scenario(&vm).await {
        tracing::warn!(
          target: "ferridriver::bdd",
          error = %self.bundle.format_error(&teardown),
          "fixture teardown after a failed scenario setup"
        );
      }
      return Self::world_failure(scenario, message);
    }

    let mut steps = Vec::with_capacity(scenario.steps.len());
    let mut failed = false;

    let before_arg = HookArg {
      name: scenario.name.clone(),
      tags: scenario.tags.clone(),
      status: "PENDING".to_string(),
      message: None,
    };
    if let Err(msg) = self.run_hooks("Before", Some(&scenario.tags), Some(&before_arg)).await {
      steps.push(JsStepResult {
        keyword: "Before".into(),
        text: "hook".into(),
        line: 0,
        duration: Duration::ZERO,
        status: JsStepStatus::Failed(msg),
      });
      failed = true;
    }

    if !failed {
      let test_info = std::sync::Arc::clone(&world.fixtures().test_info);
      let feature_path = scenario.feature_path.display().to_string();
      for (step, matched) in scenario.steps.iter().zip(matches) {
        let step_meta = serde_json::json!({
          "bdd_keyword": step.keyword.trim(),
          "bdd_text": step.text,
          "bdd_line": step.line,
          "bdd_arguments": step.cucumber_arguments(),
        });
        let step_location =
          ferridriver_test::model::StepLocation::new(feature_path.clone(), u32::try_from(step.line).unwrap_or(0));
        let title = format!("{}{}", step.keyword, step.text);
        if failed {
          let mut handle = test_info
            .begin_step_at(
              &title,
              ferridriver_test::model::StepCategory::TestStep,
              Some(step_location),
            )
            .await;
          handle.metadata = Some(step_meta);
          handle.skip(None).await;
          steps.push(JsStepResult {
            keyword: step.keyword.clone(),
            text: step.text.clone(),
            line: step.line,
            duration: Duration::ZERO,
            status: JsStepStatus::Skipped,
          });
          continue;
        }
        let started = Instant::now();
        // Live step boundary: streams StepStarted to reporters DURING
        // the scenario and opens the trace span protocol actions nest
        // under (the span is the recorder's current parent while the
        // handler runs).
        let mut step_handle = test_info
          .begin_step_at(
            &title,
            ferridriver_test::model::StepCategory::TestStep,
            Some(step_location),
          )
          .await;
        step_handle.metadata = Some(step_meta);

        // BeforeStep hooks — mirror the Rust executor: a failing
        // step-scoped hook warns but never fails the step itself, and
        // skipped steps (after a failure) get no step hooks at all.
        let step_hook_arg = HookArg {
          name: scenario.name.clone(),
          tags: scenario.tags.clone(),
          status: "PENDING".to_string(),
          message: None,
        };
        if let Err(msg) = self
          .run_hooks("BeforeStep", Some(&scenario.tags), Some(&step_hook_arg))
          .await
        {
          tracing::warn!(step = %step.text, "BeforeStep hook failed: {msg}");
        }

        let status = match matched {
          // An ambiguous step is a definition bug, not a missing
          // definition: it fails the scenario even under --no-strict,
          // exactly as the Rust-step executor treats it.
          Err(e @ crate::step::MatchError::Ambiguous { .. }) => {
            failed = true;
            JsStepStatus::Failed(e.to_string())
          },
          Err(e) => {
            failed = true;
            // JS step authors need a JS snippet, not a Rust skeleton.
            let snip = crate::snippet::generate_js_snippet(
              &step.keyword,
              &step.text,
              step.table.is_some(),
              step.docstring.is_some(),
            );
            JsStepStatus::Undefined(format!("{e}\n\nImplement with:\n\n{snip}"))
          },
          Ok(m) => {
            let fut = (m.def.handler)(world, m.params, step.table.as_ref(), step.docstring.as_deref());
            match fut.await {
              Ok(()) => JsStepStatus::Passed,
              Err(e) if e.pending => {
                failed = true;
                JsStepStatus::Pending
              },
              Err(e) => {
                failed = true;
                JsStepStatus::Failed(e.to_string())
              },
            }
          },
        };
        // AfterStep hooks — always run for an executed step (even on
        // failure), reverse registration order, warn-only on error.
        let after_step_arg = HookArg {
          name: scenario.name.clone(),
          tags: scenario.tags.clone(),
          status: match &status {
            JsStepStatus::Passed => "PASSED".to_string(),
            JsStepStatus::Pending => "PENDING".to_string(),
            JsStepStatus::Skipped => "SKIPPED".to_string(),
            JsStepStatus::Failed(_) | JsStepStatus::Undefined(_) => "FAILED".to_string(),
          },
          message: match &status {
            JsStepStatus::Failed(m) | JsStepStatus::Undefined(m) => Some(m.clone()),
            _ => None,
          },
        };
        if let Err(msg) = self
          .run_hooks("AfterStep", Some(&scenario.tags), Some(&after_step_arg))
          .await
        {
          tracing::warn!(step = %step.text, "AfterStep hook failed: {msg}");
        }

        match &status {
          JsStepStatus::Passed => step_handle.end(None).await,
          JsStepStatus::Skipped => step_handle.skip(None).await,
          JsStepStatus::Pending => step_handle.pending(None).await,
          JsStepStatus::Undefined(msg) => step_handle.pending(Some(msg.clone())).await,
          JsStepStatus::Failed(msg) => step_handle.end(Some(msg.clone())).await,
        }
        steps.push(JsStepResult {
          keyword: step.keyword.clone(),
          text: step.text.clone(),
          line: step.line,
          duration: started.elapsed(),
          status,
        });
      }
    }

    // After hooks always run (cleanup), even on failure. Pass the
    // scenario result so `After(s => { if (s.result.status === 'FAILED')
    // ... })` works (the screenshot-on-failure idiom).
    let after_msg = steps.iter().find_map(|s| match &s.status {
      JsStepStatus::Failed(m) | JsStepStatus::Undefined(m) => Some(m.clone()),
      JsStepStatus::Pending => Some(format!("pending: {}{}", s.keyword, s.text)),
      _ => None,
    });
    let after_arg = HookArg {
      name: scenario.name.clone(),
      tags: scenario.tags.clone(),
      status: if failed { "FAILED" } else { "PASSED" }.to_string(),
      message: after_msg,
    };
    if let Err(msg) = self.run_hooks("After", Some(&scenario.tags), Some(&after_arg)).await {
      world
        .fixtures()
        .test_info
        .record_step(ferridriver_test::model::RecordedStep {
          title: "After hook".to_string(),
          category: ferridriver_test::model::StepCategory::Hook,
          status: ferridriver_test::model::StepStatus::Failed,
          duration: Duration::ZERO,
          error: Some(msg.clone()),
          metadata: None,
        })
        .await;
      steps.push(JsStepResult {
        keyword: "After".into(),
        text: "hook".into(),
        line: 0,
        duration: Duration::ZERO,
        status: JsStepStatus::Failed(msg),
      });
      failed = true;
    }

    // Teardown last: the fixture factories resume (LIFO) only once the
    // last step and every `After` hook are done with what they set up.
    if let Err(e) = end_scenario(&vm).await {
      steps.push(JsStepResult {
        keyword: "Fixture".into(),
        text: "teardown".into(),
        line: 0,
        duration: Duration::ZERO,
        status: JsStepStatus::Failed(self.bundle.format_error(&e)),
      });
      failed = true;
    }
    // Runtime annotations into the test result, and any step a mid-step
    // failure left open closed, exactly as the spec host does.
    bridge.flush().await;

    JsScenarioResult {
      name: scenario.name.clone(),
      tags: scenario.tags.clone(),
      passed: !failed,
      steps,
    }
  }

  /// A scenario that never got as far as its first step: the fixture
  /// chain could not be chosen, or building the world failed.
  fn world_failure(scenario: &ScenarioExecution, message: String) -> JsScenarioResult {
    JsScenarioResult {
      name: scenario.name.clone(),
      tags: scenario.tags.clone(),
      steps: vec![JsStepResult {
        keyword: "World".into(),
        text: "bind fixtures".into(),
        line: 0,
        duration: Duration::ZERO,
        status: JsStepStatus::Failed(message),
      }],
      passed: false,
    }
  }
}

fn js_step_handler(vm: VmHandle, idx: usize, bundle: Arc<CompiledBundle>) -> StepHandler {
  Arc::new(move |_world, params, table, doc| {
    let vm = vm.clone();
    let bundle = Arc::clone(&bundle);
    let params_json: Vec<JsArg> = params.iter().map(step_param_to_jsarg).collect();
    let data_table: Option<Vec<Vec<String>>> = table.map(|t| t.raw().to_vec());
    let doc_string: Option<String> = doc.map(str::to_string);
    Box::pin(async move {
      match invoke_step(
        &vm,
        idx,
        &params_json,
        data_table.as_deref(),
        doc_string.as_deref(),
        &bundle.module_name,
      )
      .await
      {
        Ok(StepOutcome::Passed | StepOutcome::Skipped) => Ok(()),
        Ok(StepOutcome::Pending) => Err(StepError::pending("step returned 'pending'")),
        Err(e) => Err(StepError::from(bundle.format_error(&e))),
      }
    })
  })
}

fn step_param_to_jsarg(p: &StepParam) -> JsArg {
  match p {
    StepParam::String(s) | StepParam::Word(s) => JsArg::Str(s.clone()),
    StepParam::Int(i) => JsArg::Int(*i),
    StepParam::Float(f) => JsArg::Float(*f),
    StepParam::Custom { type_name, value } => JsArg::Custom {
      type_name: type_name.clone(),
      raw: value.clone(),
    },
  }
}

// ── Per-worker session cache + TestRunner integration ────────────────
//
// Per-worker `OnceCell`s keyed by `worker_index`. The outer `DashMap`
// is sharded so concurrent workers fetching their own slot don't
// contend on a single mutex. The inner `OnceCell` is per-worker, so
// `JsBddSession::load(...)` for worker N proceeds in parallel with
// worker M's load — only the same worker's repeated calls collapse to
// one init (the desired behaviour). Previous design used
// `Mutex<HashMap>` and held the lock across `load().await`, which
// serialised the first-scenario session load across every worker.

type WorkerSessionCell = OnceCell<Arc<JsBddSession>>;
type WorkerSessions = DashMap<u32, Arc<WorkerSessionCell>>;
static WORKER_SESSIONS: OnceLock<WorkerSessions> = OnceLock::new();

/// The `[scripting]` sandbox caps the BDD step VM runs with. Set once
/// by the `ferridriver bdd` entry point from resolved config (mirrors
/// how the MCP server threads them); unset ⇒ locked down
/// ([`ScriptCaps::default`]), the safe default for the macro/harness
/// path that has no config.
static BDD_SCRIPT_CAPS: OnceLock<ferridriver_script::ScriptCaps> = OnceLock::new();

/// Install the BDD step VM sandbox caps (the env allow-list). Call
/// before the run; idempotent (first set wins).
pub fn set_bdd_script_caps(caps: ferridriver_script::ScriptCaps) {
  let _ = BDD_SCRIPT_CAPS.set(caps);
}

/// Declared sidecar specs the BDD step VM exposes as
/// `sidecars.connect(name)`. Set once by the `ferridriver bdd` entry from
/// resolved config (same threading as [`BDD_SCRIPT_CAPS`]); unset ⇒ no
/// declared sidecars (`sidecars.connect` rejects every name).
static BDD_SIDECARS: OnceLock<Vec<ferridriver_script::sidecar::SidecarSpec>> = OnceLock::new();

/// Install the declared sidecar specs for the BDD step VM. Call before the
/// run; idempotent (first set wins).
pub fn set_bdd_sidecars(sidecars: Vec<ferridriver_script::sidecar::SidecarSpec>) {
  let _ = BDD_SIDECARS.set(sidecars);
}

/// The caps a BDD step VM runs with — also what the extension gate is
/// checked against, so a package's `requires` is answered by the same
/// environment its tools will run in.
#[must_use]
pub fn bdd_script_caps() -> ferridriver_script::ScriptCaps {
  BDD_SCRIPT_CAPS.get().cloned().unwrap_or_default()
}

#[must_use]
pub fn bdd_sidecars() -> Vec<ferridriver_script::sidecar::SidecarSpec> {
  BDD_SIDECARS.get().cloned().unwrap_or_default()
}

/// End every worker session this run created: `AfterAll` hooks, then
/// the teardown half of every worker-scoped fixture, then the sessions
/// themselves. Call once after `TestRunner::run` returns — a worker VM
/// outlives the individual scenarios, so nothing earlier can do it.
pub async fn teardown_worker_sessions() {
  let Some(map) = WORKER_SESSIONS.get() else { return };
  let cells: Vec<(u32, Arc<WorkerSessionCell>)> = map.iter().map(|r| (*r.key(), Arc::clone(r.value()))).collect();
  map.clear();
  for (worker, cell) in cells {
    let Some(session) = cell.get() else { continue };
    if let Err(e) = session.after_all().await {
      tracing::warn!(target: "ferridriver::bdd", worker, error = %e, "AfterAll hook failed");
    }
    if let Err(e) = session.teardown_worker_fixtures().await {
      tracing::warn!(target: "ferridriver::bdd", worker, error = %e, "worker fixture teardown failed");
    }
  }
}

async fn worker_session(
  worker_index: u32,
  bundle: Arc<CompiledBundle>,
  cwd: Arc<PathBuf>,
  setup: BddSessionSetup,
) -> Result<Arc<JsBddSession>, String> {
  let map = WORKER_SESSIONS.get_or_init(DashMap::new);
  let cell = map
    .entry(worker_index)
    .or_insert_with(|| Arc::new(OnceCell::new()))
    .clone();
  cell
    .get_or_try_init(|| async move {
      JsBddSession::load(bundle, &cwd, &setup)
        .await
        .map(Arc::new)
        .map_err(|e| e.to_string())
    })
    .await
    .cloned()
}

/// Translate parsed Gherkin features into a `TestPlan` whose scenarios
/// execute through per-worker JS sessions. Reuses the core
/// `feature`/`scenario`/`filter` expansion and the shared
/// annotation/ordering helpers — only the per-scenario `test_fn`
/// differs from the Rust-step path.
pub fn translate_features_js(
  feature_set: &FeatureSet,
  config: &ferridriver_test::config::TestConfig,
  bundle: Arc<CompiledBundle>,
  cwd: PathBuf,
  extensions: Arc<Vec<ferridriver_script::ExtensionBinding>>,
) -> ferridriver_test::model::TestPlan {
  use ferridriver_test::model::{ExpectedStatus, Hooks, SuiteMode, TestCase, TestFailure, TestFn, TestId, TestSuite};

  let cwd = Arc::new(cwd);
  // Open `use` keys travel with the plan: what each one means is only
  // decidable once a worker VM has evaluated the step bundle and its
  // `test.extend` chains exist.
  let setup = BddSessionSetup {
    world_parameters: config.world_parameters.clone(),
    open_use_keys: Arc::new(config.open_use_keys().into_iter().cloned().collect::<Vec<String>>()),
    extensions,
    builtin_steps: config.builtin_steps,
  };
  let mut suites = Vec::new();

  for feature in &feature_set.features {
    let scenarios = crate::scenario::expand_feature_with(
      feature,
      &crate::scenario::ExpandOptions {
        examples_title_format: config.examples_title_format.clone(),
      },
    );
    if scenarios.is_empty() {
      continue;
    }
    let feature_name = feature.feature.name.clone();
    let feature_path = feature.path.display().to_string();
    let is_serial = scenarios.iter().any(|s| s.tags.iter().any(|t| t == "@serial"));

    let mut tests = Vec::new();
    for scenario in scenarios {
      // Build the immutable TestCase metadata up front, then move the
      // scenario into an Arc so the per-invocation closure shares it via a
      // refcount bump instead of deep-cloning the step Vec twice (mirrors
      // the Rust-step path in `translate::translate_scenario`).
      let id = TestId {
        file: scenario.feature_path.display().to_string(),
        suite: Some(
          std::iter::once(scenario.feature_name.clone())
            .chain(scenario.describe_path.iter().cloned())
            .collect::<Vec<_>>()
            .join("::"),
        ),
        name: scenario.name.clone(),
        line: crate::translate::scenario_line(&scenario),
        column: None,
      };
      let annotations = crate::translate::scenario_annotations(&scenario);
      let use_options = crate::translate::scenario_use_options(&scenario);
      let metadata = serde_json::to_value(&scenario.source).ok();
      let scenario = Arc::new(scenario);

      let bundle = Arc::clone(&bundle);
      let cwd = Arc::clone(&cwd);
      let browser_config = config.browser.clone();
      let bdd_strict = config.strict;
      let setup = setup.clone();

      let test_fn: TestFn = Arc::new(move |pool: FixturePool| {
        let scenario = Arc::clone(&scenario);
        let bundle = Arc::clone(&bundle);
        let cwd = Arc::clone(&cwd);
        let browser_config = browser_config.clone();
        let bdd_strict = bdd_strict;
        let setup = setup.clone();
        Box::pin(async move {
          let browser = pool
            .get("browser")
            .await
            .map_err(|e| TestFailure::wrap("fixture 'browser' failed", e))?;
          let page = pool
            .get("page")
            .await
            .map_err(|e| TestFailure::wrap("fixture 'page' failed", e))?;
          let context = pool
            .get("context")
            .await
            .map_err(|e| TestFailure::wrap("fixture 'context' failed", e))?;
          let test_info: Arc<TestInfo> = pool
            .get("test_info")
            .await
            .map_err(|e| TestFailure::wrap("fixture 'test_info' failed", e))?;
          let request = pool
            .get("request")
            .await
            .map_err(|e| TestFailure::wrap("fixture 'request' failed", e))?;

          let session = worker_session(test_info.worker_index, bundle, cwd, setup)
            .await
            .map_err(|e| TestFailure::from(format!("JS step load failed: {e}")))?;

          let fixtures = ferridriver_test::model::TestFixtures {
            browser,
            page,
            context,
            request,
            test_info: Arc::clone(&test_info),
            modifiers: Arc::new(ferridriver_test::model::TestModifiers::default()),
            browser_config,
            bdd_args: None,
            bdd_data_table: None,
            bdd_doc_string: None,
          };
          let mut world = BrowserWorld::new(fixtures);

          let result = session.run_scenario(&scenario, &mut world).await;
          forward_attachments(&test_info, session.drain_attachments().await).await;

          if result.passed {
            return Ok(());
          }
          // Non-strict mode: undefined / pending steps don't fail the
          // test — they're reported as `StepStatus::Pending` and the
          // scenario passes overall (mirrors the Rust executor's
          // `Err(e) if e.pending && !self.strict` branch).
          let only_pending = !bdd_strict
            && result.steps.iter().all(|s| {
              matches!(
                s.status,
                JsStepStatus::Passed | JsStepStatus::Skipped | JsStepStatus::Pending | JsStepStatus::Undefined(_)
              )
            });
          if only_pending {
            return Ok(());
          }
          let msg = result
            .steps
            .iter()
            .find_map(|s| match &s.status {
              JsStepStatus::Failed(m) | JsStepStatus::Undefined(m) => Some(m.clone()),
              JsStepStatus::Pending => Some(format!("pending: {}{}", s.keyword, s.text)),
              _ => None,
            })
            .unwrap_or_else(|| "scenario failed".to_string());
          Err(TestFailure::from(msg))
        })
      });

      tests.push(TestCase {
        metadata,
        id,
        test_fn,
        fixture_requests: vec![
          "browser".to_string(),
          "context".to_string(),
          "page".to_string(),
          "test_info".to_string(),
          "request".to_string(),
        ],
        annotations,
        timeout: None,
        retries: None,
        expected_status: ExpectedStatus::Pass,
        use_options,
      });
    }

    suites.push(TestSuite {
      name: feature_name,
      file: feature_path,
      tests,
      hooks: Hooks::default(),
      annotations: Vec::new(),
      mode: if is_serial {
        SuiteMode::Serial
      } else {
        SuiteMode::Parallel
      },
    });
  }

  let total_tests = suites.iter().map(|s| s.tests.len()).sum();
  ferridriver_test::model::TestPlan {
    suites,
    total_tests,
    shard: None,
  }
}

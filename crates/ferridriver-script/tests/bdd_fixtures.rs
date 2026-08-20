#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The fixture graph on the BDD world: a scenario's steps and hooks
//! receive the resolved fixture bag as both `this` and their first
//! argument, resolved through the same `use()` handshake, worker-scoped
//! cache and LIFO teardown a `test()` body gets.

use std::sync::Arc;

use ferridriver_script::{
  ExtensionHost, InMemoryVars, RunContext, ScenarioSpec, ScriptCaps, ScriptEngineConfig, Session, begin_scenario,
  bundle_and_compile, collect_registry, drain_attachments, end_scenario, eval_bundle, invoke_hook, invoke_step,
  teardown_worker_fixtures,
};
use ferridriver_test::fixture_graph::dominant_fixture_set;
use ferridriver_test::host::TestWorldData;

mod support;

use support::MockBridge;

struct Suite {
  session: Session,
  registry: ferridriver_script::CollectedRegistry,
  module: String,
  _dir: tempfile::TempDir,
}

async fn suite(source: &str) -> Suite {
  let dir = tempfile::tempdir().expect("tempdir");
  let entry = dir.path().join("steps.ts");
  std::fs::write(&entry, source).expect("write steps");
  let bundle = bundle_and_compile(&[entry], dir.path()).await.expect("bundle");
  let context = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: dir.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ExtensionHost::Bdd,
    caps: ScriptCaps::default(),
    session: None,
  };
  let session = Session::create(ScriptEngineConfig::default(), &context)
    .await
    .expect("session");
  eval_bundle(&session.vm_handle(), &bundle).await.expect("eval bundle");
  let registry = collect_registry(&session.vm_handle()).await.expect("collect");
  Suite {
    session,
    registry,
    module: bundle.module_name.clone(),
    _dir: dir,
  }
}

impl Suite {
  /// The fixture chain and names one scenario asks for, computed the
  /// way the BDD host computes it: the union over the steps it runs.
  fn plan(&self, steps: &[usize]) -> (usize, Vec<String>) {
    let mut sets = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for &i in steps {
      let step = &self.registry.steps[i];
      sets.push(step.fixture_set.unwrap_or(0));
      for n in step.requested.iter().flatten() {
        if !names.contains(n) {
          names.push(n.clone());
        }
      }
    }
    let set = dominant_fixture_set(&self.registry.fixture_sets, &sets).expect("one chain");
    (set, names)
  }

  async fn scenario(&self, steps: &[usize]) -> Vec<String> {
    self.scenario_with_hooks(steps, &[]).await
  }

  async fn scenario_with_hooks(&self, steps: &[usize], hooks: &[usize]) -> Vec<String> {
    let vm = self.session.vm_handle();
    let (fixture_set, requested) = self.plan(steps);
    let mut requested = requested;
    for &h in hooks {
      for n in self.registry.hooks[h].requested.iter().flatten() {
        if !requested.contains(n) {
          requested.push(n.clone());
        }
      }
    }
    begin_scenario(
      &vm,
      ScenarioSpec {
        world: TestWorldData::default(),
        parameters: serde_json::Value::Null,
        fixture_set,
        requested,
        source_label: self.module.clone(),
      },
      Arc::new(MockBridge::default()),
    )
    .await
    .expect("begin scenario");
    for &h in hooks {
      invoke_hook(&vm, h, None, &self.module).await.expect("hook");
    }
    for &i in steps {
      invoke_step(&vm, i, &[], None, None, &self.module).await.expect("step");
    }
    let atts = drain_attachments(&vm).await.expect("drain");
    end_scenario(&vm).await.expect("end scenario");
    atts
      .into_iter()
      .map(|a| String::from_utf8(a.bytes).expect("utf8"))
      .collect()
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_step_destructures_a_fixture_of_the_chain_it_was_bound_to() {
  let s = suite(
    "const test = ferridriver.test.extend({
       token: async ({}, use) => { await use('t-42'); },
     });
     const { Given } = bindSteps(test);
     Given('reads the token', async function ({ token }) { this.log(token); });
     Given('reads nothing', async function () { this.log('none'); });",
  )
  .await;

  assert_eq!(s.scenario(&[0]).await, vec!["t-42".to_string()]);
  // A step that names nothing sets nothing up, and still runs.
  assert_eq!(s.scenario(&[1]).await, vec!["none".to_string()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_auto_fixture_runs_without_any_step_naming_it() {
  let s = suite(
    "const marks = [];
     const test = ferridriver.test.extend({
       seeded: [async ({}, use) => { marks.push('setup'); await use(true); }, { auto: true }],
     });
     const { Given } = bindSteps(test);
     Given('runs', async function () { this.log(marks.join(',')); });",
  )
  .await;

  assert_eq!(s.scenario(&[0]).await, vec!["setup".to_string()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn teardown_runs_lifo_after_the_last_step() {
  let s = suite(
    "const order = [];
     const test = ferridriver.test.extend({
       outer: async ({}, use) => { order.push('outer up'); await use('o'); order.push('outer down'); },
       inner: async ({ outer }, use) => { order.push('inner up'); await use(outer + 'i'); order.push('inner down'); },
     });
     const { Given } = bindSteps(test);
     Given('uses both', async function ({ inner }) { this.log('step ' + inner); });
     Given('reports', async function () { this.log(order.join('|')); });",
  )
  .await;

  assert_eq!(s.scenario(&[0]).await, vec!["step oi".to_string()]);
  // Second scenario reports what the first one's teardown recorded.
  assert_eq!(
    s.scenario(&[1]).await,
    vec!["outer up|inner up|inner down|outer down".to_string()],
    "dependency order up, LIFO down, after the last step"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_worker_fixture_is_set_up_once_for_every_scenario_and_torn_down_at_the_end() {
  let s = suite(
    "const marks = [];
     const test = ferridriver.test.extend({
       pool: [async ({}, use) => { marks.push('open'); await use(marks.length); marks.push('close'); }, { scope: 'worker' }],
     });
     const { Given } = bindSteps(test);
     Given('uses the pool', async function ({ pool }) { this.log('pool=' + pool + ' marks=' + marks.join(',')); });",
  )
  .await;

  assert_eq!(s.scenario(&[0]).await, vec!["pool=1 marks=open".to_string()]);
  assert_eq!(
    s.scenario(&[0]).await,
    vec!["pool=1 marks=open".to_string()],
    "the second scenario reuses the cached worker value — no second setup"
  );

  teardown_worker_fixtures(&s.session.vm_handle())
    .await
    .expect("worker teardown");
  assert_eq!(
    s.scenario(&[0]).await,
    vec!["pool=3 marks=open,close,open".to_string()],
    "teardown ran once, at worker end"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_bag_is_this_and_arg0_with_the_world_instance_as_its_prototype() {
  let s = suite(
    "setWorldConstructor(class World {
       constructor({ parameters }) { this.env = parameters.env; }
       greet() { return 'hi ' + this.env; }
     });
     const test = ferridriver.test.extend({ token: async ({}, use) => { await use('t'); } });
     const { Given } = bindSteps(test);
     Given('writes', async function (world) { world.seen = this.greet(); });
     Given('reads', async function ({ token }) { this.log(this.seen + ' ' + token + ' ' + (this === arguments[0])); });",
  )
  .await;

  let vm = s.session.vm_handle();
  let (fixture_set, requested) = s.plan(&[0, 1]);
  begin_scenario(
    &vm,
    ScenarioSpec {
      world: TestWorldData::default(),
      parameters: serde_json::json!({ "env": "staging" }),
      fixture_set,
      requested,
      source_label: s.module.clone(),
    },
    Arc::new(MockBridge::default()),
  )
  .await
  .expect("begin");
  invoke_step(&vm, 0, &[], None, None, &s.module).await.expect("step 0");
  invoke_step(&vm, 1, &[], None, None, &s.module).await.expect("step 1");
  let atts = drain_attachments(&vm).await.expect("drain");
  end_scenario(&vm).await.expect("end");

  let logged = String::from_utf8(atts[0].bytes.clone()).expect("utf8");
  assert_eq!(
    logged, "hi staging t true",
    "instance method through the prototype, assignment surviving to the next step, one object as `this` and arg0"
  );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hook_resolves_from_the_chain_it_was_bound_to() {
  let s = suite(
    "const test = ferridriver.test.extend({ token: async ({}, use) => { await use('t-9'); } });
     const { Given, Before } = bindSteps(test);
     Before(async function ({ token }) { this.fromHook = token; });
     Given('reads what the hook stashed', async function () { this.log(this.fromHook); });",
  )
  .await;

  assert_eq!(s.scenario_with_hooks(&[0], &[0]).await, vec!["t-9".to_string()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn steps_from_unrelated_chains_are_refused_by_name() {
  let s = suite(
    "const a = ferridriver.test.extend({ alpha: async ({}, use) => { await use('a'); } });
     const b = ferridriver.test.extend({ beta: async ({}, use) => { await use('b'); } });
     bindSteps(a).Given('from a', async function ({ alpha }) {});
     bindSteps(b).Given('from b', async function ({ beta }) {});",
  )
  .await;

  let sets: Vec<usize> = s.registry.steps.iter().map(|st| st.fixture_set.unwrap_or(0)).collect();
  let err = dominant_fixture_set(&s.registry.fixture_sets, &sets).expect_err("unrelated chains");
  assert!(err.contains("unrelated `test` objects"), "{err}");
  assert!(err.contains("mergeTests"), "{err}");

  // A scenario using only one of them is fine.
  assert_eq!(dominant_fixture_set(&s.registry.fixture_sets, &sets[..1]), Ok(sets[0]));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_merged_chain_serves_steps_bound_to_either_half() {
  let s = suite(
    "const a = ferridriver.test.extend({ alpha: async ({}, use) => { await use('a'); } });
     const b = ferridriver.test.extend({ beta: async ({}, use) => { await use('b'); } });
     const both = ferridriver.mergeTests(a, b);
     bindSteps(both).Given('needs both', async function ({ alpha, beta }) { this.log(alpha + beta); });
     bindSteps(a).Given('needs alpha', async function ({ alpha }) { this.log(alpha); });",
  )
  .await;

  assert_eq!(s.scenario(&[0, 1]).await, vec!["ab".to_string(), "a".to_string()]);
}

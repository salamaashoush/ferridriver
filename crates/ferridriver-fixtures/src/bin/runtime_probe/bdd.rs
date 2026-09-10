use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use ferridriver_script::{
  BundleSourceMap, CollectedRegistry, ExtensionHost, HookArg, JsArg, RunContext, ScenarioSpec, ScriptEngineConfig,
  Session, begin_scenario, bundle_and_compile, collect_registry, drain_attachments, end_scenario, eval_bundle,
  invoke_hook, invoke_step, teardown_worker_fixtures,
};
use ferridriver_test::fixture_graph::dominant_fixture_set;
use ferridriver_test::host::{InfoBridge, TestWorldData};
use ferridriver_test::model::{TestInfo, TestModifiers};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum Action {
  Plan {
    steps: Vec<usize>,
  },
  Begin {
    steps: Vec<usize>,
    #[serde(default)]
    hooks: Vec<usize>,
    #[serde(default)]
    parameters: Value,
  },
  Step {
    index: usize,
    #[serde(default)]
    args: Vec<Argument>,
  },
  Hook {
    index: usize,
    argument: Option<HookArgument>,
  },
  Drain,
  End,
  TeardownWorker,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Argument {
  String { value: String },
  Int { value: i64 },
  Float { value: f64 },
  Custom { name: String, raw: String },
}

impl From<Argument> for JsArg {
  fn from(arg: Argument) -> Self {
    match arg {
      Argument::String { value } => Self::Str(value),
      Argument::Int { value } => Self::Int(value),
      Argument::Float { value } => Self::Float(value),
      Argument::Custom { name, raw } => Self::Custom { type_name: name, raw },
    }
  }
}

#[derive(Deserialize)]
pub struct HookArgument {
  name: String,
  tags: Vec<String>,
  status: String,
  message: Option<String>,
}

fn plan(registry: &CollectedRegistry, steps: &[usize], hooks: &[usize]) -> Result<(usize, Vec<String>)> {
  let mut sets = Vec::new();
  let mut requested = Vec::new();
  for &index in steps {
    let step = registry.steps.get(index).context("unknown step index")?;
    sets.push(step.fixture_set.unwrap_or(0));
    requested.extend(step.requested.iter().flatten().cloned());
  }
  for &index in hooks {
    let hook = registry.hooks.get(index).context("unknown hook index")?;
    requested.extend(hook.requested.iter().flatten().cloned());
  }
  requested.sort();
  requested.dedup();
  let set = dominant_fixture_set(&registry.fixture_sets, &sets).map_err(anyhow::Error::msg)?;
  Ok((set, requested))
}

pub async fn run(root: &Path, context: &RunContext, entries: Vec<PathBuf>, actions: Vec<Action>) -> Result<Value> {
  let bundle = Arc::new(bundle_and_compile(&entries, root).await?);
  let mut context = context.clone();
  context.host = ExtensionHost::Bdd;
  let session = Session::create(ScriptEngineConfig::default(), &context).await?;
  let vm = session.vm_handle();
  eval_bundle(&vm, &bundle).await?;
  let registry = collect_registry(&vm).await?;
  let cwd = Arc::new(root.to_path_buf());
  let bridge = Arc::new(InfoBridge::new(
    Arc::new(TestInfo::new_anonymous()),
    Arc::new(TestModifiers::default()),
    Arc::new(session.deadline()),
    Arc::new(BundleSourceMap::new(bundle.clone(), cwd.clone())),
    cwd,
    Duration::from_secs(30),
    Vec::new(),
  ));
  let mut results = Vec::new();
  for action in actions {
    let result = execute(&session, &registry, &bundle.module_name, bridge.clone(), action).await;
    results.push(match result {
      Ok(value) => json!({ "value": value }),
      Err(error) => json!({ "error": error.to_string() }),
    });
  }
  Ok(json!({ "steps": registry.steps.len(), "hooks": registry.hooks.len(),
    "parameterTypes": registry.param_types.len(),
    "stepFixtureSets": registry.steps.iter().map(|step| step.fixture_set.unwrap_or(0)).collect::<Vec<_>>(), "results": results }))
}

async fn execute(
  session: &Session,
  registry: &CollectedRegistry,
  module: &str,
  bridge: Arc<InfoBridge>,
  action: Action,
) -> Result<Value> {
  let vm = session.vm_handle();
  match action {
    Action::Plan { steps } => {
      let (set, requested) = plan(registry, &steps, &[])?;
      Ok(json!({ "fixtureSet": set, "requested": requested }))
    },
    Action::Begin {
      steps,
      hooks,
      parameters,
    } => {
      let (fixture_set, requested) = plan(registry, &steps, &hooks)?;
      begin_scenario(
        &vm,
        ScenarioSpec {
          world: TestWorldData::default(),
          parameters,
          fixture_set,
          requested,
          source_label: module.into(),
        },
        bridge,
      )
      .await?;
      Ok(Value::Null)
    },
    Action::Step { index, args } => {
      let args: Vec<JsArg> = args.into_iter().map(Into::into).collect();
      Ok(match invoke_step(&vm, index, &args, None, None, module).await {
        Ok(outcome) => json!({ "outcome": format!("{outcome:?}") }),
        Err(error) => json!({ "error": error }),
      })
    },
    Action::Hook { index, argument } => {
      let argument = argument.map(|arg| HookArg {
        name: arg.name,
        tags: arg.tags,
        status: arg.status,
        message: arg.message,
      });
      let outcome = invoke_hook(&vm, index, argument.as_ref(), module).await?;
      Ok(json!({ "outcome": format!("{outcome:?}") }))
    },
    Action::Drain => Ok(Value::Array(
      drain_attachments(&vm)
        .await?
        .into_iter()
        .map(|attachment| json!({ "bytes": attachment.bytes, "mediaType": attachment.media_type }))
        .collect(),
    )),
    Action::End => {
      end_scenario(&vm).await?;
      Ok(Value::Null)
    },
    Action::TeardownWorker => {
      teardown_worker_fixtures(&vm).await?;
      Ok(Value::Null)
    },
  }
}

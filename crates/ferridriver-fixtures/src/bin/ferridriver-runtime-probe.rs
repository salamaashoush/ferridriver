#[path = "runtime_probe/bdd.rs"]
mod bdd;
#[path = "runtime_probe/extensions.rs"]
mod extensions;
#[path = "runtime_probe/http.rs"]
mod http;
#[path = "runtime_probe/test_registry.rs"]
mod test_registry;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use ferridriver_script::bundle::{BundlerEnv, bytecode_cache, set_bundler_env};
use ferridriver_script::{
  CompiledBundleExt, ExtensionHost, InMemoryVars, RunContext, RunOptions, ScriptCaps, ScriptEngineConfig, Session,
  bundle_and_compile,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", rename_all_fields = "camelCase")]
enum Operation {
  HttpRequest {
    #[serde(flatten)]
    request: http::Request,
  },
  ExecuteVirtualScript {
    source: String,
  },
  SessionState,
  CompileExtensions {
    #[serde(flatten)]
    request: extensions::CompileRequest,
  },
  ExtensionSession {
    #[serde(flatten)]
    request: extensions::Request,
  },
  TestRegistry {
    entries: Vec<PathBuf>,
    host: String,
  },
  BddSession {
    entries: Vec<PathBuf>,
    actions: Vec<bdd::Action>,
  },
  GateExtensions {
    entries: Vec<PathBuf>,
    host: String,
  },
  LoadExtensions {
    entries: Vec<PathBuf>,
    #[serde(default)]
    include_manifests: bool,
  },
  RunSession {
    source: String,
  },
  ConvertValue {
    value: ferridriver::protocol::SerializedValue,
    expression: Option<String>,
  },
  LoadBdd {
    globs: Vec<String>,
    allow_env: Option<Vec<String>>,
  },
  BddRegistry {
    globs: Vec<String>,
    extensions: Vec<String>,
    queries: Vec<String>,
  },
  ExpandFeature {
    source: String,
    title_format: Option<String>,
  },
  BddPlan {
    config: Box<ferridriver_config::test::TestConfig>,
  },
  RuntimeContract,
  ConfigureContext {
    host: Option<String>,
    script_root: Option<PathBuf>,
    artifacts: Option<PathBuf>,
    #[serde(default)]
    commands: std::collections::BTreeMap<String, ferridriver_script::CommandSpec>,
  },
  RunEngine {
    source: String,
    #[serde(default)]
    args: Vec<Value>,
    timeout_ms: Option<u64>,
  },
  ReadVar {
    name: String,
  },
  SetBundler {
    #[serde(default)]
    config: ferridriver_config::BundlerConfig,
    tsconfig: Option<String>,
  },
  Bundle {
    entries: Vec<PathBuf>,
  },
  BundleSource {
    entries: Vec<PathBuf>,
  },
  ExecuteModule {
    entries: Vec<PathBuf>,
  },
  ExecuteScript {
    source: String,
    #[serde(default)]
    args: Vec<Value>,
  },
  SetAliases {
    aliases: Vec<(String, String)>,
  },
  Aliases,
  CacheInfo,
  CacheStore {
    key: u64,
    bytecode: Vec<u8>,
    module_name: String,
    source_map: Option<String>,
    aux: Option<String>,
    inputs: Vec<PathBuf>,
  },
  CacheLoad {
    key: u64,
  },
  InputsFingerprint {
    inputs: Vec<PathBuf>,
  },
  WriteFile {
    path: PathBuf,
    content: String,
  },
}

struct Probe {
  root: PathBuf,
  context: RunContext,
  session: Option<Session>,
  sessions: ferridriver_script::SessionTable,
}

fn paths(root: &Path, entries: Vec<PathBuf>) -> Vec<PathBuf> {
  entries.into_iter().map(|path| root.join(path)).collect()
}

fn cached_entry(key: u64) -> Value {
  bytecode_cache().load(key).map_or(Value::Null, |entry| {
    json!({
      "bytecode": entry.bytecode,
      "moduleName": entry.module_name,
      "sourceMap": entry.source_map_json,
      "aux": entry.aux,
      "inputFingerprint": ferrijs_bundle::cache::inputs_fingerprint(&entry.inputs).map(|value| value.to_string()),
      "inputs": entry.inputs,
    })
  })
}

fn extension_host(name: &str) -> Result<ExtensionHost> {
  ExtensionHost::ALL
    .iter()
    .copied()
    .find(|host| host.as_str() == name)
    .with_context(|| format!("unknown extension host: {name}"))
}

fn gate_extensions(root: &Path, entries: Vec<PathBuf>, host: &str) -> Result<Value> {
  let caps = ScriptCaps::default();
  let sidecars = Vec::new();
  let env = ferridriver_script::RequirementEnv::from_caps(&caps, &sidecars);
  let specs: Vec<_> = paths(root, entries)
    .into_iter()
    .map(|entry| ferridriver_config::ExtensionSpec {
      spec: entry.display().to_string(),
      base_dir: root.into(),
    })
    .collect();
  let gated = ferridriver_script::gate(&specs, &env, extension_host(host)?);
  Ok(json!({ "files": gated.files, "blocked": gated.blocked }))
}

impl Probe {
  fn configure_context(
    &mut self,
    host: Option<String>,
    script_root: Option<PathBuf>,
    artifacts: Option<PathBuf>,
    commands: std::collections::BTreeMap<String, ferridriver_script::CommandSpec>,
  ) -> Result<Value> {
    if let Some(host) = host {
      self.context.host = extension_host(&host)?;
    }
    if let Some(path) = script_root {
      self.context.script_root = self.root.join(path);
    }
    if let Some(path) = artifacts {
      let path = self.root.join(path);
      std::fs::create_dir_all(&path)?;
      self.context.artifacts = Some(Arc::new(ferridriver_script::OutputDir::new(path)?));
    }
    self.context.caps.commands = commands;
    Ok(Value::Null)
  }

  async fn execute_module(&mut self, entries: Vec<PathBuf>) -> Result<Value> {
    let bundle = bundle_and_compile(&paths(&self.root, entries), &self.root).await?;
    if self.session.is_none() {
      self.session = Some(Session::create(ScriptEngineConfig::default(), &self.context).await?);
    }
    let session = self.session.as_ref().context("session was not initialized")?;
    let execution = session
      .execute_module(&bundle, &[], RunOptions::default(), &self.context)
      .await;
    Ok(serde_json::to_value(execution.result)?)
  }

  async fn load_extensions(&mut self, entries: Vec<PathBuf>, include_manifests: bool) -> Result<Value> {
    let (compiled, failures) = ferridriver_script::compile_and_extract_extensions(
      &[paths(&self.root, entries)],
      &ferridriver_config::ExtensionPolicyConfig::default(),
    )
    .await;
    let manifests = compiled
      .iter()
      .map(|extension| serde_json::from_str::<Value>(&extension.manifests_json()))
      .collect::<serde_json::Result<Vec<_>>>()?;
    self.context.extensions = compiled
      .into_iter()
      .map(|compiled| ferridriver_script::ExtensionBinding {
        bytecode: compiled.bytecode,
        name: compiled.path.display().to_string(),
        source_map: None,
        provides: None,
      })
      .collect();
    self.context.host = ExtensionHost::Mcp;
    let mut result = json!({ "failures": failures, "count": self.context.extensions.len() });
    if include_manifests {
      result["manifests"] = serde_json::to_value(manifests)?;
    }
    Ok(result)
  }

  async fn run_session(&self, source: &str) -> Result<Value> {
    let slot = self.sessions.acquire("s");
    let result = slot
      .lock()
      .await
      .run(
        ScriptEngineConfig::default(),
        source,
        &[],
        RunOptions::default(),
        self.context.clone(),
        None,
      )
      .await;
    Ok(serde_json::to_value(result)?)
  }

  async fn execute_script(&mut self, source: &str, args: &[Value]) -> Result<Value> {
    if self.session.is_none() {
      self.session = Some(Session::create(ScriptEngineConfig::default(), &self.context).await?);
    }
    let session = self.session.as_ref().context("session was not initialized")?;
    let execution = session
      .execute(source, args, RunOptions::default(), &self.context)
      .await;
    Ok(serde_json::to_value(execution.result)?)
  }

  async fn execute_virtual_script(&mut self, source: &str) -> Result<Value> {
    if self.session.is_none() {
      self.session = Some(Session::create(ScriptEngineConfig::default(), &self.context).await?);
    }
    tokio::time::pause();
    let started = tokio::time::Instant::now();
    let result = self.execute_script(source, &[]).await;
    let elapsed = started.elapsed();
    tokio::time::resume();
    Ok(json!({ "result": result?, "elapsedMs": elapsed.as_millis() }))
  }

  async fn collect_test_registry(&self, entries: Vec<PathBuf>, host: &str) -> Result<Value> {
    test_registry::collect(
      &self.root,
      &self.context,
      paths(&self.root, entries),
      extension_host(host)?,
    )
    .await
  }

  async fn run(&mut self, operation: Operation) -> Result<Value> {
    match operation {
      Operation::HttpRequest { request } => http::run(request).await,
      Operation::ExecuteVirtualScript { source } => self.execute_virtual_script(&source).await,
      Operation::SessionState => Ok(json!({ "poisoned": self.session.as_ref().map(Session::poisoned) })),
      Operation::CompileExtensions { request } => {
        Box::pin(extensions::compile(&self.root, &mut self.context, request)).await
      },
      Operation::ExtensionSession { request } => Box::pin(extensions::run(&self.root, &self.context, request)).await,
      Operation::TestRegistry { entries, host } => self.collect_test_registry(entries, &host).await,
      Operation::BddSession { entries, actions } => {
        Box::pin(bdd::run(&self.root, &self.context, paths(&self.root, entries), actions)).await
      },
      Operation::GateExtensions { entries, host } => gate_extensions(&self.root, entries, &host),
      Operation::LoadExtensions {
        entries,
        include_manifests,
      } => self.load_extensions(entries, include_manifests).await,
      Operation::RunSession { source } => self.run_session(&source).await,
      Operation::ConvertValue { value, expression } => convert_value(value, expression).await,
      Operation::LoadBdd { globs, allow_env } => load_bdd(&self.root, &globs, allow_env.as_deref()).await,
      Operation::BddRegistry {
        globs,
        extensions,
        queries,
      } => Box::pin(bdd_registry(&self.root, &globs, extensions, &queries)).await,
      Operation::ExpandFeature { source, title_format } => expand_feature(&source, title_format),
      Operation::BddPlan { config } => bdd_plan(&config).await,
      Operation::RuntimeContract => Ok(json!({
        "contributionPoints": ferridriver_script::CONTRIBUTION_POINTS,
        "hosts": ExtensionHost::ALL.iter().map(|host| host.as_str()).collect::<Vec<_>>(),
        "manifestHosts": ferridriver_config::extension_manifest::EXTENSION_HOSTS,
      })),
      Operation::ConfigureContext {
        host,
        script_root,
        artifacts,
        commands,
      } => self.configure_context(host, script_root, artifacts, commands),
      Operation::RunEngine {
        source,
        args,
        timeout_ms,
      } => run_engine(&self.context, &source, &args, timeout_ms).await,
      Operation::ReadVar { name } => Ok(json!(self.context.vars.get(&name))),
      Operation::SetBundler { config, tsconfig } => {
        let env = BundlerEnv::from_config(&config, &self.root).with_tsconfig(tsconfig.as_deref(), &self.root);
        let fingerprint = env.fingerprint().to_string();
        set_bundler_env(env);
        Ok(json!({ "fingerprint": fingerprint }))
      },
      Operation::Bundle { entries } => {
        let bundle = bundle_and_compile(&paths(&self.root, entries), &self.root).await?;
        Ok(json!({ "inputs": bundle.source_files(&self.root), "moduleName": bundle.module_name }))
      },
      Operation::BundleSource { entries } => {
        let bundle = ferridriver_script::bundle::bundle_source(&paths(&self.root, entries), &self.root).await?;
        Ok(json!({ "configInputs": bundle.config_inputs, "modules": bundle.modules, "code": bundle.code }))
      },
      Operation::ExecuteModule { entries } => self.execute_module(entries).await,
      Operation::ExecuteScript { source, args } => self.execute_script(&source, &args).await,
      Operation::SetAliases { aliases } => {
        ferridriver_script::set_module_aliases(aliases).map_err(anyhow::Error::msg)?;
        Ok(json!(ferridriver_script::module_aliases()))
      },
      Operation::Aliases => Ok(json!(ferridriver_script::module_aliases())),
      Operation::CacheInfo => {
        let cache = bytecode_cache();
        Ok(json!({ "enabled": cache.is_enabled(), "directory": cache.dir() }))
      },
      Operation::CacheStore {
        key,
        bytecode,
        module_name,
        source_map,
        aux,
        inputs,
      } => {
        bytecode_cache().store(
          key,
          &bytecode,
          &module_name,
          source_map.as_deref(),
          aux.as_deref(),
          &paths(&self.root, inputs),
        );
        Ok(Value::Null)
      },
      Operation::CacheLoad { key } => Ok(cached_entry(key)),
      Operation::InputsFingerprint { inputs } => Ok(json!(
        ferrijs_bundle::cache::inputs_fingerprint(&paths(&self.root, inputs)).map(|value| value.to_string())
      )),
      Operation::WriteFile { path, content } => {
        std::fs::write(self.root.join(path), content)?;
        Ok(Value::Null)
      },
    }
  }
}

async fn convert_value(value: ferridriver::protocol::SerializedValue, expression: Option<String>) -> Result<Value> {
  use ferridriver_script::bindings::convert::{quickjs_arg_to_serialized, serialized_value_to_quickjs};
  use ferridriver_script::rquickjs::{AsyncContext, AsyncRuntime};

  let runtime = AsyncRuntime::new()?;
  let context = AsyncContext::full(&runtime).await?;
  context
    .async_with(async |ctx| {
      ferridriver_script::ferrijs::std::url::init(&ctx)?;
      let js = serialized_value_to_quickjs(&ctx, &value)?;
      ctx.globals().set("__v", js.clone())?;
      let probe = expression
        .map(|expression| ctx.eval::<String, _>(format!("String({expression})")))
        .transpose()?;
      let back = quickjs_arg_to_serialized(&ctx, Some(js))?;
      Ok(json!({ "serialized": back.value, "handles": back.handles, "probe": probe }))
    })
    .await
}

fn expand_feature(source: &str, title_format: Option<String>) -> Result<Value> {
  let features = ferridriver_bdd::feature::FeatureSet::parse_text(source).map_err(anyhow::Error::msg)?;
  let options = ferridriver_bdd::scenario::ExpandOptions {
    examples_title_format: title_format,
    ..Default::default()
  };
  Ok(json!(features.features.iter().flat_map(|feature| {
          ferridriver_bdd::scenario::expand_feature_with(feature, &options).into_iter().map(|scenario| json!({
            "name": scenario.name,
            "describePath": scenario.describe_path,
            "tags": scenario.tags,
            "steps": scenario.steps.iter().map(|step| json!({ "keyword": step.keyword, "text": step.text })).collect::<Vec<_>>(),
            "source": scenario.source,
          }))
        }).collect::<Vec<_>>()))
}

async fn bdd_plan(config: &ferridriver_config::test::TestConfig) -> Result<Value> {
  let (plan, projects) = Box::pin(ferridriver_bdd::build_bdd_plans(config, &[], &[], None))
    .await
    .map_err(anyhow::Error::msg)?;
  let projects = projects
    .into_iter()
    .map(|(name, plan)| {
      let mut names = plan
        .suites
        .iter()
        .flat_map(|suite| suite.tests.iter().map(|test| test.id.name.clone()))
        .collect::<Vec<_>>();
      names.sort();
      (name, names)
    })
    .collect::<std::collections::BTreeMap<_, _>>();
  let mut names = plan
    .suites
    .iter()
    .flat_map(|suite| suite.tests.iter().map(|test| test.id.name.clone()))
    .collect::<Vec<_>>();
  names.sort();
  Ok(json!({ "totalTests": plan.total_tests, "names": names, "projects": projects }))
}

async fn bdd_registry(root: &Path, globs: &[String], extensions: Vec<String>, queries: &[String]) -> Result<Value> {
  use ferridriver_bdd::js::{BddSessionSetup, JsBddSession, bundle_steps_with};
  let specs = extensions
    .into_iter()
    .map(|spec| ferridriver_script::ExtensionSpec {
      spec,
      base_dir: root.to_path_buf(),
    })
    .collect::<Vec<_>>();
  let bundle = bundle_steps_with(globs, &specs, root).await?;
  let caps = ScriptCaps::default();
  let env = ferridriver_script::RequirementEnv::from_caps(&caps, &[]);
  let bindings = ferridriver_script::load_bindings(&specs, &env, &caps.extension_policy, ExtensionHost::Bdd).await;
  let session = JsBddSession::load(
    bundle,
    root,
    &BddSessionSetup {
      extensions: Arc::new(bindings),
      ..Default::default()
    },
  )
  .await?;
  let registry = session.registry();
  Ok(json!({
    "patterns": registry.steps().iter().map(|step| &step.expression).collect::<Vec<_>>(),
    "matches": queries.iter().map(|query| (query, registry.find_match(query).is_ok())).collect::<std::collections::BTreeMap<_, _>>(),
  }))
}

async fn load_bdd(root: &Path, globs: &[String], allow_env: Option<&[String]>) -> Result<Value> {
  if let Some(names) = allow_env {
    ferridriver_bdd::js::set_bdd_script_caps(ScriptCaps::resolve(names));
  }
  let session = ferridriver_bdd::js::JsBddSession::from_globs(globs, root).await?;
  Ok(json!(
    session
      .registry()
      .steps()
      .iter()
      .map(|step| &step.expression)
      .collect::<Vec<_>>()
  ))
}

async fn run_engine(context: &RunContext, source: &str, args: &[Value], timeout_ms: Option<u64>) -> Result<Value> {
  let engine = ferridriver_script::ScriptEngine::new(ScriptEngineConfig::default());
  let options = RunOptions {
    timeout: timeout_ms.map(std::time::Duration::from_millis),
    ..RunOptions::default()
  };
  Ok(serde_json::to_value(
    engine.run(source, args, options, context.clone()).await,
  )?)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
  let mut input = String::new();
  std::io::stdin().read_to_string(&mut input)?;
  let operations: Vec<Operation> = serde_json::from_str(&input)?;
  let root = std::env::current_dir()?;
  let context = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    script_root: root.clone(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ExtensionHost::Script,
    caps: ScriptCaps::default(),
    session: None,
  };
  let mut probe = Probe {
    root,
    context,
    session: None,
    sessions: ferridriver_script::SessionTable::new(8, None),
  };
  let mut results = Vec::with_capacity(operations.len());
  for operation in operations {
    results.push(match Box::pin(probe.run(operation)).await {
      Ok(value) => json!({ "value": value }),
      Err(error) => json!({ "error": error.to_string() }),
    });
  }
  println!("{}", serde_json::to_string(&results)?);
  Ok(())
}

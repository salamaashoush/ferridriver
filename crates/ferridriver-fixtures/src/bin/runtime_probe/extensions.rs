use std::path::Path;

use anyhow::Result;
use ferridriver_script::{
  ExtensionHost, ExtensionSpec, RequirementEnv, RunContext, RunOptions, ScriptEngineConfig, Session,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct Request {
  entries: Vec<String>,
  host: String,
  sources: Vec<String>,
}

pub async fn run(root: &Path, context: &RunContext, request: Request) -> Result<Value> {
  let specs = request
    .entries
    .into_iter()
    .map(|spec| ExtensionSpec {
      spec,
      base_dir: root.into(),
    })
    .collect::<Vec<_>>();
  let sidecars = Vec::new();
  let env = RequirementEnv::from_caps(&context.caps, &sidecars);
  let bindings =
    ferridriver_script::load_bindings(&specs, &env, &context.caps.extension_policy, ExtensionHost::Script).await;
  let (gated, compiled, failures) =
    ferridriver_script::extension_load::load(&specs, &env, &context.caps.extension_policy, ExtensionHost::Script).await;
  let mut result = json!({
    "blocked": gated.blocked,
    "failures": failures,
    "compiled": compiled.iter().map(|extension| json!({
      "snapshot": extension.snapshot,
      "manifests": extension.manifests_json(),
    })).collect::<Vec<_>>(),
    "bindings": bindings.iter().map(|binding| json!({
      "hasSourceMap": binding.source_map.is_some(),
    })).collect::<Vec<_>>(),
  });
  let mut context = context.clone();
  context.host = super::extension_host(&request.host)?;
  context.extensions = bindings;
  let session = Session::create(ScriptEngineConfig::default(), &context).await?;
  let mut outcomes = Vec::new();
  for source in request.sources {
    let execution = session.execute(&source, &[], RunOptions::default(), &context).await;
    outcomes.push(serde_json::to_value(execution.result)?);
  }
  result["outcomes"] = json!(outcomes);
  Ok(result)
}

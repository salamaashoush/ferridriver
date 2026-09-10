use std::path::{Path, PathBuf};

use anyhow::Result;
use ferridriver_script::{
  ExtensionHost, ExtensionSpec, RequirementEnv, RunContext, RunOptions, ScriptEngineConfig, Session,
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
  entries: Vec<String>,
  host: String,
  sources: Vec<String>,
  #[serde(default)]
  modules: Vec<PathBuf>,
  #[serde(default)]
  resolve: Vec<String>,
  #[serde(default)]
  http_client: bool,
  load_policy: Option<ferridriver_config::ExtensionPolicyConfig>,
  session_policy: Option<ferridriver_config::ExtensionPolicyConfig>,
}

#[derive(Deserialize)]
pub struct CompileRequest {
  groups: Vec<Vec<PathBuf>>,
  #[serde(default)]
  append: bool,
  #[serde(default)]
  policy: ferridriver_config::ExtensionPolicyConfig,
}

pub async fn compile(root: &Path, context: &mut RunContext, request: CompileRequest) -> Result<Value> {
  let groups = request
    .groups
    .into_iter()
    .map(|group| super::paths(root, group))
    .collect::<Vec<_>>();
  let (compiled, failures) = ferridriver_script::compile_and_extract_extensions(&groups, &request.policy).await;
  let result = json!({
    "failures": failures,
    "compiled": compiled.iter().map(|extension| json!({
      "snapshot": extension.snapshot,
      "manifests": extension.manifests_json(),
    })).collect::<Vec<_>>(),
  });
  if !request.append {
    context.extensions.clear();
  }
  context.extensions.extend(
    compiled
      .into_iter()
      .map(|compiled| ferridriver_script::ExtensionBinding {
        bytecode: compiled.bytecode,
        name: compiled.path.display().to_string(),
        source_map: None,
        provides: None,
      }),
  );
  Ok(result)
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
  let policy = request
    .load_policy
    .unwrap_or_else(|| context.caps.extension_policy.clone());
  let bindings = ferridriver_script::load_bindings(&specs, &env, &policy, ExtensionHost::Script).await;
  let (gated, compiled, failures) =
    ferridriver_script::extension_load::load(&specs, &env, &policy, ExtensionHost::Script).await;
  let mut result = json!({
    "loadPolicy": policy,
    "blocked": gated.blocked,
    "issues": gated.issues.iter().map(|issue| json!({
      "message": issue.message, "source": issue.source, "blocking": issue.blocking,
    })).collect::<Vec<_>>(),
    "resolved": request.resolve.iter().map(|name| {
      (name.clone(), ferridriver_script::provided_modules::canonical_provided_name(name))
    }).collect::<std::collections::BTreeMap<_, _>>(),
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
  context.caps.extension_policy = request.session_policy.unwrap_or(policy);
  context.host = super::extension_host(&request.host)?;
  context.extensions = bindings;
  if request.http_client {
    context.request = Some(std::sync::Arc::new(ferridriver::http_client::HttpClient::new(
      ferridriver::http_client::HttpClientOptions::default(),
    )));
  }
  let session = Session::create(ScriptEngineConfig::default(), &context).await?;
  let mut outcomes = Vec::new();
  for source in request.sources {
    let execution = session.execute(&source, &[], RunOptions::default(), &context).await;
    outcomes.push(serde_json::to_value(execution.result)?);
  }
  for entry in request.modules {
    let bundle = ferridriver_script::bundle_and_compile(&[root.join(entry)], root).await?;
    let execution = session
      .execute_module(&bundle, &[], RunOptions::default(), &context)
      .await;
    outcomes.push(serde_json::to_value(execution.result)?);
  }
  result["outcomes"] = json!(outcomes);
  Ok(result)
}

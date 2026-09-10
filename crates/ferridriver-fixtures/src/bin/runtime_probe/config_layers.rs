use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use ferridriver_config::layer::{LoadOptions, resolve};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
  cwd: PathBuf,
  explicit: Option<PathBuf>,
  user: Option<PathBuf>,
  machine: Option<PathBuf>,
  #[serde(default)]
  env: BTreeMap<String, String>,
  inherit: bool,
  #[serde(default)]
  defaults: Vec<(String, Value)>,
  module: Option<Value>,
}

pub fn run(root: &Path, request: Request) -> Result<Value> {
  let module_loader = request
    .module
    .map(|value| Arc::new(move |_: &Path| Ok(value.clone())) as ferridriver_config::layer::ModuleLoader);
  let resolved = resolve(&LoadOptions {
    explicit: request.explicit.map(|path| root.join(path)),
    cwd: root.join(request.cwd),
    user_config_dir: request.user.map(|path| root.join(path)),
    machine_config_dir: request.machine.map(|path| root.join(path)),
    env: request.env,
    inherit: request.inherit,
    extension_defaults: request.defaults,
    cache: ferridriver_config::layer::LayerCache::default(),
    module_loader,
    documents_only: false,
  })?;
  let config = &resolved.config;
  let mut effective = config.test.clone();
  effective.browser.apply_use_engine();
  effective.browser.normalize();
  effective.apply_use_options();
  Ok(json!({
    "config": config, "effective": effective,
    "layers": resolved.layers, "warnings": resolved.warnings,
    "provenance": resolved.provenance, "contributors": resolved.contributors,
    "origins": resolved.provenance.iter().map(|(key, origin)| (key.clone(), origin.describe())).collect::<BTreeMap<_, _>>(),
    "serverName": config.mcp.server_name(), "headless": config.mcp.headless(),
    "instructions": config.mcp.server_instructions("base"),
    "chromeArgs": config.mcp.chrome_args(), "extensions": config.extensions.paths(),
    "extensionSpecs": config.extension_specs().iter().map(|spec| json!({ "spec": spec.spec, "baseDir": spec.base_dir })).collect::<Vec<_>>(),
    "projects": config.test.projects.iter().map(|project| config.test.merge_project(project)).collect::<Vec<_>>(),
    "mcpViewport": config.mcp.viewport(),
    "useViewportPresent": config.test.browser.use_options.viewport.is_some(),
  }))
}

pub fn contracts(modes: Vec<(String, u32)>) -> Value {
  use ferridriver_config::test::{TraceMode, VideoMode};
  let modes: Vec<Value> = modes
    .into_iter()
    .map(|(label, attempt)| {
      let trace = TraceMode::parse_label(&label);
      let video = VideoMode::parse_label(&label);
      json!({ "label": label, "attempt": attempt,
        "trace": trace, "video": video,
        "recordTrace": trace.should_record(attempt, false),
        "retainPass": trace.should_retain(false, attempt),
        "retainFail": trace.should_retain(true, attempt),
        "recordVideo": video.should_record(attempt),
        "eagerVideo": video.records_eagerly(attempt),
      })
    })
    .collect();
  json!({ "defaults": ferridriver_config::FerridriverConfig::default(),
    "aliases": ferridriver_config::layer::DOCUMENT_ALIASES, "modes": modes })
}

pub fn cached(root: &Path, path: &Path, updated: &str) -> Result<Value> {
  let file = root.join(path);
  let cache = ferridriver_config::layer::LayerCache::default();
  let first = cache.parse(&file, None)?;
  std::fs::write(&file, updated)?;
  let second = cache.parse(&file, None)?;
  let fresh = ferridriver_config::layer::LayerCache::default().parse(&file, None)?;
  Ok(json!({ "first": first, "second": second, "fresh": fresh }))
}

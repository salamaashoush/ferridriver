use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use ferridriver_test::config::TestConfig;
use ferridriver_test::reporter::{
  Reporter, ReporterSet, RunStatus, base::Out, blob, create_reporters_pub, dot, empty, github,
};
use rustc_hash::FxHashMap;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::reporters::{Collector, observation};

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Mode {
  #[default]
  Factory,
  Dot,
  Empty,
  Github,
  Blob,
  Js,
}

#[derive(Deserialize)]
pub struct Request {
  config: Box<TestConfig>,
  events: Vec<blob::WireEvent>,
  #[serde(default)]
  resources: FxHashMap<String, Vec<u8>>,
  #[serde(default)]
  mode: Mode,
  shard: Option<(u32, u32)>,
  preprocess: Option<ferridriver_test::reporter::api::RunPreamble>,
}

async fn js_reporter(config: &TestConfig) -> Result<Box<dyn Reporter>> {
  let entry = config.reporter.first().context("missing JS reporter")?;
  let module = ferridriver_script::reporter::load(
    entry,
    config,
    &std::env::current_dir()?,
    ferridriver_script::ScriptCaps::default(),
  )
  .await?;
  Ok(Box::new(Arc::new(module).reporter()))
}

pub async fn run(request: Request) -> Result<Value> {
  let (out, captured) = Out::buffer();
  let collected = Arc::new(Mutex::new(Vec::new()));
  let mut direct: Option<Box<dyn Reporter>> = match request.mode {
    Mode::Factory => None,
    Mode::Js => Some(js_reporter(&request.config).await?),
    Mode::Dot => Some(Box::new(dot::DotReporter::new().with_plain_screen().with_output(out))),
    Mode::Empty => Some(Box::new(empty::EmptyReporter)),
    Mode::Github => Some(Box::new(
      github::GithubReporter::new(Box::new(Collector(Arc::clone(&collected))))
        .with_enabled(true)
        .with_output(out),
    )),
    Mode::Blob => {
      let mut reporter = blob::BlobReporter::new(request.config.output_dir.join("report-1.zip"));
      if let Some((current, total)) = request.shard {
        reporter = reporter.with_shard(current, total);
      }
      Some(Box::new(reporter))
    },
  };
  let mut edits = ferridriver_test::reporter::TestRunEdits::default();
  if let Some(preamble) = &request.preprocess {
    direct
      .as_mut()
      .context("preprocess requires a direct reporter")?
      .preprocess(preamble, &mut edits)
      .await
      .map_err(anyhow::Error::msg)?;
  }
  let prints_to_stdio = direct.as_ref().map(|reporter| reporter.prints_to_stdio());
  let mut reporters = if direct.is_some() {
    ReporterSet::default()
  } else {
    create_reporters_pub(&request.config.reporter, &request.config)
  };
  for wire in request.events {
    if let Some(event) = wire.into_runtime_with(&request.resources) {
      match &mut direct {
        Some(reporter) => reporter.on_event(&event).await,
        None => reporters.emit(&event).await,
      }
    }
  }
  match &mut direct {
    Some(reporter) => reporter.finalize().await?,
    None => reporters.finalize().await,
  }
  let events = if matches!(request.mode, Mode::Blob) {
    blob::read_blob_dir(&request.config.output_dir)
      .map_err(anyhow::Error::msg)?
      .iter()
      .map(observation)
      .collect::<Vec<_>>()
  } else {
    Vec::new()
  };
  let text = captured
    .lock()
    .map_err(|_| anyhow!("reporter output lock poisoned"))?
    .clone();
  let status = direct
    .as_ref()
    .and_then(|reporter| reporter.status_override())
    .map(RunStatus::as_str);
  Ok(
    json!({ "text": text, "delegated": *collected.lock().await, "events": events,
    "defaultWorkers": TestConfig::default().workers, "printsToStdio": prints_to_stdio, "status": status,
    "edits": { "excluded": edits.excluded, "annotations": edits.annotations, "skipSharding": edits.skip_sharding } }),
  )
}

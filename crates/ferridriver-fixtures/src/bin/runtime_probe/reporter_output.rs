use std::sync::Arc;

use anyhow::{Result, anyhow};
use ferridriver_test::config::TestConfig;
use ferridriver_test::reporter::{Reporter, ReporterSet, base::Out, blob, create_reporters_pub, dot, empty, github};
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
}

pub async fn run(request: Request) -> Result<Value> {
  let (out, captured) = Out::buffer();
  let collected = Arc::new(Mutex::new(Vec::new()));
  let mut direct: Option<Box<dyn Reporter>> = match request.mode {
    Mode::Factory => None,
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
  Ok(
    json!({ "text": text, "delegated": *collected.lock().await, "events": events,
    "defaultWorkers": TestConfig::default().workers }),
  )
}

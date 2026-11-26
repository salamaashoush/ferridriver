//! CDP `HeapProfiler` domain.

use serde::Serialize;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Enable {}

impl Command for Enable {
  const METHOD: &'static str = "HeapProfiler.enable";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct CollectGarbage {}

impl Command for CollectGarbage {
  const METHOD: &'static str = "HeapProfiler.collectGarbage";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeHeapSnapshot {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub report_progress: Option<bool>,
}

impl Command for TakeHeapSnapshot {
  const METHOD: &'static str = "HeapProfiler.takeHeapSnapshot";
  type Response = EmptyResponse;
}

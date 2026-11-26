//! CDP `Performance` domain.

use serde::{Deserialize, Serialize};

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Enable {}

impl Command for Enable {
  const METHOD: &'static str = "Performance.enable";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetMetrics {}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metric {
  pub name: String,
  pub value: f64,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetMetricsResponse {
  #[serde(default)]
  pub metrics: Vec<Metric>,
}

impl Command for GetMetrics {
  const METHOD: &'static str = "Performance.getMetrics";
  type Response = GetMetricsResponse;
}

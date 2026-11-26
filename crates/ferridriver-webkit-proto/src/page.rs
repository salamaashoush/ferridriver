//! `WebKit` `Page` domain.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Enable {}

impl Command for Enable {
  const METHOD: &'static str = "Page.enable";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Disable {}

impl Command for Disable {
  const METHOD: &'static str = "Page.disable";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reload {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub ignore_cache: Option<bool>,
}

impl Command for Reload {
  const METHOD: &'static str = "Page.reload";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GoBack {}

impl Command for GoBack {
  const METHOD: &'static str = "Page.goBack";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GoForward {}

impl Command for GoForward {
  const METHOD: &'static str = "Page.goForward";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetResourceTree {}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetResourceTreeResponse {
  pub frame_tree: Value,
}

impl Command for GetResourceTree {
  const METHOD: &'static str = "Page.getResourceTree";
  type Response = GetResourceTreeResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetBootstrapScript<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub source: Option<&'a str>,
}

impl Command for SetBootstrapScript<'_> {
  const METHOD: &'static str = "Page.setBootstrapScript";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateUserWorld<'a> {
  pub name: &'a str,
}

impl Command for CreateUserWorld<'_> {
  const METHOD: &'static str = "Page.createUserWorld";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverrideUserAgent<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub value: Option<&'a str>,
}

impl Command for OverrideUserAgent<'_> {
  const METHOD: &'static str = "Page.overrideUserAgent";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTimeZone<'a> {
  pub time_zone: &'a str,
}

impl Command for SetTimeZone<'_> {
  const METHOD: &'static str = "Page.setTimeZone";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetBypassCsp {
  pub enabled: bool,
}

impl Command for SetBypassCsp {
  const METHOD: &'static str = "Page.setBypassCSP";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertText<'a> {
  pub text: &'a str,
}

impl Command for InsertText<'_> {
  const METHOD: &'static str = "Page.insertText";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverrideUserPreference<'a> {
  pub name: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub value: Option<&'a str>,
}

impl Command for OverrideUserPreference<'_> {
  const METHOD: &'static str = "Page.overrideUserPreference";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetEmulatedMedia<'a> {
  pub media: &'a str,
}

impl Command for SetEmulatedMedia<'_> {
  const METHOD: &'static str = "Page.setEmulatedMedia";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetInterceptFileChooserDialog {
  pub enabled: bool,
}

impl Command for SetInterceptFileChooserDialog {
  const METHOD: &'static str = "Page.setInterceptFileChooserDialog";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct HeapGc {}

impl Command for HeapGc {
  const METHOD: &'static str = "Heap.gc";
  type Response = EmptyResponse;
}

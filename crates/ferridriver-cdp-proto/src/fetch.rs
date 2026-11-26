//! CDP `Fetch` domain.

use serde::{Deserialize, Serialize};

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderEntry {
  pub name: String,
  pub value: String,
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPattern<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub url_pattern: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub resource_type: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub request_stage: Option<&'a str>,
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Enable<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub patterns: Option<&'a [RequestPattern<'a>]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub handle_auth_requests: Option<bool>,
}

impl Command for Enable<'_> {
  const METHOD: &'static str = "Fetch.enable";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Disable {}

impl Command for Disable {
  const METHOD: &'static str = "Fetch.disable";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueRequest<'a> {
  pub request_id: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub url: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub method: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub post_data: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub headers: Option<&'a [HeaderEntry]>,
}

impl Command for ContinueRequest<'_> {
  const METHOD: &'static str = "Fetch.continueRequest";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FulfillRequest<'a> {
  pub request_id: &'a str,
  pub response_code: u16,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub response_headers: Option<&'a [HeaderEntry]>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub body: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub response_phrase: Option<&'a str>,
}

impl Command for FulfillRequest<'_> {
  const METHOD: &'static str = "Fetch.fulfillRequest";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailRequest<'a> {
  pub request_id: &'a str,
  pub error_reason: &'a str,
}

impl Command for FailRequest<'_> {
  const METHOD: &'static str = "Fetch.failRequest";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthChallengeResponse<'a> {
  pub response: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub username: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub password: Option<&'a str>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueWithAuth<'a> {
  pub request_id: &'a str,
  pub auth_challenge_response: AuthChallengeResponse<'a>,
}

impl Command for ContinueWithAuth<'_> {
  const METHOD: &'static str = "Fetch.continueWithAuth";
  type Response = EmptyResponse;
}

//! `WebKit` `Playwright` root domain.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct GetInfo {}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetInfoResponse {
  #[serde(default)]
  pub version: String,
}

impl Command for GetInfo {
  const METHOD: &'static str = "Playwright.getInfo";
  type Response = GetInfoResponse;
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateContext {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub proxy_options: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateContextResponse {
  pub browser_context_id: String,
}

impl Command for CreateContext {
  const METHOD: &'static str = "Playwright.createContext";
  type Response = CreateContextResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteContext<'a> {
  pub browser_context_id: &'a str,
}

impl Command for DeleteContext<'_> {
  const METHOD: &'static str = "Playwright.deleteContext";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePage<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePageResponse {
  pub page_proxy_id: String,
}

impl Command for CreatePage<'_> {
  const METHOD: &'static str = "Playwright.createPage";
  type Response = CreatePageResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Navigate<'a> {
  pub url: &'a str,
  pub page_proxy_id: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub frame_id: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub referrer: Option<&'a str>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NavigateResponse {
  #[serde(default)]
  pub loader_id: Option<String>,
}

impl Command for Navigate<'_> {
  const METHOD: &'static str = "Playwright.navigate";
  type Response = NavigateResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetLanguages<'a> {
  pub languages: &'a [&'a str],
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<&'a str>,
}

impl Command for SetLanguages<'_> {
  const METHOD: &'static str = "Playwright.setLanguages";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetDownloadBehavior<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub behavior: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub download_path: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<&'a str>,
}

impl Command for SetDownloadBehavior<'_> {
  const METHOD: &'static str = "Playwright.setDownloadBehavior";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetIgnoreCertificateErrors<'a> {
  pub ignore: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<&'a str>,
}

impl Command for SetIgnoreCertificateErrors<'_> {
  const METHOD: &'static str = "Playwright.setIgnoreCertificateErrors";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAllCookies<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<&'a str>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAllCookiesResponse {
  #[serde(default)]
  pub cookies: Vec<Value>,
}

impl Command for GetAllCookies<'_> {
  const METHOD: &'static str = "Playwright.getAllCookies";
  type Response = GetAllCookiesResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCookies<'a> {
  pub cookies: &'a [Value],
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<&'a str>,
}

impl Command for SetCookies<'_> {
  const METHOD: &'static str = "Playwright.setCookies";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteAllCookies<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub browser_context_id: Option<&'a str>,
}

impl Command for DeleteAllCookies<'_> {
  const METHOD: &'static str = "Playwright.deleteAllCookies";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Close {}

impl Command for Close {
  const METHOD: &'static str = "Playwright.close";
  type Response = EmptyResponse;
}

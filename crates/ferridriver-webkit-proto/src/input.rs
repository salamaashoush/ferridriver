//! `WebKit` `Input`, `Console`, `Screencast`, and `Worker` domains.

use serde::Serialize;

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchKeyEvent<'a> {
  #[serde(rename = "type")]
  pub event_type: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub modifiers: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub text: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub unmodified_text: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub code: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub key: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub windows_virtual_key_code: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub is_keypad: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub auto_repeat: Option<bool>,
}

impl Command for DispatchKeyEvent<'_> {
  const METHOD: &'static str = "Input.dispatchKeyEvent";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchMouseEvent<'a> {
  #[serde(rename = "type")]
  pub event_type: &'a str,
  pub x: f64,
  pub y: f64,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub button: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub buttons: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub click_count: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub modifiers: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub delta_x: Option<f64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub delta_y: Option<f64>,
}

impl Command for DispatchMouseEvent<'_> {
  const METHOD: &'static str = "Input.dispatchMouseEvent";
  type Response = EmptyResponse;
}

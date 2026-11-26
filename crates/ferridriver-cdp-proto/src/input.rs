//! CDP `Input` domain.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseEventType {
  #[serde(rename = "mousePressed")]
  Pressed,
  #[serde(rename = "mouseReleased")]
  Released,
  #[serde(rename = "mouseMoved")]
  Moved,
  #[serde(rename = "mouseWheel")]
  Wheel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MouseButton {
  None,
  Left,
  Middle,
  Right,
  Back,
  Forward,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchMouseEvent {
  #[serde(rename = "type")]
  pub event_type: MouseEventType,
  pub x: f64,
  pub y: f64,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub modifiers: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub button: Option<MouseButton>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub buttons: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub click_count: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub delta_x: Option<f64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub delta_y: Option<f64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub pointer_type: Option<&'static str>,
}

impl Command for DispatchMouseEvent {
  const METHOD: &'static str = "Input.dispatchMouseEvent";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyEventType {
  KeyDown,
  KeyUp,
  RawKeyDown,
  Char,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchKeyEvent<'a> {
  #[serde(rename = "type")]
  pub event_type: KeyEventType,
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
  #[serde(skip_serializing_if = "Option::is_none")]
  pub location: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub commands: Option<&'a [&'a str]>,
}

impl Command for DispatchKeyEvent<'_> {
  const METHOD: &'static str = "Input.dispatchKeyEvent";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InsertText<'a> {
  pub text: &'a str,
}

impl Command for InsertText<'_> {
  const METHOD: &'static str = "Input.insertText";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TouchPoint {
  pub x: f64,
  pub y: f64,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub radius_x: Option<f64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub radius_y: Option<f64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub force: Option<f64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub id: Option<f64>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchTouchEvent<'a> {
  #[serde(rename = "type")]
  pub event_type: &'a str,
  pub touch_points: &'a [TouchPoint],
  #[serde(skip_serializing_if = "Option::is_none")]
  pub modifiers: Option<u32>,
}

impl Command for DispatchTouchEvent<'_> {
  const METHOD: &'static str = "Input.dispatchTouchEvent";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetInterceptDrags {
  pub enabled: bool,
}

impl Command for SetInterceptDrags {
  const METHOD: &'static str = "Input.setInterceptDrags";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchDragEvent<'a> {
  #[serde(rename = "type")]
  pub event_type: &'a str,
  pub x: f64,
  pub y: f64,
  pub data: Value,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub modifiers: Option<u32>,
}

impl Command for DispatchDragEvent<'_> {
  const METHOD: &'static str = "Input.dispatchDragEvent";
  type Response = EmptyResponse;
}

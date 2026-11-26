//! CDP `Emulation` domain.

use serde::Serialize;

use crate::{Command, EmptyResponse};

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenOrientation<'a> {
  #[serde(rename = "type")]
  pub orientation_type: &'a str,
  pub angle: u32,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetDeviceMetricsOverride<'a> {
  pub width: u32,
  pub height: u32,
  pub device_scale_factor: f64,
  pub mobile: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub screen_width: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub screen_height: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub screen_orientation: Option<ScreenOrientation<'a>>,
}

impl Command for SetDeviceMetricsOverride<'_> {
  const METHOD: &'static str = "Emulation.setDeviceMetricsOverride";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct ClearDeviceMetricsOverride {}

impl Command for ClearDeviceMetricsOverride {
  const METHOD: &'static str = "Emulation.clearDeviceMetricsOverride";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaFeature {
  pub name: String,
  pub value: String,
}

#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetEmulatedMedia<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub media: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub features: Option<&'a [MediaFeature]>,
}

impl Command for SetEmulatedMedia<'_> {
  const METHOD: &'static str = "Emulation.setEmulatedMedia";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetUserAgentOverride<'a> {
  pub user_agent: &'a str,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub accept_language: Option<&'a str>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub platform: Option<&'a str>,
}

impl Command for SetUserAgentOverride<'_> {
  const METHOD: &'static str = "Emulation.setUserAgentOverride";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTimezoneOverride<'a> {
  pub timezone_id: &'a str,
}

impl Command for SetTimezoneOverride<'_> {
  const METHOD: &'static str = "Emulation.setTimezoneOverride";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetLocaleOverride<'a> {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub locale: Option<&'a str>,
}

impl Command for SetLocaleOverride<'_> {
  const METHOD: &'static str = "Emulation.setLocaleOverride";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetGeolocationOverride {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub latitude: Option<f64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub longitude: Option<f64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub accuracy: Option<f64>,
}

impl Command for SetGeolocationOverride {
  const METHOD: &'static str = "Emulation.setGeolocationOverride";
  type Response = EmptyResponse;
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct ClearGeolocationOverride {}

impl Command for ClearGeolocationOverride {
  const METHOD: &'static str = "Emulation.clearGeolocationOverride";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetScriptExecutionDisabled {
  pub value: bool,
}

impl Command for SetScriptExecutionDisabled {
  const METHOD: &'static str = "Emulation.setScriptExecutionDisabled";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetTouchEmulationEnabled {
  pub enabled: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub max_touch_points: Option<u32>,
}

impl Command for SetTouchEmulationEnabled {
  const METHOD: &'static str = "Emulation.setTouchEmulationEnabled";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Rgba {
  pub r: u8,
  pub g: u8,
  pub b: u8,
  pub a: f64,
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetDefaultBackgroundColorOverride {
  #[serde(skip_serializing_if = "Option::is_none")]
  pub color: Option<Rgba>,
}

impl Command for SetDefaultBackgroundColorOverride {
  const METHOD: &'static str = "Emulation.setDefaultBackgroundColorOverride";
  type Response = EmptyResponse;
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetFocusEmulationEnabled {
  pub enabled: bool,
}

impl Command for SetFocusEmulationEnabled {
  const METHOD: &'static str = "Emulation.setFocusEmulationEnabled";
  type Response = EmptyResponse;
}

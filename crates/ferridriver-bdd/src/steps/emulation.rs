//! Emulation step definitions: viewport, timezone, locale, color scheme.

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::{given, step};

#[given("I set timezone to {string}")]
async fn set_timezone(world: &mut BrowserWorld, timezone: String) {
  world
    .page()
    .set_timezone(&timezone)
    .await
    .map_err(|e| StepError::from(format!("set timezone: {e}")))?;
}

#[given("I set locale to {string}")]
async fn set_locale(world: &mut BrowserWorld, locale: String) {
  world
    .page()
    .set_locale(&locale)
    .await
    .map_err(|e| StepError::from(format!("set locale: {e}")))?;
}

#[given("I emulate color scheme {string}")]
async fn emulate_color_scheme(world: &mut BrowserWorld, scheme: String) {
  let opts = ferridriver::options::EmulateMediaOptions {
    color_scheme: ferridriver::options::MediaOverride::Set(scheme),
    ..Default::default()
  };
  world
    .page()
    .emulate_media(&opts)
    .await
    .map_err(|e| StepError::from(format!("emulate color scheme: {e}")))?;
}

#[step("I set user agent to {string}")]
async fn set_user_agent(world: &mut BrowserWorld, ua: String) {
  world
    .page()
    .set_user_agent(&ua)
    .await
    .map_err(|e| StepError::from(format!("set user agent: {e}")))?;
}

#[step("I set geolocation to {float},{float}")]
async fn set_geolocation(world: &mut BrowserWorld, lat: f64, lng: f64) {
  world
    .page()
    .set_geolocation(lat, lng, 1.0)
    .await
    .map_err(|e| StepError::from(format!("set geolocation: {e}")))?;
}

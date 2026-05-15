//! Emulation step definitions: viewport, timezone, locale, color scheme.
//!
//! Each step builds a partial [`ferridriver::options::BrowserContextOptions`]
//! bag with just the field it cares about, then calls
//! [`ferridriver::Page::apply_context_options`] — the single entry
//! point for every context-level state mutation.

use crate::step::StepError;
use crate::world::BrowserWorld;
use ferridriver_bdd_macros::{given, step};

async fn apply(
  world: &mut BrowserWorld,
  label: &str,
  opts: ferridriver::options::BrowserContextOptions,
) -> Result<(), StepError> {
  world
    .page()
    .apply_context_options(&opts)
    .await
    .map_err(|e| StepError::wrap(label, e))
}

#[given("I set timezone to {string}")]
async fn set_timezone(world: &mut BrowserWorld, timezone: String) {
  apply(
    world,
    "set timezone",
    ferridriver::options::BrowserContextOptions {
      timezone_id: Some(timezone),
      ..Default::default()
    },
  )
  .await?;
}

#[given("I set locale to {string}")]
async fn set_locale(world: &mut BrowserWorld, locale: String) {
  apply(
    world,
    "set locale",
    ferridriver::options::BrowserContextOptions {
      locale: Some(locale),
      ..Default::default()
    },
  )
  .await?;
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
    .map_err(|e| StepError::wrap("emulate color scheme", e))?;
}

#[step("I set user agent to {string}")]
async fn set_user_agent(world: &mut BrowserWorld, ua: String) {
  apply(
    world,
    "set user agent",
    ferridriver::options::BrowserContextOptions {
      user_agent: Some(ua),
      ..Default::default()
    },
  )
  .await?;
}

#[step("I set geolocation to {float},{float}")]
async fn set_geolocation(world: &mut BrowserWorld, lat: f64, lng: f64) {
  apply(
    world,
    "set geolocation",
    ferridriver::options::BrowserContextOptions {
      geolocation: Some(ferridriver::options::Geolocation {
        latitude: lat,
        longitude: lng,
        accuracy: 1.0,
      }),
      ..Default::default()
    },
  )
  .await?;
}

use serde_json::json;

use super::page::WebDriverPage;
use super::session::Command;
use crate::error::{FerriError, Result};
use crate::options::{BrowserContextOptions, ViewportConfig, ViewportOption};

impl WebDriverPage {
  pub async fn apply_context_options(&self, opts: &BrowserContextOptions) -> Result<()> {
    for (requested, name) in [
      (opts.bypass_csp.is_some(), "bypassCSP"),
      (opts.any_media_override(), "media emulation"),
      (opts.extra_http_headers.is_some(), "extraHTTPHeaders"),
      (opts.geolocation.is_some(), "geolocation"),
      (opts.http_credentials.is_some(), "httpCredentials"),
      (opts.java_script_enabled.is_some(), "javaScriptEnabled"),
      (opts.locale.is_some(), "locale"),
      (opts.offline.is_some(), "offline"),
      (opts.permissions.is_some(), "permissions"),
      (opts.proxy.is_some(), "proxy"),
      (opts.screen.is_some(), "screen"),
      (opts.service_workers.is_some(), "serviceWorkers"),
      (opts.timezone_id.is_some(), "timezoneId"),
      (opts.user_agent.is_some(), "userAgent"),
    ] {
      if requested {
        return Err(FerriError::unsupported(format!(
          "Classic WebDriver cannot change {name} on an existing session"
        )));
      }
    }
    if let Some(ignore) = opts.ignore_https_errors
      && self.capabilities["acceptInsecureCerts"].as_bool() != Some(ignore)
    {
      return Err(FerriError::unsupported(
        "acceptInsecureCerts must be negotiated when creating a WebDriver session",
      ));
    }
    if let Some(viewport) = opts.resolved_viewport() {
      if !self.supports_window_resize() && matches!(opts.viewport, ViewportOption::Default) {
        if opts.device_scale_factor.is_some() || opts.is_mobile.is_some() || opts.has_touch.is_some() {
          return Err(FerriError::unsupported(
            "A mobile Safari session uses its device's native viewport and input capabilities",
          ));
        }
      } else {
        self.emulate_viewport(&viewport).await?;
      }
    }
    Ok(())
  }

  pub async fn emulate_viewport(&self, viewport: &ViewportConfig) -> Result<()> {
    if viewport.is_mobile
      || viewport.has_touch
      || (viewport.device_scale_factor - 1.0).abs() > f64::EPSILON
      || viewport.is_landscape
    {
      return Err(FerriError::unsupported(
        "Classic WebDriver can resize desktop windows but cannot emulate a device or change pixel density",
      ));
    }
    if !self.supports_window_resize() {
      return Err(FerriError::unsupported(
        "This WebDriver session does not support changing the device's window size",
      ));
    }
    let dimensions = self
      .execute_script(
        "return {width:outerWidth-innerWidth,height:outerHeight-innerHeight};",
        vec![],
      )
      .await?;
    let width = dimensions["width"]
      .as_i64()
      .ok_or_else(|| FerriError::protocol("viewport", "missing window width offset"))?;
    let height = dimensions["height"]
      .as_i64()
      .ok_or_else(|| FerriError::protocol("viewport", "missing window height offset"))?;
    self
      .command(Command::post(
        &["window", "rect"],
        json!({"width":viewport.width + width,"height":viewport.height + height}),
      ))
      .await?;
    Ok(())
  }
}

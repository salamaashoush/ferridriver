//! `WebKitPage` cookies and emulation.

use super::*;

impl WebKitPage {
  // ── Cookies ───────────────────────────────────────────────────────────

  /// Browser-context-wide cookie params for the `Playwright.*Cookies`
  /// browser-session commands. The page always carries a concrete
  /// `browserContextId` (the minted default context when the caller didn't
  /// name one), so the cookie store is always scoped to the right context.
  fn cookie_ctx_params(&self) -> Value {
    match &self.context_id {
      Some(ctx) => json!({ "browserContextId": ctx.to_string() }),
      None => json!({}),
    }
  }

  /// # Errors
  ///
  /// Returns an error when the `Playwright.getAllCookies` command fails.
  pub async fn get_cookies(&self) -> Result<Vec<CookieData>> {
    // Context-wide store via the root browser session (mirrors Playwright's
    // `Playwright.getAllCookies`); `Page.getCookies` only returns cookies the
    // current page's origin can see, which breaks context-level reads.
    //
    // A `document.cookie` write leaves the page's process as a message
    // to the network process, and the store read below travels another
    // route there, so under load it could arrive first and miss a cookie
    // the page had just set. Reading the cookie in the page goes the
    // page's own route and comes back after its writes have landed. A
    // page that cannot evaluate just now has no write of its own waiting.
    let _ = self.evaluate("void document.cookie").await;
    let resp = self
      .browser
      .send(
        super::super::protocol::PLAYWRIGHT_GET_ALL_COOKIES,
        self.cookie_ctx_params(),
      )
      .await
      .map_err(conn_err)?;
    let arr = resp
      .get("cookies")
      .and_then(Value::as_array)
      .cloned()
      .unwrap_or_default();
    Ok(arr.iter().map(super::super::events::parse_cookie).collect())
  }

  /// # Errors
  ///
  /// Returns an error when the `Playwright.setCookies` command fails.
  pub async fn set_cookie(&self, cookie: CookieData) -> Result<()> {
    let mut params = self.cookie_ctx_params();
    params["cookies"] = json!([super::super::events::webkit_set_cookie_param(&cookie)]);
    self
      .browser
      .send(super::super::protocol::PLAYWRIGHT_SET_COOKIES, params)
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when reading the cookies with `Playwright.getAllCookies`, clearing them with
  /// `Playwright.deleteAllCookies`, or setting the rest back with `Playwright.setCookies` fails. A
  /// failure after the clear leaves the context with none of its cookies.
  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<()> {
    // WebKit's browser session exposes only `deleteAllCookies`; to drop a
    // single cookie, re-seed the context with every other cookie. Matches the
    // observable effect of Playwright's per-cookie clear path.
    let remaining: Vec<CookieData> = self
      .get_cookies()
      .await?
      .into_iter()
      .filter(|c| !(c.name == name && domain.is_none_or(|d| c.domain == d)))
      .collect();
    self.replace_all_cookies(remaining).await
  }

  /// # Errors
  ///
  /// Returns an error when the `Playwright.deleteAllCookies` command fails.
  pub async fn clear_cookies(&self) -> Result<()> {
    self
      .browser
      .send(
        super::super::protocol::PLAYWRIGHT_DELETE_ALL_COOKIES,
        self.cookie_ctx_params(),
      )
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  /// Clear the context's cookies and set `cookies` as the new full set.
  async fn replace_all_cookies(&self, cookies: Vec<CookieData>) -> Result<()> {
    self.clear_cookies().await?;
    if cookies.is_empty() {
      return Ok(());
    }
    let params_array: Vec<Value> = cookies
      .iter()
      .map(super::super::events::webkit_set_cookie_param)
      .collect();
    let mut params = self.cookie_ctx_params();
    params["cookies"] = json!(params_array);
    self
      .browser
      .send(super::super::protocol::PLAYWRIGHT_SET_COOKIES, params)
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  // ── Emulation ─────────────────────────────────────────────────────────

  /// The timezone already in force on this page, if any.
  fn applied_timezone(&self) -> Option<String> {
    self
      .applied_timezone
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
  }

  /// Whether this page has already committed a document.
  ///
  /// The session's first document is the boundary `WebKit` applies some
  /// overrides at; after it the engine accepts the command and ignores
  /// it. A page holding an execution context has one.
  fn has_committed_document(&self) -> bool {
    self
      .frame_contexts
      .try_read()
      .is_ok_and(|contexts| !contexts.is_empty())
  }

  /// # Errors
  ///
  /// Returns `FerriError::Unsupported` when `timezone_id` would change after the page has a
  /// document, and `FerriError::InvalidArgument` for an unknown permission. Also returns an error
  /// when applying the viewport (see [`Self::emulate_viewport`]) or the
  /// `Network.setExtraHTTPHeaders`, `Emulation.grantPermissions` or `Playwright.setLanguages`
  /// command fails.
  pub async fn apply_context_options(&self, opts: &crate::options::BrowserContextOptions) -> Result<()> {
    // `Page.setTimeZone` is honoured only before a session's first
    // document. WebKit accepts the command afterwards and does nothing
    // with it, so sending it on a live page reports success while the
    // page keeps its old zone — the worst of both. Say so instead;
    // `newContext({ timezoneId })` is the supported path and works.
    let timezone_changes = match (opts.timezone_id.as_deref(), self.applied_timezone()) {
      (Some(wanted), Some(applied)) => wanted != applied,
      (Some(_), None) => true,
      (None, _) => false,
    };
    if timezone_changes && self.has_committed_document() {
      return Err(FerriError::Unsupported(
        "timezoneId cannot be changed after the page has a document on WebKit: the engine honours \
         Page.setTimeZone only before a session's first document. Pass `timezoneId` to newContext() \
         instead."
          .to_string(),
      ));
    }
    // Per-page document-time overrides (userAgent, timezone, locale,
    // JS-disabled, bypassCSP, offline, permissions) are sent inside
    // [`Self::attach`] from the browser's stashed context options, so
    // they're live BEFORE about:blank becomes scriptable. Pipeline the
    // remaining overrides concurrently in FIFO order (`runtime` first so
    // `opts.screen` in `target_session` still wins over viewport screen).
    let futs: Vec<futures::future::BoxFuture<'_, Result<()>>> = vec![
      Box::pin(self.apply_runtime_overrides(opts)),
      Box::pin(async {
        self.apply_proxy_session_overrides(opts).await;
        Ok(())
      }),
      Box::pin(grant_permissions(&self.proxy, opts)),
      Box::pin(self.apply_browser_session_overrides(opts)),
      Box::pin(async {
        self.apply_target_session_overrides(opts).await;
        Ok(())
      }),
    ];
    for res in futures::future::join_all(futs).await {
      res?;
    }
    Ok(())
  }

  /// Backs [`crate::Page::set_http_credentials`]. Mirrors Playwright's
  /// `wkPage.updateHttpCredentials` — `Emulation.setAuthCredentials` on
  /// the page-proxy session; clearing sends an empty credentials list.
  ///
  /// # Errors
  ///
  /// Returns an error when the `Emulation.setAuthCredentials` command fails.
  pub async fn set_http_credentials(&self, creds: Option<Vec<crate::options::HttpCredentials>>) -> Result<()> {
    if let Ok(mut auth) = self.auth_credentials.lock() {
      auth.site = creds.unwrap_or_default();
    }
    self.send_auth_credentials().await
  }

  /// The proxy this page's context sends through; its credentials answer
  /// the proxy's challenges (`proxyCredentials`, which `WebKit` uses for
  /// those and nothing else). The context was already created with the
  /// proxy (`Playwright.createContext({ proxyServer })`).
  ///
  /// # Errors
  ///
  /// Returns an error when the `Emulation.setAuthCredentials` command fails.
  pub async fn set_proxy(&self, proxy: Option<crate::options::ProxyConfig>) -> Result<()> {
    if let Ok(mut auth) = self.auth_credentials.lock() {
      auth.proxy = proxy.as_ref().and_then(crate::options::ProxyConfig::credentials);
    }
    self.send_auth_credentials().await
  }

  async fn send_auth_credentials(&self) -> Result<()> {
    let params = {
      let auth = self
        .auth_credentials
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      let credentials: Vec<Value> = auth
        .site
        .iter()
        .map(|creds| {
          let mut value = json!({ "username": creds.username, "password": creds.password });
          if let Some(origin) = &creds.origin {
            value["origin"] = json!(origin);
          }
          value
        })
        .collect();
      let mut params = json!({ "credentials": credentials });
      if let Some(proxy) = &auth.proxy {
        params["proxyCredentials"] = json!({ "username": proxy.username, "password": proxy.password });
      }
      params
    };
    self
      .proxy_session()
      .send("Emulation.setAuthCredentials", params)
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  async fn apply_runtime_overrides(&self, opts: &crate::options::BrowserContextOptions) -> Result<()> {
    let vp_fut = async {
      if let Some(vp) = opts.resolved_viewport() {
        self.emulate_viewport(&vp).await
      } else {
        Ok(())
      }
    };
    let hdr_fut = async {
      if let Some(h) = opts.extra_http_headers.as_ref() {
        self.set_extra_http_headers(h).await
      } else {
        Ok(())
      }
    };
    let (r_vp, r_hdr) = tokio::join!(vp_fut, hdr_fut);
    r_vp?;
    r_hdr?;
    Ok(())
  }

  async fn apply_target_session_overrides(&self, opts: &crate::options::BrowserContextOptions) {
    let target = self.target_session();
    let mut cmds: Vec<(&'static str, Value)> = Vec::new();
    if let Some(ua) = opts.user_agent.as_deref() {
      cmds.push(("Page.overrideUserAgent", json!({ "value": ua })));
      // A UA claiming an iPhone with `navigator.platform` still reading
      // "MacIntel" is a contradiction every device-sniffing script sees.
      if let Some(platform) = navigator_platform_for(ua) {
        cmds.push(("Page.overridePlatform", json!({ "value": platform })));
      }
    }
    if let Some(tz) = opts.timezone_id.as_deref() {
      cmds.push(("Page.setTimeZone", json!({ "timeZone": tz })));
      *self
        .applied_timezone
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tz.to_string());
    }
    if let Some(screen) = opts.screen {
      cmds.push((
        "Page.setScreenSizeOverride",
        json!({ "width": screen.width, "height": screen.height }),
      ));
    }
    if let Some(o) = opts.offline {
      cmds.push(("Network.setEmulateOfflineState", json!({ "offline": o })));
    }
    if let Some(true) = opts.bypass_csp {
      cmds.push(("Page.setBypassCSP", json!({ "enabled": true })));
    }
    if cmds.is_empty() {
      return;
    }
    let futs = cmds.into_iter().map(|(method, params)| {
      let target = target.clone();
      async move {
        let _ = target.send(method, params).await;
      }
    });
    futures::future::join_all(futs).await;
  }

  async fn apply_proxy_session_overrides(&self, opts: &crate::options::BrowserContextOptions) {
    let js_fut = async {
      if let Some(true) = opts.java_script_enabled.map(|v| !v) {
        let _ = self
          .proxy
          .send_typed(&ferridriver_webkit_proto::emulation::SetJavaScriptEnabled { enabled: false })
          .await;
      }
    };
    // Sent for every page, as `wkPage.updateHttpCredentials` does: with no
    // list at all WebKit leaves a challenge to the embedder, and headless
    // Linux builds then wait on it forever. An empty list lets automation
    // answer, so the 401 surfaces.
    let auth_fut = async {
      let _ = self.set_http_credentials(opts.http_credentials.clone()).await;
    };
    tokio::join!(js_fut, auth_fut);
  }

  async fn apply_browser_session_overrides(&self, opts: &crate::options::BrowserContextOptions) -> Result<()> {
    // The behaviour itself is sent per context when the context is
    // created (`WebKitBrowser::new_context_with_options`); this is the
    // page-side half, so a refused download says which option refused
    // it rather than just `cancelled`.
    if let Some(accept) = opts.accept_downloads {
      self.download_manager.set_accept_downloads(accept);
    }
    let Some(ctx) = &self.context_id else {
      return Ok(());
    };
    let locale_fut = async {
      if let Some(locale) = opts.locale.as_deref() {
        self
          .browser
          .send(
            "Playwright.setLanguages",
            json!({ "browserContextId": ctx.to_string(), "languages": [locale] }),
          )
          .await
          .map_err(conn_err)?;
      }
      Ok::<(), FerriError>(())
    };
    let geo_fut = async {
      if let Some(g) = opts.geolocation {
        let ts: u64 = std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .ok()
          .and_then(|d| u64::try_from(d.as_millis()).ok())
          .unwrap_or(0);
        let _ = self
          .browser
          .send(
            "Playwright.setGeolocationOverride",
            json!({
              "browserContextId": ctx.to_string(),
              "geolocation": {
                "latitude": g.latitude,
                "longitude": g.longitude,
                "accuracy": g.accuracy,
                "timestamp": ts,
              },
            }),
          )
          .await;
      }
    };
    let (r_locale, ()) = tokio::join!(locale_fut, geo_fut);
    r_locale
  }

  /// # Errors
  ///
  /// Returns an error when the `Emulation.setDeviceMetricsOverride` or `Page.setBootstrapScript`
  /// command fails. Failures of the screen-size, orientation, touch and setting overrides are
  /// ignored.
  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<()> {
    let mobile_changed = {
      let mut slot = self
        .emulated_viewport
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      if slot.config.as_ref() == Some(config) {
        return Ok(());
      }
      slot.config = Some(config.clone());
      let changed = slot.bootstrap_mobile != config.is_mobile;
      slot.bootstrap_mobile = config.is_mobile;
      changed
    };
    let target = self.target_session();
    let mut cmds: Vec<futures::future::BoxFuture<'_, Result<()>>> = Vec::with_capacity(12);
    cmds.push(Box::pin(async {
      self
        .proxy
        .send(
          "Emulation.setDeviceMetricsOverride",
          json!({
            "width": config.width,
            "height": config.height,
            // `fixedLayout` is what makes the page lay out against the emulated
            // viewport instead of the window: without it a 390px mobile viewport
            // still laid out at the desktop fallback width of 980px.
            "fixedLayout": config.is_mobile,
            "deviceScaleFactor": if config.device_scale_factor > 0.0 { config.device_scale_factor } else { 1.0 },
          }),
        )
        .await
        .map(|_| ())
        .map_err(conn_err)
    }));
    cmds.push(Box::pin({
      let target = target.clone();
      async move {
        let _ = target
          .send(
            "Page.setScreenSizeOverride",
            json!({ "width": config.width, "height": config.height }),
          )
          .await;
        Ok(())
      }
    }));
    if config.is_mobile {
      let angle = if config.is_landscape || config.width > config.height {
        90
      } else {
        0
      };
      cmds.push(Box::pin(async move {
        let _ = self
          .proxy
          .send_typed(&ferridriver_webkit_proto::emulation::SetOrientationOverride { angle })
          .await;
        Ok(())
      }));
    }
    cmds.push(Box::pin({
      let target = target.clone();
      async move {
        let _ = target
          .send_typed(&ferridriver_webkit_proto::page::SetTouchEmulationEnabled {
            enabled: config.has_touch,
          })
          .await;
        Ok(())
      }
    }));
    for (setting, value) in Self::mobile_settings(config.is_mobile) {
      let target = target.clone();
      cmds.push(Box::pin(async move {
        let _ = target
          .send_typed(&ferridriver_webkit_proto::page::OverrideSetting { setting, value })
          .await;
        Ok(())
      }));
    }
    if mobile_changed {
      cmds.push(Box::pin(self.flush_bootstrap_script()));
    }
    for res in futures::future::join_all(cmds).await {
      res?;
    }
    Ok(())
  }

  /// Settings `WebKit` gates on "is this a mobile device", in Playwright's
  /// `wkPage.ts::_initializeSession` order.
  pub(super) fn mobile_settings(is_mobile: bool) -> [(&'static str, bool); 7] {
    [
      ("DeviceOrientationEventEnabled", is_mobile),
      ("FullScreenEnabled", !is_mobile),
      ("NotificationsEnabled", !is_mobile),
      ("PointerLockEnabled", !is_mobile),
      ("InputTypeMonthEnabled", is_mobile),
      ("InputTypeWeekEnabled", is_mobile),
      ("FixedBackgroundsPaintRelativeToDocument", is_mobile),
    ]
  }

  /// Whether the page is currently emulating a mobile device.
  pub(super) fn is_mobile_emulation(&self) -> bool {
    self
      .emulated_viewport
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .config
      .as_ref()
      .is_some_and(|vp| vp.is_mobile)
  }

  /// Lower the merged emulation state into the protocol command list —
  /// always all five commands, in Playwright's `wkPage._setEmulateMedia`
  /// order. `Page.setEmulatedMedia` resets every active user-preference
  /// override on WPE, so it must go first and the still-active
  /// preferences must be re-asserted after it; a preference removal is
  /// exactly "reset happened and the preference was not re-added" (an
  /// omitted-`value` `overrideUserPreference` on its own has no effect
  /// on a live override). The list shape lets
  /// `handle_provisional_target_created` replay the exact commands on
  /// the post-navigation session.
  pub(crate) fn emulate_media_commands(opts: &crate::options::EmulateMediaOptions) -> Vec<(&'static str, Value)> {
    use crate::options::MediaOverride;
    fn user_pref(name: &str, value: Option<&str>) -> Value {
      // Removal is "value omitted", not `value: null` — the inspector
      // protocol rejects an explicit null (Playwright sends `undefined`,
      // which JSON.stringify drops).
      match value {
        Some(v) => json!({ "name": name, "value": v }),
        None => json!({ "name": name }),
      }
    }
    fn pref_value(field: &MediaOverride, map: fn(&str) -> &str) -> Option<&str> {
      match field {
        MediaOverride::Set(v) => Some(map(v)),
        MediaOverride::Disabled | MediaOverride::Unchanged => None,
      }
    }
    let media = match &opts.media {
      MediaOverride::Set(v) => v.as_str(),
      MediaOverride::Disabled | MediaOverride::Unchanged => "",
    };
    let color = pref_value(&opts.color_scheme, |v| match v {
      "light" => "Light",
      "dark" => "Dark",
      other => other,
    });
    let reduced = pref_value(&opts.reduced_motion, |v| match v {
      "reduce" => "Reduce",
      "no-preference" => "NoPreference",
      other => other,
    });
    let forced = pref_value(&opts.forced_colors, |v| match v {
      "active" => "Active",
      "none" => "None",
      other => other,
    });
    let contrast = pref_value(&opts.contrast, |v| match v {
      "more" => "More",
      "no-preference" => "NoPreference",
      other => other,
    });
    let forced_params = match forced {
      Some(v) => json!({ "forcedColors": v }),
      None => json!({}),
    };
    vec![
      ("Page.setEmulatedMedia", json!({ "media": media })),
      ("Page.overrideUserPreference", user_pref("PrefersColorScheme", color)),
      (
        "Page.overrideUserPreference",
        user_pref("PrefersReducedMotion", reduced),
      ),
      ("Page.setForcedColors", forced_params),
      ("Page.overrideUserPreference", user_pref("PrefersContrast", contrast)),
    ]
  }

  /// # Errors
  ///
  /// Returns an error when a `Page.setEmulatedMedia`, `Page.overrideUserPreference` or
  /// `Page.setForcedColors` command fails; the commands after it are not sent.
  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<()> {
    // Persist the merged state (the core `Page` merges before
    // delegating) so the per-target re-apply on the next navigation's
    // provisional session keeps the overrides alive across the swap.
    {
      let mut slot = self
        .emulated_media
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      if slot.as_ref() == Some(opts) {
        return Ok(());
      }
      *slot = Some(opts.clone());
    }
    let target = self.target_session();
    let futs = Self::emulate_media_commands(opts).into_iter().map(|(method, params)| {
      let target = target.clone();
      async move { target.send(method, params).await.map_err(conn_err) }
    });
    futures::future::try_join_all(futs).await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the `Network.setExtraHTTPHeaders` command fails.
  pub async fn set_extra_http_headers(&self, headers: &rustc_hash::FxHashMap<String, String>) -> Result<()> {
    let map: serde_json::Map<String, Value> = headers
      .iter()
      .map(|(k, v)| (k.clone(), Value::String(v.clone())))
      .collect();
    // Persist so the per-target re-apply (on the provisional session of
    // the next navigation) keeps the headers alive across the session
    // swap — see `Self::extra_http_headers`.
    *self
      .extra_http_headers
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(map.clone());
    let headers_val = Value::Object(map);
    self
      .target_session()
      .send_typed(&ferridriver_webkit_proto::network::SetExtraHttpHeaders { headers: &headers_val })
      .await
      .map_err(conn_err)?;
    Ok(())
  }

  /// Playwright: `wkPage.ts::_clearPermissions`. The override is the
  /// page proxy's, like the grant it undoes.
  ///
  /// # Errors
  ///
  /// Returns an error when the `Emulation.resetPermissions` command fails.
  pub async fn reset_permissions(&self) -> Result<()> {
    self
      .proxy
      .send_typed(&ferridriver_webkit_proto::emulation::ResetPermissions {})
      .await
      .map_err(conn_err)?;
    Ok(())
  }
}

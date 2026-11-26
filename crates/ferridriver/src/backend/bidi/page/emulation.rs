//! `BidiPage` cookies, emulation, tracing, and ref resolution.

use super::{
  AnyElement, BidiPage, CookieData, FerriError, FxHashMap, MetricData, Result, color_scheme_feature,
  context_option_errors, download_behavior_params, json, parse_bidi_cookie, permissions_origin,
};

impl BidiPage {
  // ── Cookies ─────────────────────────────────────────────────────────────

  /// Cookie partition descriptor: the owning user context's storage key
  /// (see the `user_context` field for why not `{type: "context"}`).
  fn cookie_partition(&self) -> serde_json::Value {
    json!({"type": "storageKey", "userContext": &*self.user_context})
  }

  /// # Errors
  ///
  /// Returns an error when `storage.getCookies` fails or its response has no `cookies` array.
  pub async fn get_cookies(&self) -> Result<Vec<CookieData>> {
    let Some(result) = self
      .session
      .try_command("storage.getCookies", json!({"partition": self.cookie_partition()}))
      .await?
    else {
      return self.session.classic_page(&self.context_id)?.get_cookies().await;
    };

    let cookies = result
      .get("cookies")
      .and_then(|v| v.as_array())
      .ok_or_else(|| FerriError::protocol("storage.getCookies", "missing cookies array"))?;

    let mut out = Vec::with_capacity(cookies.len());
    for c in cookies {
      out.push(parse_bidi_cookie(c));
    }
    Ok(out)
  }

  /// # Errors
  ///
  /// Returns an error when the `storage.setCookie` command fails.
  pub async fn set_cookie(&self, cookie: CookieData) -> Result<()> {
    if self.session.lacks("storage.setCookie") {
      return self.session.classic_page(&self.context_id)?.set_cookie(cookie).await;
    }
    // BiDi `storage.setCookie` has no `url` field (unlike CDP), and
    // `domain` is mandatory. Match Playwright by deriving domain/path
    // from `url` when domain/path are not given explicitly.
    let (mut domain, mut path) = (cookie.domain.clone(), cookie.path.clone());
    if let Some(u) = &cookie.url
      && let Ok(parsed) = reqwest::Url::parse(u)
    {
      if domain.is_empty() {
        domain = parsed.host_str().unwrap_or("").to_string();
      }
      if path.is_empty() {
        let p = parsed.path();
        path = if p.is_empty() { "/".to_string() } else { p.to_string() };
      }
    }
    let mut cookie_obj = json!({
      "name": cookie.name,
      "value": {"type": "string", "value": cookie.value},
      "domain": domain,
      "path": path
    });
    if cookie.secure {
      cookie_obj["secure"] = json!(true);
    }
    if cookie.http_only {
      cookie_obj["httpOnly"] = json!(true);
    }
    if let Some(expires) = cookie.expires {
      // Cookie expiry is a Unix timestamp (seconds). Convert to a JSON integer
      // without a direct float-to-int cast by formatting and re-parsing.
      let rounded = expires.round();
      if rounded.is_finite()
        && rounded >= 0.0
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&format!("{rounded:.0}"))
      {
        cookie_obj["expiry"] = v;
      }
    }
    if let Some(ref ss) = cookie.same_site {
      cookie_obj["sameSite"] = json!(ss.as_str().to_lowercase());
    }

    let params = json!({"cookie": cookie_obj, "partition": self.cookie_partition()});
    if self.session.try_command("storage.setCookie", params).await?.is_none() {
      return self.session.classic_page(&self.context_id)?.set_cookie(cookie).await;
    }
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the `storage.deleteCookies` command fails.
  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<()> {
    let mut filter = json!({"name": name});
    if let Some(d) = domain {
      filter["domain"] = json!(d);
    }
    let params = json!({"filter": filter, "partition": self.cookie_partition()});
    if self
      .session
      .try_command("storage.deleteCookies", params)
      .await?
      .is_none()
    {
      return self
        .session
        .classic_page(&self.context_id)?
        .delete_cookie(name, domain)
        .await;
    }
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error when the `storage.deleteCookies` command fails.
  pub async fn clear_cookies(&self) -> Result<()> {
    let params = json!({"partition": self.cookie_partition()});
    if self
      .session
      .try_command("storage.deleteCookies", params)
      .await?
      .is_none()
    {
      return self.session.classic_page(&self.context_id)?.clear_cookies().await;
    }
    Ok(())
  }

  // ── Emulation ───────────────────────────────────────────────────────────

  /// Send this page's download behaviour, allowed or denied.
  ///
  /// Browser-scoped and best-effort in Firefox, the same way Playwright
  /// treats it; the tempdir drop cleans up if the command never landed.
  ///
  /// # Errors
  ///
  /// Returns an error when the `browser.setDownloadBehavior` command fails.
  pub async fn send_download_behavior(&self) -> Result<()> {
    let user_context = self.session.user_contexts.then_some(&*self.user_context);
    self
      .cmd(
        "browser.setDownloadBehavior",
        download_behavior_params(&self.accept_downloads, &self.downloads_subdir, user_context),
      )
      .await
      .map(|_| ())
  }

  /// Apply an emulation override to this page. In a user context of its
  /// own the override goes onto the context as well, where a page the
  /// browser opens there has it for its first request; the default user
  /// context is shared by pages configured apart, so it gets none.
  async fn emulate(&self, method: &'static str, mut params: serde_json::Value) -> Result<()> {
    if &*self.user_context != "default" {
      let mut for_context = params.clone();
      for_context["userContexts"] = json!([&*self.user_context]);
      params["contexts"] = json!([&*self.context_id]);
      let (ctx_res, page_res) = futures::future::join(self.cmd(method, for_context), self.cmd(method, params)).await;
      if let Err(error) = ctx_res {
        tracing::debug!("{method} for user context {}: {error}", self.user_context);
      }
      return page_res.map(|_| ());
    }
    params["contexts"] = json!([&*self.context_id]);
    self.cmd(method, params).await.map(|_| ())
  }

  /// Apply a [`crate::options::BrowserContextOptions`] bag to this
  /// page. Every `BiDi` command is inlined — no per-field helpers
  /// remain. Mirrors Playwright's
  /// `/tmp/playwright/packages/playwright-core/src/server/bidi/bidiBrowser.ts::initialize`
  /// sequence. Unsupported fields return a typed error per field
  /// which is aggregated and surfaced to the caller.
  ///
  /// # Errors
  ///
  /// Returns `FerriError::Backend` listing, per field, each override whose `BiDi` command failed
  /// and each option this backend cannot honour (`bypassCSP` or `hasTouch` set to true). The
  /// remaining fields are still applied.
  pub async fn apply_context_options(&self, opts: &crate::options::BrowserContextOptions) -> Result<()> {
    use futures::future::BoxFuture;

    self.store_network_options(opts);

    let mut tasks: Vec<(&'static str, BoxFuture<'_, Result<()>>)> = Vec::with_capacity(13);
    if let Some(vp) = opts.resolved_viewport() {
      tasks.push(("viewport", Box::pin(async move { self.emulate_viewport(&vp).await })));
    }
    // Playwright drives the same pair from `doUpdateDefaultViewport`
    // (`bidi/bidiBrowser.ts:457-483`): the screen area is whichever of
    // `screen` / `viewport` is set, and `isMobile` only decides the
    // orientation it reports.
    if let Some(screen) = opts.screen.or_else(|| {
      opts.resolved_viewport().map(|vp| crate::options::ScreenSize {
        width: vp.width,
        height: vp.height,
      })
    }) {
      let is_mobile = opts.is_mobile.unwrap_or(false);
      tasks.push(("screen", Box::pin(self.emulate_screen(screen, is_mobile))));
    }
    if let Some(ua) = opts.user_agent.as_deref() {
      tasks.push((
        "userAgent",
        Box::pin(self.emulate("emulation.setUserAgentOverride", json!({"userAgent": ua}))),
      ));
    }
    if let Some(l) = opts.locale.as_deref() {
      tasks.push((
        "locale",
        Box::pin(self.emulate("emulation.setLocaleOverride", json!({"locale": l}))),
      ));
    }
    if let Some(tz) = opts.timezone_id.as_deref() {
      tasks.push((
        "timezoneId",
        Box::pin(self.emulate("emulation.setTimezoneOverride", json!({"timezone": tz}))),
      ));
    }
    if let Some(v) = opts.java_script_enabled {
      tasks.push((
        "javaScriptEnabled",
        Box::pin(self.emulate("emulation.setScriptingEnabled", json!({"enabled": v}))),
      ));
    }
    if let Some(accept) = opts.accept_downloads {
      tasks.push(("acceptDownloads", Box::pin(self.apply_accept_downloads(accept))));
    }
    if let Some(h) = opts.extra_http_headers.as_ref() {
      tasks.push(("extraHTTPHeaders", Box::pin(self.set_extra_http_headers(h))));
    }
    tasks.push((
      "colorScheme",
      Box::pin(async move {
        self.emulate_user_context_color_scheme(&opts.color_scheme).await;
        Ok(())
      }),
    ));
    if let Some(g) = opts.geolocation {
      tasks.push((
        "geolocation",
        Box::pin(self.context_command(
          "emulation.setGeolocationOverride",
          json!({"coordinates": {"latitude": g.latitude, "longitude": g.longitude, "accuracy": g.accuracy}}),
        )),
      ));
    }
    // Playwright's BiDi backend shape (bidiBrowser.ts:406):
    // `networkConditions: { type: 'offline' } | null`.
    if let Some(o) = opts.offline {
      let conditions = if o {
        json!({"type": "offline"})
      } else {
        serde_json::Value::Null
      };
      tasks.push((
        "offline",
        Box::pin(self.context_command(
          "emulation.setNetworkConditions",
          json!({"networkConditions": conditions}),
        )),
      ));
    }
    if let Some(policy) = opts.service_workers {
      tasks.push(("serviceWorkers", Box::pin(self.apply_service_workers(policy))));
    }
    if opts.permissions.is_some() || !opts.origin_permissions.is_empty() {
      tasks.push(("permissions", Box::pin(self.apply_context_permissions(opts))));
    }

    let applied =
      futures::future::join_all(tasks.into_iter().map(|(label, fut)| async move { (label, fut.await) })).await;
    context_option_errors(applied, opts)
  }

  /// Stored synchronously — the network tracker answers
  /// `network.authRequired` from this shared slot. A bag without the
  /// field leaves previously-set credentials alone (clearing rides the
  /// dedicated `set_http_credentials(None)` path, same as the other
  /// backends).
  fn store_network_options(&self, opts: &crate::options::BrowserContextOptions) {
    if let Some(c) = opts.http_credentials.clone()
      && let Ok(mut slot) = self.http_credentials.lock()
    {
      *slot = Some(c);
    }
    if let Some(ignore) = opts.ignore_https_errors
      && let Ok(mut network) = self.network.lock()
    {
      network.ignore_https_errors = ignore;
    }
  }

  /// `method` with `params` scoped to this page's browsing context.
  async fn context_command(&self, method: &'static str, mut params: serde_json::Value) -> Result<()> {
    params["contexts"] = json!([&*self.context_id]);
    self.cmd(method, params).await.map(|_| ())
  }

  /// Record and re-assert: `attach_listeners` sends the same command
  /// with this page's real destination folder, and the two must not
  /// disagree. The re-send here is what makes a `denied` reach the
  /// browser before anything registers interest in downloads, since
  /// Firefox's own default is to allow.
  async fn apply_accept_downloads(&self, accept: bool) -> Result<()> {
    self
      .accept_downloads
      .store(accept, std::sync::atomic::Ordering::Relaxed);
    self.download_manager.set_accept_downloads(accept);
    self.send_download_behavior().await
  }

  async fn apply_service_workers(&self, policy: crate::options::ServiceWorkerPolicy) -> Result<()> {
    if matches!(policy, crate::options::ServiceWorkerPolicy::Block) {
      self
        .add_init_script(
          "if(navigator.serviceWorker){navigator.serviceWorker.register=()=>Promise.reject(new Error('Service workers blocked'))}",
        )
        .await
        .map(|_| ())
    } else {
      Ok(())
    }
  }

  async fn apply_context_permissions(&self, opts: &crate::options::BrowserContextOptions) -> Result<()> {
    if let Some(p) = &opts.permissions {
      if let Ok(mut state) = self.permissions.lock() {
        state.global = crate::permissions::bidi(p);
      }
      // Grant for the current document's origin right away —
      // `grant_permissions` can arrive after the page already
      // navigated. Future navigations re-grant from the event
      // listener (`browsingContext.navigationStarted`).
      let url = self.url().await.ok().flatten().unwrap_or_default();
      self.grant_global_permissions_for_url(&url).await?;
    }
    for (origin, permissions) in &opts.origin_permissions {
      self
        .grant_permissions_for_origin(origin, crate::permissions::bidi(permissions))
        .await?;
    }
    Ok(())
  }

  async fn emulate_screen(&self, screen: crate::options::ScreenSize, is_mobile: bool) -> Result<()> {
    let portrait = is_mobile || screen.height >= screen.width;
    let (kind, angle, natural) = if portrait {
      ("portrait-primary", 0, "portrait")
    } else {
      ("landscape-primary", 0, "landscape")
    };
    let orientation = self.cmd(
      "emulation.setScreenOrientationOverride",
      json!({
        "contexts": [&*self.context_id],
        "screenOrientation": { "natural": natural, "type": kind, "angle": angle },
      }),
    );
    let settings = self.cmd(
      "emulation.setScreenSettingsOverride",
      json!({
        "contexts": [&*self.context_id],
        "screenArea": { "width": screen.width, "height": screen.height },
      }),
    );
    futures::future::try_join(orientation, settings).await.map(|_| ())
  }

  /// The page's own media, layered over this, is applied by
  /// `AnyPage::apply_context_options`. The context's colour scheme also
  /// goes onto its user context, so a page the browser opens there has
  /// it before its first document rather than once it is registered.
  async fn emulate_user_context_color_scheme(&self, color_scheme: &crate::options::MediaOverride) {
    if &*self.user_context == "default" {
      return;
    }
    let scheme = match color_scheme {
      crate::options::MediaOverride::Unchanged => {
        crate::options::MediaOverride::Set(crate::backend::DEFAULT_COLOR_SCHEME.to_owned())
      },
      other => other.clone(),
    };
    let Ok(Some(value)) = color_scheme_feature(&scheme) else {
      return;
    };
    let params = json!({
      "features": { "prefers-color-scheme": value },
      "userContexts": [&*self.user_context],
    });
    if let Err(error) = self
      .session
      .try_command("emulation.setMediaFeaturesOverride", params)
      .await
    {
      tracing::debug!("colour scheme for user context {}: {error}", self.user_context);
    }
  }

  /// Backs [`crate::Page::set_http_credentials`]. The session-wide
  /// `authRequired`-phase intercept (registered at connect) pauses every
  /// auth challenge; the network tracker answers from this shared slot
  /// via `network.continueWithAuth`, mirroring Playwright's
  /// `bidiNetworkManager.ts::_onAuthRequired`. Passing `None` clears the
  /// stored credentials so future challenges are cancelled (the 401
  /// surfaces).
  pub fn set_http_credentials(
    &self,
    creds: Option<Vec<crate::options::HttpCredentials>>,
  ) -> impl std::future::Future<Output = Result<()>> {
    if let Ok(mut slot) = self.http_credentials.lock() {
      *slot = creds;
    }
    std::future::ready(Ok(()))
  }

  /// The proxy this page's context sends through. The user context was
  /// already created with it (`browser.createUserContext({ proxy })`);
  /// this is what answers the proxy's challenges and what a rewritten
  /// route is re-issued through.
  pub fn set_proxy(&self, proxy: Option<crate::options::ProxyConfig>) -> impl std::future::Future<Output = Result<()>> {
    if let Ok(mut network) = self.network.lock() {
      network.proxy = proxy;
    }
    std::future::ready(Ok(()))
  }

  /// # Errors
  ///
  /// Returns an error when the `browsingContext.setViewport` command fails.
  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<()> {
    if self
      .emulated_viewport
      .lock()
      .is_ok_and(|slot| slot.as_ref() == Some(config))
    {
      return Ok(());
    }
    let mut params = json!({
      "context": &*self.context_id,
      "viewport": {
        "width": config.width,
        "height": config.height
      }
    });
    if config.device_scale_factor > 0.0 {
      params["devicePixelRatio"] = json!(config.device_scale_factor);
    }
    self.cmd("browsingContext.setViewport", params).await?;
    if let Ok(mut slot) = self.emulated_viewport.lock() {
      *slot = Some(config.clone());
    }
    Ok(())
  }

  /// # Errors
  ///
  /// Returns `FerriError::Unsupported` when `media`, `reducedMotion`, `forcedColors` or `contrast`
  /// is specified or `colorScheme` is neither light nor dark, and an error when
  /// `emulation.setMediaFeaturesOverride` fails.
  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<()> {
    tokio::task::yield_now().await;
    // `emulation.setMediaFeaturesOverride` is the only media command
    // Firefox has, and of the features it lists it overrides
    // `prefers-color-scheme` alone: the others are refused as an
    // unsupported operation, and the media type has no command at all.
    // Each is refused here before anything is sent, so a bag that cannot
    // be honoured whole changes nothing.
    if opts.media.is_specified() {
      return Err(FerriError::unsupported(
        "BiDi/Firefox does not support `media` emulation — no BiDi protocol command exists for it",
      ));
    }
    if opts.reduced_motion.is_specified() {
      return Err(FerriError::unsupported(
        "BiDi/Firefox does not support `reducedMotion` emulation — Firefox refuses the prefers-reduced-motion media feature override",
      ));
    }
    if opts.forced_colors.is_specified() {
      return Err(FerriError::unsupported(
        "BiDi/Firefox does not support `forcedColors` emulation — Firefox refuses the forced-colors media feature override",
      ));
    }
    if opts.contrast.is_specified() {
      return Err(FerriError::unsupported(
        "BiDi/Firefox does not support `contrast` emulation — Firefox refuses the prefers-contrast media feature override",
      ));
    }
    let Some(value) = color_scheme_feature(&opts.color_scheme)? else {
      return Ok(());
    };
    self
      .cmd(
        "emulation.setMediaFeaturesOverride",
        json!({
          "features": { "prefers-color-scheme": value },
          "contexts": [&*self.context_id],
        }),
      )
      .await
      .map(|_| ())
  }

  /// Direct `BiDi` `network.setExtraHeaders` command. Backs
  /// [`crate::Page::set_extra_http_headers`] (Playwright's public
  /// `page.setExtraHTTPHeaders(headers)`).
  ///
  /// # Errors
  ///
  /// Returns an error when the `network.setExtraHeaders` command fails.
  pub async fn set_extra_http_headers(&self, headers: &FxHashMap<String, String>) -> Result<()> {
    let header_list: Vec<serde_json::Value> = headers
      .iter()
      .map(|(k, v)| {
        json!({
          "name": k,
          "value": {"type": "string", "value": v}
        })
      })
      .collect();

    self
      .cmd(
        "network.setExtraHeaders",
        json!({
          "contexts": [&*self.context_id],
          "headers": header_list
        }),
      )
      .await?;
    Ok(())
  }

  /// Called from [`crate::ContextRef::clear_permissions`]. Resets every
  /// (origin, permission) pair this page granted back to `"prompt"` and
  /// drops the context-wide grant list — mirrors Playwright's
  /// `bidiBrowser.ts::doClearPermissions`.
  ///
  /// # Errors
  ///
  /// Returns an error when a `permissions.setPermission` call resetting a granted permission to
  /// `prompt` fails, and stops at that permission.
  pub async fn reset_permissions(&self) -> Result<()> {
    let granted: Vec<(String, Vec<String>)> = match self.permissions.lock() {
      Ok(mut state) => {
        state.global.clear();
        state.granted.drain().collect()
      },
      Err(_) => Vec::new(),
    };
    for (origin, perms) in granted {
      for perm in perms {
        self.set_permission(&perm, "prompt", &origin).await?;
      }
    }
    Ok(())
  }

  /// One W3C `permissions.setPermission` call (Firefox ships the
  /// permissions extension module — `bidiBrowser.ts::_setPermission`).
  async fn set_permission(&self, permission: &str, state: &str, origin: &str) -> Result<()> {
    self
      .session
      .send_command(
        "permissions.setPermission",
        json!({
          "descriptor": {"name": permission},
          "state": state,
          "origin": origin,
          "userContext": &*self.user_context,
        }),
      )
      .await
      .map(|_| ())
  }

  /// Grant the context-wide permission list for `url`'s origin,
  /// skipping pairs already granted. `BiDi` requires a concrete origin
  /// per grant, so the origin-global context option is materialised
  /// per navigated origin (Playwright's
  /// `doGrantGlobalPermissionsForURL` model). Opaque origins (`data:`,
  /// `about:blank`) never receive grants.
  pub(super) async fn grant_global_permissions_for_url(&self, url: &str) -> Result<()> {
    let Some(origin) = permissions_origin(url) else {
      return Ok(());
    };
    let global = match self.permissions.lock() {
      Ok(state) => state.global.clone(),
      Err(_) => Vec::new(),
    };
    self.grant_permissions_for_origin(&origin, global).await
  }

  /// Grant `permissions` for `origin`, skipping what it already has
  /// (`bidiBrowser.ts::doGrantPermissions`).
  async fn grant_permissions_for_origin(&self, origin: &str, permissions: Vec<String>) -> Result<()> {
    if origin == "null" {
      return Ok(());
    }
    let origin = origin.to_string();
    let missing: Vec<String> = match self.permissions.lock() {
      Ok(state) => {
        let already = state.granted.get(&origin);
        permissions
          .into_iter()
          .filter(|p| !already.is_some_and(|g| g.contains(p)))
          .collect()
      },
      Err(_) => Vec::new(),
    };
    for perm in missing {
      self.set_permission(&perm, "granted", &origin).await?;
      if let Ok(mut state) = self.permissions.lock() {
        state.granted.entry(origin.clone()).or_default().push(perm);
      }
    }
    Ok(())
  }

  // ── Tracing ─────────────────────────────────────────────────────────────

  pub fn start_tracing(&self, _categories: Option<&[String]>) -> impl std::future::Future<Output = Result<()>> {
    let _ = &self.context_id;
    std::future::ready(Err(FerriError::unsupported("Tracing not supported on BiDi backend")))
  }

  pub fn stop_tracing(&self) -> impl std::future::Future<Output = Result<Vec<serde_json::Value>>> {
    let _ = &self.context_id;
    std::future::ready(Err(FerriError::unsupported("Tracing not supported on BiDi backend")))
  }

  pub fn metrics(&self) -> impl std::future::Future<Output = Result<Vec<MetricData>>> {
    let _ = &self.context_id;
    std::future::ready(Err(FerriError::unsupported(
      "Performance metrics not supported on BiDi backend",
    )))
  }

  // ── Ref resolution ──────────────────────────────────────────────────────

  /// # Errors
  ///
  /// Returns an error when the selector engine cannot be injected, and
  /// `FerriError::ElementNotFound` when no element carries the `data-fdref` value.
  pub async fn resolve_backend_node(&self, backend_node_id: i64, _ref_id: &str) -> Result<AnyElement> {
    // Resolve via data-fdref attribute (set during accessibility tree walk)
    self.find_element(&format!("[data-fdref='{backend_node_id}']")).await
  }
}

//! `CdpPage` cookies, viewport/media/context emulation, heap snapshots, and tracing.

use super::*;

impl CdpPage {
  // ---- Cookies ----

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Storage.getCookies`, the target has closed, or the command outlives
  /// its time budget.
  pub async fn get_cookies(&self) -> Result<Vec<CookieData>> {
    // Playwright's `context.cookies()` returns EVERY cookie in the
    // browser context (`Storage.getCookies`), not just the current
    // page's (`Network.getCookies` is frame-URL scoped — a cookie set
    // for another domain would be invisible).
    //
    // `Storage.getCookies` is a browser-target command: on a page
    // session it errors with "browserContextId is only allowed for
    // Browser target", and without `browserContextId` on the root
    // session it returns the DEFAULT context's cookies (missing
    // everything set on a page in an isolated `Target.createBrowserContext`).
    // Send on the root session with explicit `browserContextId` so
    // test-fixture contexts see their own cookies.
    let (sid, cmd) = if let Some(ref ctx_id) = self.browser_context_id {
      (
        None,
        ferridriver_cdp_proto::storage::GetCookies {
          browser_context_id: Some(ctx_id.as_ref()),
        },
      )
    } else {
      (
        self.session_id.as_deref(),
        ferridriver_cdp_proto::storage::GetCookies {
          browser_context_id: None,
        },
      )
    };
    let result = self.transport.send(sid, &cmd).await?;
    Ok(result.cookies.iter().map(cookie_from_cdp).collect())
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Network.setCookie`, the target has closed, or the command outlives
  /// its time budget.
  pub async fn set_cookie(&self, cookie: CookieData) -> Result<()> {
    self.cmd("Network.setCookie", cookie_to_cdp(&cookie)).await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Network.deleteCookies`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<()> {
    let mut params = serde_json::json!({"name": name});
    if let Some(d) = domain {
      params["domain"] = serde_json::json!(d);
    } else if let Ok(Some(url)) = self.url().await {
      params["url"] = serde_json::json!(url);
    }
    self.cmd("Network.deleteCookies", params).await?;
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Storage.getCookies` or `Network.deleteCookies`, the target has
  /// closed, or the command outlives its time budget.
  pub async fn clear_cookies(&self) -> Result<()> {
    // Use Network.getCookies + Network.deleteCookies (session-scoped)
    // instead of Storage.clearCookies (browser-scoped) to correctly
    // clear cookies for this page's context.
    let cookies = self.get_cookies().await?;
    for c in &cookies {
      self
        .cmd(
          "Network.deleteCookies",
          serde_json::json!({
            "name": c.name,
            "domain": c.domain,
            "path": c.path,
          }),
        )
        .await?;
    }
    Ok(())
  }

  // ---- Emulation ----

  /// Apply a [`crate::options::BrowserContextOptions`] bag to this
  /// page. Every protocol command is inlined here — there are no
  /// per-field helper methods. Mirrors Playwright's
  /// `crPage._updateXxx()` family in
  /// `/tmp/playwright/packages/playwright-core/src/server/chromium/crPage.ts:520`
  /// but folded into one call so `ContextRef::new_page` dispatches
  /// once and every command ships in parallel via
  /// [`futures::future::join_all`].
  ///
  /// Any field whose `Option` is `None` is skipped; `MediaOverride`
  /// fields with `Unchanged` are skipped. Errors from individual
  /// commands fold into a single aggregated error listing every
  /// field that failed — matches Playwright's `Promise.all` failure
  /// mode.
  ///
  /// # Errors
  ///
  /// Returns [`FerriError::Backend`] naming every field whose command
  /// failed, after all of them have run.
  pub async fn apply_context_options(&self, opts: &crate::options::BrowserContextOptions) -> Result<()> {
    // Stash the context's screen BEFORE the viewport override goes out:
    // the two travel in one `setDeviceMetricsOverride`, and every later
    // `setViewportSize` reads it back from here.
    if let Some(screen) = opts.screen {
      match self.context_screen.lock() {
        Ok(mut g) => *g = Some(screen),
        Err(p) => *p.into_inner() = Some(screen),
      }
    }

    let mut pending = Vec::new();
    self.queue_emulation_options(opts, &mut pending);
    self.queue_session_options(opts, &mut pending);
    self.queue_throttling_options(opts, &mut pending);
    let results = futures::future::join_all(
      pending
        .into_iter()
        .map(|(label, fut)| async move { (label, fut.await) }),
    )
    .await;

    // Aggregate errors — preserve a single `Unsupported` error directly so
    // callers (e.g. `setCPUThrottling`, `setNetworkConditions`, `setOffline`)
    // observe the typed `FerriError::Unsupported` rather than a wrapped backend
    // string; fold multiple failures into one labelled summary.
    let mut errs: Vec<(&'static str, FerriError)> = results
      .into_iter()
      .filter_map(|(label, r)| r.err().map(|e| (label, e)))
      .collect();
    if errs.is_empty() {
      Ok(())
    } else if errs.len() == 1 && matches!(errs[0].1, FerriError::Unsupported { .. }) {
      Err(errs.remove(0).1)
    } else {
      let summary: Vec<String> = errs.into_iter().map(|(label, e)| format!("{label}: {e}")).collect();
      Err(FerriError::Backend(summary.join("; ")))
    }
  }

  /// Queue the commands for viewport, user agent, locale, timezone,
  /// JavaScript, CSP and TLS, in the order their failures are reported.
  fn queue_emulation_options<'a>(
    &'a self,
    opts: &'a crate::options::BrowserContextOptions,
    pending: &mut Vec<ContextOptionCommand<'a>>,
  ) {
    // `viewport` composes over the `ViewportConfig` helper that
    // `Page::set_viewport_size` also calls, so it keeps a dedicated
    // method. Media is layered with the page's own overrides by
    // `AnyPage::apply_context_options`.
    if let Some(vp) = opts.resolved_viewport() {
      pending.push(("viewport", Box::pin(async move { self.emulate_viewport(&vp).await })));
    }

    // Every remaining field is a direct CDP command — inlined below
    // so no `set_*` helper lives on the backend.
    // `Network.setUserAgentOverride` replaces the whole override, so with
    // a locale the agent rides in the locale's command below. Two
    // concurrent overrides leave whichever lands last, and one carrying
    // only `acceptLanguage` resets the agent to the browser's default.
    if let Some(ua) = opts.user_agent.as_deref().filter(|_| opts.locale.is_none()) {
      pending.push((
        "userAgent",
        Box::pin(self.configure("Network.setUserAgentOverride", serde_json::json!({"userAgent": ua}))),
      ));
    }
    if let Some(l) = opts.locale.as_deref() {
      pending.push((
        "locale",
        Box::pin(async move {
          let _ = self
            .configure("Emulation.setLocaleOverride", serde_json::json!({"locale": l}))
            .await;
          self
            .configure(
              "Network.setUserAgentOverride",
              serde_json::json!({"userAgent": opts.user_agent.as_deref().unwrap_or(""), "acceptLanguage": l}),
            )
            .await
        }),
      ));
    }
    if let Some(tz) = opts.timezone_id.as_deref() {
      pending.push((
        "timezoneId",
        Box::pin(self.configure("Emulation.setTimezoneOverride", serde_json::json!({"timezoneId": tz}))),
      ));
    }
    if let Some(v) = opts.java_script_enabled {
      pending.push((
        "javaScriptEnabled",
        Box::pin(async move {
          self
            .configure("Emulation.setScriptExecutionDisabled", serde_json::json!({"value": !v}))
            .await
            .map_err(|e| map_cdp_unsupported(e, "disabling JavaScript is not supported by this browser"))
        }),
      ));
    }
    if let Some(v) = opts.bypass_csp {
      pending.push((
        "bypassCSP",
        Box::pin(self.configure("Page.setBypassCSP", serde_json::json!({"enabled": v}))),
      ));
    }
    if let Some(v) = opts.ignore_https_errors {
      pending.push((
        "ignoreHTTPSErrors",
        Box::pin(self.configure("Security.setIgnoreCertificateErrors", serde_json::json!({"ignore": v}))),
      ));
    }
  }

  /// Queue the commands for credentials, service workers, downloads,
  /// headers, geolocation, permissions, network conditions and CPU
  /// throttling, in the order their failures are reported.
  fn queue_session_options<'a>(
    &'a self,
    opts: &'a crate::options::BrowserContextOptions,
    pending: &mut Vec<ContextOptionCommand<'a>>,
  ) {
    if let Some(c) = opts.http_credentials.clone() {
      pending.push((
        "httpCredentials",
        Box::pin(async move {
          *self.http_credentials.write().await = Some(c);
          self.ensure_fetch_enabled().await
        }),
      ));
    }
    if opts.service_workers == Some(crate::options::ServiceWorkerPolicy::Block) {
      pending.push((
        "serviceWorkers",
        Box::pin(self.configure(
          "Page.addScriptToEvaluateOnNewDocument",
          serde_json::json!({
            "source": "if(navigator.serviceWorker){navigator.serviceWorker.register=()=>Promise.reject(new Error('Service workers blocked'))}"
          }),
        )),
      ));
    }
    // Record rather than send: `enable_download_behavior` owns the one
    // `Browser.setDownloadBehavior` this context gets, so the lazy latch
    // cannot overwrite an explicit `deny`. Denial is applied eagerly
    // because the browser's own default is to allow, so waiting for
    // something to show interest in downloads would let the first one
    // through; acceptance stays lazy and keeps saving that round trip.
    if let Some(accept) = opts.accept_downloads {
      pending.push((
        "acceptDownloads",
        Box::pin(async move {
          self
            .accept_downloads
            .store(accept, std::sync::atomic::Ordering::Relaxed);
          self.download_manager.set_accept_downloads(accept);
          if accept {
            Ok(())
          } else {
            self.enable_download_behavior().await
          }
        }),
      ));
    }
    if let Some(h) = opts.extra_http_headers.as_ref() {
      let pairs: serde_json::Map<String, serde_json::Value> = h
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
      pending.push((
        "extraHTTPHeaders",
        Box::pin(self.configure("Network.setExtraHTTPHeaders", serde_json::json!({"headers": pairs}))),
      ));
    }
    if let Some(g) = opts.geolocation {
      pending.push((
        "geolocation",
        Box::pin(self.configure(
          "Emulation.setGeolocationOverride",
          serde_json::json!({"latitude": g.latitude, "longitude": g.longitude, "accuracy": g.accuracy}),
        )),
      ));
    }
    if opts.permissions.is_some() || !opts.origin_permissions.is_empty() {
      pending.push((
        "permissions",
        Box::pin(async move {
          if let Some(permissions) = &opts.permissions {
            self.grant_permissions(None, permissions).await?;
          }
          for (origin, permissions) in &opts.origin_permissions {
            self.grant_permissions(Some(origin), permissions).await?;
          }
          Ok(())
        }),
      ));
    }
  }

  /// Queue offline, network-conditions, and CPU-throttling emulation commands.
  fn queue_throttling_options<'a>(
    &'a self,
    opts: &'a crate::options::BrowserContextOptions,
    pending: &mut Vec<ContextOptionCommand<'a>>,
  ) {
    // One command carries both, so each must be sent with the other's
    // current value or setting one would undo the other.
    if opts.offline.is_some() || opts.network_conditions.is_some() {
      let conditions = opts
        .network_conditions
        .unwrap_or(crate::options::NetworkConditions::NONE);
      if self.flavor.is_lightpanda() && opts.network_conditions.is_some() {
        pending.push((
          "offline/networkConditions",
          Box::pin(async { Err(FerriError::unsupported("network conditions emulation needs Chromium")) }),
        ));
      } else {
        pending.push((
          "offline/networkConditions",
          Box::pin(async move {
            self
              .configure(
                "Network.emulateNetworkConditions",
                serde_json::json!({
                  "offline": opts.offline.unwrap_or(false),
                  "latency": conditions.latency,
                  "downloadThroughput": conditions.download_throughput.unwrap_or(-1.0),
                  "uploadThroughput": conditions.upload_throughput.unwrap_or(-1.0),
                }),
              )
              .await
              .map_err(|e| map_cdp_unsupported(e, "offline emulation is not supported by this browser"))
          }),
        ));
      }
    }
    if let Some(rate) = opts.cpu_throttling_rate {
      pending.push((
        "cpuThrottling",
        Box::pin(async move {
          self
            .configure("Emulation.setCPUThrottlingRate", serde_json::json!({ "rate": rate }))
            .await
            .map_err(|e| map_cdp_unsupported(e, "CPU throttling needs Chromium"))
        }),
      ));
    }
  }

  /// Resolve, once, the browser window this page's target lives in.
  ///
  /// `Browser.getWindowForTarget` fails for targets that have no real
  /// window; that is not an error worth surfacing, it just means there
  /// is nothing to resize.
  async fn window_id(&self) -> Option<i64> {
    if let Some(id) = self.window_id.get() {
      return Some(*id);
    }
    let resp = self
      .cmd("Browser.getWindowForTarget", serde_json::json!({}))
      .await
      .ok()?;
    let id = resp.get("windowId").and_then(serde_json::Value::as_i64)?;
    let _ = self.window_id.set(id);
    Some(id)
  }

  /// Size the browser window so its content area is exactly `config`.
  ///
  /// Playwright resizes the window alongside every viewport emulation
  /// (`chromium/crPage.ts::_updateViewport`) and ferridriver did not,
  /// which left a headed browser showing the viewport letterboxed
  /// inside whatever bounds the window happened to have — and a browser
  /// launched against a persistent profile starts at the bounds Chrome
  /// restored from that profile's last run, so the window carried a
  /// stale size across every later session.
  async fn resize_window_for(&self, config: &crate::options::ViewportConfig) {
    if self.flavor.is_lightpanda() {
      return;
    }
    let Some(window_id) = self.window_id().await else {
      return;
    };
    let (inset_w, inset_h) = window_insets(self.headful);
    let bounds = serde_json::json!({
      "windowId": window_id,
      "bounds": {
        "width": config.width + inset_w,
        "height": config.height + inset_h,
      },
    });
    let Err(e) = self.cmd("Browser.setWindowBounds", bounds.clone()).await else {
      return;
    };

    // Chrome refuses to give a size to a window that is maximized,
    // minimized or fullscreen. A persistent profile restores the state
    // it was left in, so a browser someone maximized once comes back
    // maximized and every resize after it fails — normalise and retry
    // rather than leave the viewport unenforced. Done on the failure
    // path so the ordinary case still costs one command.
    let normal = serde_json::json!({ "windowId": window_id, "bounds": { "windowState": "normal" } });
    if let Err(state_err) = self.cmd("Browser.setWindowBounds", normal).await {
      tracing::debug!("could not resize the window for viewport emulation: {e}; {state_err}");
      return;
    }
    if let Err(retry_err) = self.cmd("Browser.setWindowBounds", bounds).await {
      tracing::debug!("could not resize the window for viewport emulation: {retry_err}");
    }
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Emulation.setDeviceMetricsOverride` or
  /// `Emulation.setTouchEmulationEnabled`, the target has closed, or the command outlives its time budget.
  /// Also fails when resizing the window for a headful page fails.
  pub async fn emulate_viewport(&self, config: &crate::options::ViewportConfig) -> Result<()> {
    let screen = match self.context_screen.lock() {
      Ok(g) => *g,
      Err(p) => *p.into_inner(),
    };
    let params = metrics_params_for(config, screen);

    // Skip the `setDeviceMetricsOverride` RTT when params match the
    // last shipped value (Playwright `_metricsOverride` JSON-equality
    // pattern, `crPage.ts:932`). Touch emulation is a separate
    // command and runs through its own idempotent path below — a
    // metrics cache hit must not mask a `has_touch` change.
    let metrics_unchanged = {
      let last = match self.last_metrics_params.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
      };
      last.as_ref() == Some(&params)
    };
    if !metrics_unchanged {
      // Window first, override second — Playwright's ordering
      // (`crPage.ts::_updateViewport`): the embedder resize can clobber
      // the emulated size if it lands after it. Both hang off the same
      // unchanged-metrics guard, so a re-applied viewport costs nothing.
      self.resize_window_for(config).await;
      self
        .configure("Emulation.setDeviceMetricsOverride", params.clone())
        .await?;
      let mut last = match self.last_metrics_params.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
      };
      *last = Some(params);
    }

    if config.has_touch {
      // Pass `maxTouchPoints` explicitly so `navigator.maxTouchPoints`
      // reports a non-zero value (Chrome leaves it at 0 when the
      // param is omitted on some channels). Mirrors Playwright's
      // `crEmulationManager._updateTouch` which uses 5.
      let _ = self
        .configure(
          "Emulation.setTouchEmulationEnabled",
          serde_json::json!({"enabled": true, "maxTouchPoints": 5}),
        )
        .await;
    }
    Ok(())
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Emulation.setEmulatedMedia`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn emulate_media(&self, opts: &crate::options::EmulateMediaOptions) -> Result<()> {
    use crate::options::MediaOverride;
    // `Emulation.setEmulatedMedia` replaces the emulation state per call.
    // Chrome treats a feature listed in `features` with `value: ""` as
    // *still overridden* (the value just happens to be the empty string)
    // — to actually clear an override the feature must be *omitted* from
    // the array. Tested empirically against Chrome 147; the legacy
    // "always include all four features" path Playwright still uses
    // does not reset the override on current Chromium builds and breaks
    // `page.emulateMedia({colorScheme: null})`.
    //
    // Strategy: include features only when they're actively `Set`.
    // `Disabled` and `Unchanged` drop out of the array so the override
    // is cleared. `media` ships an empty string to clear (still
    // honoured by Chrome).
    let mut features: Vec<serde_json::Value> = Vec::with_capacity(4);
    let push_if_set = |features: &mut Vec<serde_json::Value>, name: &str, o: &MediaOverride| {
      if let MediaOverride::Set(v) = o {
        features.push(serde_json::json!({ "name": name, "value": v }));
      }
    };
    push_if_set(&mut features, "prefers-color-scheme", &opts.color_scheme);
    push_if_set(&mut features, "prefers-reduced-motion", &opts.reduced_motion);
    push_if_set(&mut features, "forced-colors", &opts.forced_colors);
    push_if_set(&mut features, "prefers-contrast", &opts.contrast);
    let media = match &opts.media {
      MediaOverride::Set(v) => v.as_str(),
      MediaOverride::Disabled | MediaOverride::Unchanged => "",
    };
    let params = serde_json::json!({"features": features, "media": media});
    let unchanged = {
      let last = self
        .last_media_params
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      last.as_ref() == Some(&params)
    };
    if unchanged {
      return Ok(());
    }
    self.configure("Emulation.setEmulatedMedia", params.clone()).await?;
    *self
      .last_media_params
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(params);
    Ok(())
  }

  /// `Browser.grantPermissions` for `origin` (every origin when `None`),
  /// at the browser level and scoped to this page's context — matches
  /// `crBrowser.ts::doGrantPermissions`, including its retry with the
  /// older local-network names.
  async fn grant_permissions(&self, origin: Option<&str>, permissions: &[String]) -> Result<()> {
    let send = |legacy: bool| async move {
      let names = crate::permissions::chromium(permissions, legacy)?;
      self
        .transport
        .send(
          None,
          &ferridriver_cdp_proto::browser::GrantPermissions {
            permissions: &names,
            origin,
            browser_context_id: self.browser_context_id.as_deref(),
          },
        )
        .await
        .map(|_| ())
    };
    match send(false).await {
      Err(error) if !error.to_string().contains("Unknown permission") => send(true).await.map_err(|_| error),
      result => result,
    }
  }

  /// Reset permissions granted via the options-bag / context-level
  /// `grantPermissions` — called from
  /// [`crate::ContextRef::clear_permissions`] (`Playwright
  /// browserContext.clearPermissions`).
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Browser.resetPermissions`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn reset_permissions(&self) -> Result<()> {
    self
      .transport
      .send(
        None,
        &ferridriver_cdp_proto::browser::ResetPermissions {
          browser_context_id: self.browser_context_id.as_deref(),
        },
      )
      .await?;
    Ok(())
  }

  /// Direct `Network.setExtraHTTPHeaders` command. Backs
  /// [`crate::Page::set_extra_http_headers`] (Playwright's public
  /// `page.setExtraHTTPHeaders(headers)`). The `apply_context_options`
  /// path inlines the same command separately so both entry points
  /// land independently.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Network.setExtraHTTPHeaders`, the target has closed, or the command
  /// outlives its time budget.
  pub async fn set_extra_http_headers(&self, headers: &FxHashMap<String, String>) -> Result<()> {
    let pairs: serde_json::Map<String, serde_json::Value> = headers
      .iter()
      .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
      .collect();
    self
      .cmd("Network.setExtraHTTPHeaders", serde_json::json!({"headers": pairs}))
      .await?;
    Ok(())
  }

  /// Set (or clear) the HTTP credentials used to answer
  /// `Fetch.authRequired` challenges. Backs
  /// [`crate::Page::set_http_credentials`] (Playwright's public
  /// `browserContext.setHTTPCredentials(creds | null)`). Passing `None`
  /// clears stored credentials so future 401 challenges surface as the
  /// browser's native auth dialog (`CancelAuth`).
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Fetch.enable` on this page or one of its renderers, the target has
  /// closed, or the command outlives its time budget.
  pub async fn set_http_credentials(&self, creds: Option<Vec<crate::options::HttpCredentials>>) -> Result<()> {
    *self.http_credentials.write().await = creds;
    // Re-arm Fetch so `handleAuthRequests` tracks the new state.
    self.ensure_fetch_enabled().await
  }

  /// The proxy this page's context sends through, whose credentials
  /// answer its challenges. The context was already created with it
  /// (`Target.createBrowserContext({ proxyServer })`); Playwright passes
  /// `proxyCredentials()` beside the site credentials to
  /// `crNetworkManager.authenticate`, and either arms `handleAuthRequests`.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Fetch.enable` when the proxy carries credentials, the target has
  /// closed, or the command outlives its time budget.
  pub async fn set_proxy(&self, proxy: Option<crate::options::ProxyConfig>) -> Result<()> {
    let creds = proxy.as_ref().and_then(crate::options::ProxyConfig::credentials);
    let arm = creds.is_some();
    *self.proxy_credentials.write().await = creds;
    if arm { self.ensure_fetch_enabled().await } else { Ok(()) }
  }

  // ---- Heap profiling ----

  /// Take a V8 heap snapshot, as `HeapProfiler.takeHeapSnapshot` writes
  /// it: the same JSON a `.heapsnapshot` file holds.
  ///
  /// Garbage is collected first, so what comes back is what the page is
  /// actually still holding rather than what it has merely not dropped
  /// yet. That is Puppeteer's `captureHeapSnapshot` and, through it,
  /// `chrome-devtools-mcp`'s `take_heapsnapshot`.
  ///
  /// Chrome does not answer with the snapshot; it streams it as
  /// `HeapProfiler.addHeapSnapshotChunk` events and then resolves the
  /// command. The tap is therefore installed BEFORE the command, and it
  /// is a wire-ordered tap rather than a broadcast subscription because
  /// one dropped chunk leaves JSON that either fails to parse or, worse,
  /// parses into a graph missing whatever was in it.
  ///
  /// # Why the domain is left enabled
  ///
  /// Puppeteer's `captureHeapSnapshot` disables `HeapProfiler` when it
  /// is done, and that throws away V8's object-id map. Ids are the only
  /// thing that identifies the same object in two snapshots, so a page
  /// captured twice through that path reports objects it never freed as
  /// collected and re-allocated: measured here, 78 of 200 surviving
  /// objects came back with new ids. `DevTools` keeps the domain
  /// enabled for as long as the Memory panel is open, and so does this.
  /// The cost is the id map staying alive, which is what a diff is.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects a `HeapProfiler` command, the target has closed, or the command
  /// outlives its time budget. Also fails when the snapshot arrives empty.
  pub async fn take_heap_snapshot(&self) -> Result<String> {
    let mut rx = self
      .transport
      .tap_event_methods(&["HeapProfiler.addHeapSnapshotChunk"], self.session_id.as_deref());

    self
      .cmd("HeapProfiler.enable", super::super::empty_params())
      .await
      .map_err(|e| map_cdp_unsupported(e, "heap snapshots need Chromium"))?;
    self
      .cmd("HeapProfiler.collectGarbage", super::super::empty_params())
      .await?;

    self
      .cmd(
        "HeapProfiler.takeHeapSnapshot",
        serde_json::json!({ "reportProgress": false }),
      )
      .await?;

    // Every chunk precedes the command's reply on one connection, so by
    // the time that reply has been awaited they are all in the channel
    // and draining what is queued is draining all of it.
    let mut snapshot = String::new();
    while let Ok(event) = rx.try_recv() {
      if let Some(chunk) = event.pointer("/params/chunk").and_then(|v| v.as_str()) {
        snapshot.push_str(chunk);
      }
    }
    if snapshot.is_empty() {
      return Err(crate::error::FerriError::backend(
        "HeapProfiler.takeHeapSnapshot sent no data",
      ));
    }
    Ok(snapshot)
  }

  // ---- Tracing ----

  fn parse_trace_events(bytes: &[u8]) -> Result<Vec<serde_json::Value>> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
      .map_err(|error| FerriError::protocol("Tracing", format!("invalid trace stream: {error}")))?;
    if let Some(events) = value.get("traceEvents").and_then(serde_json::Value::as_array) {
      return Ok(events.clone());
    }
    if let Some(events) = value.as_array() {
      return Ok(events.clone());
    }
    Err(FerriError::protocol("Tracing", "trace stream has no traceEvents array"))
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Tracing.start`, the target has closed, or the command outlives its
  /// time budget.
  pub async fn start_tracing(&self, categories: Option<&[String]>) -> Result<()> {
    let categories = match categories {
      Some(c) if !c.is_empty() => c.join(","),
      _ => crate::trace_categories::DEFAULT.join(","),
    };
    self
      .cmd(
        "Tracing.start",
        serde_json::json!({
          // Keep the trace lossless. Chrome writes the complete trace to
          // an IO stream and announces its handle after Tracing.end.
          "transferMode": "ReturnAsStream",
          "traceConfig": { "includedCategories": categories.split(',').collect::<Vec<_>>() },
        }),
      )
      .await
      .map_err(|e| map_cdp_unsupported(e, "CDP tracing needs Chromium"))?;
    Ok(())
  }

  /// End the trace and return every event Chrome recorded.
  ///
  /// The tap is installed BEFORE `Tracing.end` because Chrome announces the
  /// stream handle on the event channel while the command response is in
  /// flight. The stream is then consumed with IO.read, matching the
  /// `ReturnAsStream` path used by Playwright.
  ///
  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Tracing.end`, the target has closed, or the command outlives its
  /// time budget. Also fails when `Tracing.tracingComplete` does not arrive within 30 seconds or names no
  /// stream, or the stream cannot be read.
  pub async fn stop_tracing(&self) -> Result<Vec<serde_json::Value>> {
    let mut rx = self
      .transport
      .tap_event_methods(&["Tracing.tracingComplete"], self.session_id.as_deref());

    self.cmd("Tracing.end", super::super::empty_params()).await?;

    let event = tokio::time::timeout(Duration::from_secs(30), rx.recv())
      .await
      .map_err(|_| FerriError::timeout("Tracing.tracingComplete", 30_000))?
      .ok_or_else(|| FerriError::backend("CDP event stream closed before Tracing.tracingComplete"))?;
    let stream = event
      .pointer("/params/stream")
      .and_then(serde_json::Value::as_str)
      .ok_or_else(|| FerriError::protocol("Tracing.tracingComplete", "missing stream handle"))?;

    let trace = recording::read_stream(&self.transport, self.session_id.as_deref(), stream).await?;
    Self::parse_trace_events(&trace)
  }

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `Performance.enable` or `Performance.getMetrics`, the target has
  /// closed, or the command outlives its time budget.
  pub async fn metrics(&self) -> Result<Vec<MetricData>> {
    // Chrome only populates Performance.getMetrics after the domain is
    // enabled. The command is idempotent, so keeping this at the read
    // boundary also makes a first metrics call useful on a fresh page.
    self.cmd("Performance.enable", super::super::empty_params()).await?;
    let result = self.cmd("Performance.getMetrics", super::super::empty_params()).await?;
    let metrics = result
      .get("metrics")
      .and_then(|m| m.as_array())
      .cloned()
      .unwrap_or_default();
    Ok(
      metrics
        .iter()
        .map(|m| MetricData {
          name: m.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
          value: m.get("value").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
        })
        .collect(),
    )
  }

  // ---- Ref resolution ----

  /// # Errors
  ///
  /// Returns an error if Chrome rejects `DOM.resolveNode`, the target has closed, or the command outlives its
  /// time budget. Also fails when the node is gone and `ref_id` no longer resolves.
  pub async fn resolve_backend_node(&self, backend_node_id: i64, ref_id: &str) -> Result<AnyElement> {
    let resolve_result = self
      .cmd("DOM.resolveNode", serde_json::json!({"backendNodeId": backend_node_id}))
      .await?;

    let object_id = resolve_result
      .get("object")
      .and_then(|o| o.get("objectId"))
      .and_then(|v| v.as_str())
      .ok_or_else(|| FerriError::protocol("DOM.resolveNode", format!("Ref '{ref_id}' no longer valid.")))?;

    // We intentionally do NOT call `DOM.requestNode` here. Without an
    // explicit `DOM.getDocument` first (skipped by the lazy bootstrap),
    // Chrome reports `nodeId: 0` for nodes not in the DOM-agent's
    // current document. Element paths that genuinely need a nodeId
    // (e.g. `DOM.setFileInputFiles`) lazily request one via
    // `CdpElement::node_id()`; everywhere else, the cached
    // `Runtime` objectId is sufficient and bypasses the DOM agent.
    Ok(AnyElement::Cdp(CdpElement {
      transport: Arc::clone(&self.transport),
      session_id: self.session_id.clone(),
      handles: Arc::new(tokio::sync::Mutex::new(CdpElementHandles {
        node_id: None,
        object_id: Some(Arc::from(object_id)),
      })),
    }))
  }
}

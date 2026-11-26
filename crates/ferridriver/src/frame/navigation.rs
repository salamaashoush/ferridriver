//! Frame navigation, load-state waits, URL/selector waits, and HTML content access.

use std::sync::Arc;

use crate::error::Result;
use crate::options::WaitOptions;

use super::Frame;

impl Frame {
  // ── Content (frame-scoped) ───────────────────────────────────────────

  /// Playwright: `frame.content(options?: { includeShadow?: boolean }):
  /// Promise<string>` — the doctype and the document element's markup.
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation fails.
  #[track_caller]
  pub fn content(&self) -> crate::action::Action<'_, crate::options::ContentOptions, String> {
    let frame = self;
    crate::action::Action::new(move |opts: crate::options::ContentOptions| {
      Box::pin(async move {
        let params = serde_json::json!({ "includeShadow": opts.include_shadow });
        frame
          .page
          .traced_as("Frame", "content", params, frame.content_impl(opts))
          .await
      })
    })
  }

  /// The injected engine's `documentContent`, which `page.content` shares.
  pub(crate) async fn content_impl(&self, opts: crate::options::ContentOptions) -> Result<String> {
    self.page.inner.ensure_engine_injected().await?;
    let include_shadow = opts.include_shadow.unwrap_or(false);
    let r = self
      .backend_eval_expr(&format!("window.__fd._injected.documentContent({include_shadow})"))
      .await?;
    Ok(
      r.and_then(|v| v.as_str().map(std::string::ToString::to_string))
        .unwrap_or_default(),
    )
  }

  /// Get the frame's title.
  ///
  /// # Errors
  ///
  /// Returns an error if JS evaluation fails.
  pub async fn title(&self) -> Result<String> {
    self
      .page
      .traced_as("Frame", "title", serde_json::json!({}), async {
        let r = self.backend_eval_expr("document.title").await?;
        Ok(
          r.and_then(|v| v.as_str().map(std::string::ToString::to_string))
            .unwrap_or_default(),
        )
      })
      .await
  }

  // ── Navigation (frame-scoped) ────────────────────────────────────────

  /// Playwright: `frame.goto(url, options?: { referer?, timeout?,
  /// waitUntil? }): Promise<Response | null>`
  /// (`packages/playwright-core/src/client/frame.ts`).
  ///
  /// A child frame is navigated by assigning its location, then waited on
  /// like any other navigation of that frame.
  ///
  /// # Errors
  ///
  /// Returns an error if the navigation fails or does not reach
  /// `waitUntil` in time, and [`crate::error::FerriError::Unsupported`]
  /// for `referer` on a child frame.
  #[track_caller]
  pub fn goto(
    &self,
    url: &str,
  ) -> crate::action::Action<'static, crate::options::GotoOptions, Option<crate::network::Response>> {
    let frame = self.clone();
    let url = url.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        frame
          .page
          .traced_as(
            "Frame",
            "goto",
            serde_json::json!({ "url": url }),
            frame.goto_untraced(&url, opts),
          )
          .await
      })
    })
  }

  async fn goto_untraced(
    &self,
    url: &str,
    opts: crate::options::GotoOptions,
  ) -> Result<Option<crate::network::Response>> {
    if self.is_main_frame() {
      return self.page.goto_impl(url, Some(opts)).await;
    }
    if opts.referer.is_some() {
      return Err(crate::error::FerriError::unsupported(
        "A child frame is navigated by assigning its location, which cannot carry a referer",
      ));
    }
    let resolved = self.page.resolve_with_base_url(url).await;
    let target = serde_json::to_string(&resolved)?;
    // Listening starts before the assignment, which can commit a
    // same-document navigation before it returns.
    let events = self.page.events().subscribe();
    self
      .backend_eval_expr(&format!("void (window.location.href = {target})"))
      .await?;
    self
      .navigated(
        events,
        crate::options::WaitForNavigationOptions {
          timeout: opts.timeout,
          url: None,
          wait_until: opts.wait_until,
        },
      )
      .await
  }

  /// Playwright: `frame.waitForNavigation(options?: { timeout?, url?,
  /// waitUntil? }): Promise<Response | null>`. The next navigation of
  /// this frame, to a URL `url` matches when given, once it has reached
  /// `waitUntil`. The response is the new document's, and `None` for a
  /// navigation within the document.
  ///
  /// # Errors
  ///
  /// Returns an error on timeout, or if the frame is detached or the page
  /// closes first.
  #[track_caller]
  pub fn wait_for_navigation(
    &self,
  ) -> crate::action::Action<'static, crate::options::WaitForNavigationOptions, Option<crate::network::Response>> {
    let frame = self.clone();
    // At the call, not when the action is awaited: the navigation this
    // waits for is usually started before then.
    let events = frame.page.events().subscribe();
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        frame
          .page
          .traced_as(
            "Frame",
            "waitForNavigation",
            serde_json::json!({}),
            frame.navigated(events, opts),
          )
          .await
      })
    })
  }

  /// Whether `info` describes this frame. The main frame is the one with
  /// no parent whatever its id: a backend may issue a new id when a
  /// navigation moves the page to another process.
  fn is_described_by(&self, info: &crate::backend::FrameInfo, main: bool) -> bool {
    info.frame_id == *self.id || (main && info.parent_frame_id.is_none())
  }

  async fn navigated(
    &self,
    mut events: crate::events::EventSubscription<crate::events::PageEvent>,
    opts: crate::options::WaitForNavigationOptions,
  ) -> Result<Option<crate::network::Response>> {
    use crate::events::PageEvent;
    let timeout_ms = opts.timeout.unwrap_or_else(|| self.page.default_navigation_timeout());
    let deadline = deadline_after(timeout_ms);
    let main = self.is_main_frame();
    let wanted = |url: &str| opts.url.as_ref().is_none_or(|matcher| matcher.matches(url));
    let mut document: Option<crate::network::Response> = None;
    let response = loop {
      let next = match deadline {
        Some(d) => tokio::time::timeout_at(d, events.recv()).await,
        None => Ok(events.recv().await),
      };
      let event = match next {
        Ok(Some(event)) => event,
        Ok(None) => return Err(crate::error::FerriError::target_closed(None)),
        Err(_) => return Err(crate::error::FerriError::timeout("waiting for navigation", timeout_ms)),
      };
      match event {
        PageEvent::Response(response)
          if response.request().is_navigation_request()
            && response
              .frame_id()
              .is_some_and(|id| id == &*self.id || (main && id == &*self.page.main_frame().id)) =>
        {
          document = Some(response);
        },
        PageEvent::FrameNavigated(info) if self.is_described_by(&info, main) => {
          if wanted(&info.url) {
            break match document.take() {
              Some(response) => Some(response),
              None => self.late_document_response(&mut events, main).await,
            };
          }
          document = None;
        },
        PageEvent::FrameNavigatedWithinDocument(info) if self.is_described_by(&info, main) && wanted(&info.url) => {
          break None;
        },
        PageEvent::FrameDetached { frame_id } if frame_id == *self.id => {
          return Err(crate::error::FerriError::backend("Navigating frame was detached!"));
        },
        PageEvent::Close => return Err(crate::error::FerriError::target_closed(None)),
        _ => {},
      }
    };
    let state = match opts.wait_until.unwrap_or_default() {
      crate::options::LoadState::Commit => return Ok(response),
      state => state.as_str(),
    };
    let remaining = remaining_timeout(deadline)
      .ok_or_else(|| crate::error::FerriError::timeout("waiting for navigation", timeout_ms))?;
    self
      .wait_for_load_state_impl(
        Some(state),
        crate::options::WaitForLoadStateOptions {
          timeout: Some(remaining),
        },
      )
      .await?;
    Ok(response)
  }

  /// The response of a document that has committed before its response
  /// event came: the two reach us on separate consumers. A document that
  /// had no response at all (`about:blank`, a `data:` URL) costs the grace.
  async fn late_document_response(
    &self,
    events: &mut crate::events::EventSubscription<crate::events::PageEvent>,
    main: bool,
  ) -> Option<crate::network::Response> {
    let deadline = tokio::time::Instant::now() + crate::network::NAV_REQUEST_GRACE;
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.recv()).await {
      if let crate::events::PageEvent::Response(response) = event
        && response.request().is_navigation_request()
        && response
          .frame_id()
          .is_some_and(|id| id == &*self.id || (main && id == &*self.page.main_frame().id))
      {
        return Some(response);
      }
    }
    None
  }

  /// Playwright: `frame.waitForURL(url: string | RegExp | URLPattern |
  /// ((url: URL) => boolean), options?: { timeout?, waitUntil? })`. Once
  /// this frame's URL matches, `waitUntil` (default `load`) is awaited in
  /// what is left of the same timeout; `commit` returns on the match alone.
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out.
  #[track_caller]
  pub fn wait_for_url(
    &self,
    url: impl Into<crate::url_matcher::UrlCondition>,
  ) -> crate::action::Action<'static, crate::options::WaitForUrlOptions, ()> {
    let frame = self.clone();
    let condition = url.into();
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        frame
          .page
          .traced_as(
            "Frame",
            "waitForURL",
            serde_json::json!({ "url": condition.describe() }),
            frame.wait_for_url_impl(&condition, opts),
          )
          .await
      })
    })
  }

  /// Implementation of [`Self::wait_for_url`]. The URL is tracked
  /// locally (no wire cost to read), so the loop parks on the page
  /// event stream and re-checks on each event, with a coarse fallback
  /// tick for anything the stream misses. A URL is tested once, so a
  /// predicate is called for each URL the frame reaches, not per tick.
  pub(crate) async fn wait_for_url_impl(
    &self,
    condition: &crate::url_matcher::UrlCondition,
    opts: crate::options::WaitForUrlOptions,
  ) -> Result<()> {
    let timeout_ms = opts.timeout.unwrap_or_else(|| self.page.default_navigation_timeout());
    let deadline = deadline_after(timeout_ms);
    let timed_out = || {
      crate::error::FerriError::timeout(
        format!("waiting for URL matching {:?}", condition.describe()),
        timeout_ms,
      )
    };
    let main = self.is_main_frame();
    let mut events = self.page.events().subscribe();
    let mut tested: Option<String> = None;
    loop {
      let url = self.live(main).url();
      if tested.as_deref() != Some(url.as_str()) {
        let matched = match deadline {
          Some(d) => tokio::time::timeout_at(d, condition.test(&url))
            .await
            .map_err(|_| timed_out())??,
          None => condition.test(&url).await?,
        };
        if matched {
          break;
        }
        tested = Some(url);
      }
      let tick = match deadline {
        None => std::time::Duration::from_millis(250),
        Some(d) => {
          let remaining = d.saturating_duration_since(tokio::time::Instant::now());
          if remaining.is_zero() {
            return Err(timed_out());
          }
          remaining.min(std::time::Duration::from_millis(250))
        },
      };
      tokio::select! {
        ev = events.recv() => {
          if ev.is_none() {
            // Emitter dropped (page closing) — keep ticking until the
            // deadline instead of spinning on a closed channel.
            tokio::time::sleep(tick).await;
          }
        },
        () = tokio::time::sleep(tick) => {},
      }
    }
    let state = match opts.wait_until.unwrap_or_default() {
      crate::options::LoadState::Commit => return Ok(()),
      state => state.as_str(),
    };
    let remaining = remaining_timeout(deadline).ok_or_else(timed_out)?;
    self
      .live(main)
      .wait_for_load_state_impl(
        Some(state),
        crate::options::WaitForLoadStateOptions {
          timeout: Some(remaining),
        },
      )
      .await
  }

  /// Playwright: `frame.waitForTimeout(timeout): Promise<void>`.
  pub async fn wait_for_timeout(&self, timeout_ms: u64) {
    self.page.wait_for_timeout(timeout_ms).await;
  }

  // ── Waiting ──────────────────────────────────────────────────────────

  /// Wait for a selector within this frame and return the matched handle.
  ///
  /// Playwright: `frame.waitForSelector(selector, options?)`
  /// (`/tmp/playwright/packages/playwright-core/src/client/frame.ts:217`).
  /// For the default `state: 'visible'` and for `state: 'attached'` the
  /// resolved [`crate::element_handle::ElementHandle`] is returned. For
  /// `state: 'hidden' | 'detached'` the element is gone (or invisible)
  /// so Playwright returns `null`; we mirror that with `Ok(None)`.
  ///
  /// # Errors
  ///
  /// Returns an error if the element does not reach the requested state
  /// within the timeout.
  #[track_caller]
  pub fn wait_for_selector(
    &self,
    selector: &str,
  ) -> crate::action::Action<'static, WaitOptions, Option<crate::element_handle::ElementHandle>> {
    let frame = self.clone();
    let selector = selector.to_string();
    crate::action::Action::new(move |opts| Box::pin(async move { frame.wait_for_selector_impl(&selector, opts).await }))
  }

  pub(crate) async fn wait_for_selector_impl(
    &self,
    selector: &str,
    opts: WaitOptions,
  ) -> Result<Option<crate::element_handle::ElementHandle>> {
    let state = opts.state;
    let locator = match opts.strict {
      Some(strict) => self.locator(selector).strict(strict),
      None => self.selector_locator(selector),
    };
    locator.wait_for_impl(opts).await?;
    // Playwright returns a handle only when the element is present; the
    // `hidden` / `detached` states resolve precisely because it is not.
    let returns_handle = !matches!(
      state,
      Some(crate::options::WaitState::Hidden | crate::options::WaitState::Detached)
    );
    if returns_handle {
      Ok(Some(locator.element_handle().await?))
    } else {
      Ok(None)
    }
  }

  // ── Handles ──────────────────────────────────────────────────────────

  /// Playwright: `frame.setContent(html, options?: { timeout?,
  /// waitUntil? }): Promise<void>`. The document is rewritten with
  /// `document.open()` and `document.write()`, so its scripts run, and the
  /// call returns once it has reached `waitUntil` (default `load`).
  ///
  /// # Errors
  ///
  /// Returns an error if the content cannot be written or the document
  /// does not reach `waitUntil` in time.
  #[track_caller]
  pub fn set_content(&self, html: &str) -> crate::action::Action<'static, crate::options::SetContentOptions, ()> {
    let frame = self.clone();
    let html = html.to_string();
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        frame
          .page
          .traced_as(
            "Frame",
            "setContent",
            serde_json::json!({ "html": html }),
            frame.set_content_impl(&html, opts),
          )
          .await
      })
    })
  }

  pub(crate) async fn set_content_impl(&self, html: &str, opts: crate::options::SetContentOptions) -> Result<()> {
    let timeout_ms = opts.timeout.unwrap_or_else(|| self.page.default_navigation_timeout());
    let deadline = deadline_after(timeout_ms);
    let timed_out = || crate::error::FerriError::timeout("setting frame content", timeout_ms);
    let wait_until = opts.wait_until.unwrap_or_default();
    crate::operation_budget::scoped(timeout_ms, || "setting frame content".into(), async move {
      let mut need_frame_tree = self.page.frames().len() > 1;
      let state = if self.is_main_frame() {
        match wait_until {
          crate::options::LoadState::Commit => {
            self.page.inner.set_content(html).await?;
            need_frame_tree |= Self::html_may_create_subframes(html);
            None
          },
          crate::options::LoadState::Load | crate::options::LoadState::DomContentLoaded => {
            // Single-RTT path: set document content AND check `document.readyState`
            // plus iframe presence in one pipelined batch (CDP) or single eval
            // (BiDi/WebKit), skipping both a second eval RTT and `Page.getFrameTree`
            // on subframe-free documents.
            let dcl = wait_until == crate::options::LoadState::DomContentLoaded;
            match self.page.inner.set_content_and_check(html, dcl).await? {
              Some(v) if v.as_i64() == Some(1) => None,
              Some(v) if v.as_i64() == Some(2) => {
                need_frame_tree = true;
                None
              },
              _ => {
                need_frame_tree = true;
                Some(wait_until.as_str())
              },
            }
          },
          state @ crate::options::LoadState::NetworkIdle => {
            self.page.inner.set_content(html).await?;
            need_frame_tree = true;
            Some(state.as_str())
          },
        }
      } else {
        need_frame_tree = true;
        // Chromium reports `complete` for a document rewritten this way while
        // its images are still loading, though its `load` event still waits
        // for them. A listener added before `document.open()` is removed by
        // it, and with nothing to load the event fires inside
        // `document.close()`, so the listener goes on between the two. The
        // event leaves a mark that is read back.
        let load = matches!(
          wait_until,
          crate::options::LoadState::Load | crate::options::LoadState::NetworkIdle
        );
        self
          .evaluate_untraced(
            "({ html, mark }) => { \
             delete window[mark]; \
             document.open(); document.write(html); \
             window.addEventListener('load', () => { window[mark] = true; }, { once: true }); \
             document.close(); \
           }",
            crate::protocol::argument_from_serde(&serde_json::json!({ "html": html, "mark": SET_CONTENT_MARK }))?,
            Some(true),
          )
          .await?;
        if load {
          loop {
            // Read in the world the mark was set in.
            let loaded = self
              .evaluate_untraced(
                "mark => { const loaded = window[mark] === true; if (loaded) delete window[mark]; return loaded; }",
                crate::protocol::argument_from_serde(SET_CONTENT_MARK)?,
                Some(true),
              )
              .await?;
            if matches!(loaded, crate::protocol::SerializedValue::Bool(true)) {
              break;
            }
            if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) {
              return Err(timed_out());
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
          }
        }
        (wait_until == crate::options::LoadState::NetworkIdle).then_some("networkidle")
      };
      if let Some(state) = state {
        let remaining = remaining_timeout(deadline).ok_or_else(timed_out)?;
        self
          .wait_for_load_state_impl(
            Some(state),
            crate::options::WaitForLoadStateOptions {
              timeout: Some(remaining),
            },
          )
          .await?;
      }
      // Only re-fetch the frame tree when subframes exist (or existed before
      // this setContent) — avoids a sequential `Page.getFrameTree` RTT on the
      // common subframe-free `setContent` path.
      if need_frame_tree && let Ok(infos) = self.page.inner.get_frame_tree().await {
        self.page.reseed_frames(infos);
      }
      Ok(())
    })
    .await
  }

  /// Quick case-insensitive scan for `<iframe`, `<frame`, or `<script` tags
  /// when `setContent` uses `waitUntil: "commit"` and skips the DOM check.
  fn html_may_create_subframes(html: &str) -> bool {
    let bytes = html.as_bytes();
    bytes.windows(6).any(|w| {
      w.eq_ignore_ascii_case(b"<frame") || w.eq_ignore_ascii_case(b"iframe") || w.eq_ignore_ascii_case(b"script")
    })
  }

  /// Playwright: `frame.waitForLoadState(state?: 'load' | 'domcontentloaded'
  /// | 'networkidle', options?: { timeout? }): Promise<void>`
  /// (`packages/playwright-core/src/client/frame.ts`). The state is this
  /// frame's document's own, so a child frame that is still loading holds
  /// the wait while its parent has long finished.
  ///
  /// # Errors
  ///
  /// Returns an error if the wait times out before the load state is reached.
  #[track_caller]
  pub fn wait_for_load_state(
    &self,
    state: Option<&str>,
  ) -> crate::action::Action<'static, crate::options::WaitForLoadStateOptions, ()> {
    let frame = self.clone();
    let state = state.map(str::to_string);
    crate::action::Action::new(move |opts| {
      Box::pin(async move {
        frame
          .page
          .traced_as(
            "Frame",
            "waitForLoadState",
            serde_json::json!({ "state": state.clone().unwrap_or_else(|| "load".to_string()) }),
            frame.wait_for_load_state_impl(state.as_deref(), opts),
          )
          .await
      })
    })
  }

  /// Implementation of [`Self::wait_for_load_state`]. One in-page
  /// promise per attempt instead of a protocol-eval poll loop: the page
  /// resolves the promise when the state is reached, so the wire
  /// carries a single round trip in the common case. An attempt dies
  /// with its document on cross-document navigation — the loop re-arms
  /// in the new document. Each attempt is also capped so a reply lost
  /// to a target swap (`WebKit`) re-arms instead of hanging forever.
  pub(crate) async fn wait_for_load_state_impl(
    &self,
    state: Option<&str>,
    opts: crate::options::WaitForLoadStateOptions,
  ) -> Result<()> {
    const JS_WAIT_LOAD: &str = "() => document.readyState === 'complete' \
       ? true \
       : new Promise(function(resolve) { window.addEventListener('load', function() { resolve(true); }, { once: true }); })";
    const JS_WAIT_DOMCONTENTLOADED: &str = "() => (document.readyState === 'interactive' || document.readyState === 'complete') \
       ? true \
       : new Promise(function(resolve) { document.addEventListener('DOMContentLoaded', function() { resolve(true); }, { once: true }); })";
    // Same signal the old Rust-side poll used (resource timing entries,
    // 500ms quiet window, 100ms sampling) — but sampled in-page so the
    // whole wait is one round trip.
    const JS_WAIT_NETWORKIDLE: &str = "() => new Promise(function(resolve) { \
       var idleSince = performance.now(); \
       function pending() { \
         if (window.__fd_inflight > 0) return true; \
         var rs = performance.getEntriesByType('resource'); \
         var now = performance.now(); \
         for (var i = 0; i < rs.length; i++) { \
           if (rs[i].responseEnd === 0 || now - rs[i].responseEnd < 100) return true; \
         } \
         return false; \
       } \
       (function check() { \
         var now = performance.now(); \
         if (pending()) idleSince = now; \
         if (now - idleSince >= 500) return resolve(true); \
         setTimeout(check, 100); \
       })(); \
     })";

    let state = state.unwrap_or("load");
    let timeout_ms = opts.timeout.unwrap_or_else(|| self.page.default_timeout());
    let main = self.is_main_frame();

    // Playwright's `networkidle` is a lifecycle event of the document,
    // fired once and kept: a wait after it returns at once.
    if state == "networkidle"
      && let Some(network_idle) = self.page.inner().network_idle().cloned()
    {
      let frame_id = Arc::clone(&self.live(main).id);
      return crate::operation_budget::scoped(timeout_ms, || "waiting for networkidle".into(), async move {
        let wait = network_idle.wait(&frame_id);
        if let Some(deadline) = deadline_after(timeout_ms) {
          tokio::time::timeout_at(deadline, wait)
            .await
            .map_err(|_| crate::error::FerriError::timeout("waiting for networkidle", timeout_ms))
        } else {
          wait.await;
          Ok(())
        }
      })
      .await;
    }

    // The state check precedes the deadline, matching the old poll loop
    // and Playwright's local lifecycle bookkeeping: an already-reached
    // state passes regardless of how small the timeout is. Without this,
    // the promise attempt's own round trip would eat a tiny budget and
    // report a false timeout on slower transports.
    if state != "networkidle"
      && let Ok(Some(v)) = self.live(main).backend_eval_expr("document.readyState").await
    {
      let s = v.as_str().unwrap_or("loading");
      let reached = match state {
        "domcontentloaded" => s == "interactive" || s == "complete",
        _ => s == "complete",
      };
      if reached {
        return Ok(());
      }
    }

    let deadline = deadline_after(timeout_ms);

    let (js, timeout_label) = match state {
      "domcontentloaded" => (JS_WAIT_DOMCONTENTLOADED, "waiting for domcontentloaded"),
      "networkidle" => (JS_WAIT_NETWORKIDLE, "waiting for networkidle"),
      _ => (JS_WAIT_LOAD, "waiting for load state"),
    };

    crate::operation_budget::scoped(timeout_ms, || timeout_label.into(), async move {
      loop {
        let attempt_cap = match deadline {
          None => std::time::Duration::from_secs(5),
          Some(d) => {
            let remaining = d.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
              return Err(crate::error::FerriError::timeout(timeout_label, timeout_ms));
            }
            remaining.min(std::time::Duration::from_secs(5))
          },
        };
        match tokio::time::timeout(attempt_cap, self.live(main).eval::<bool>(js)).await {
          Ok(Ok(true)) => return Ok(()),
          Ok(Ok(false) | Err(_)) => {
            // Transient evaluate failure (document destroyed mid-wait);
            // brief pause, then re-arm in whatever document is live now.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
          },
          Err(_) => {}, // attempt cap hit — re-arm
        }
      }
    })
    .await
  }

  // ── Action methods (Playwright parity — task 3.9) ──────────────────────
  //
  // Mirrors Playwright's frame action surface from
  // `/tmp/playwright/packages/playwright-core/src/client/frame.ts:296-447`.
  // Each method delegates to `self.selector_locator(selector).<action>()` —
  // Frame's locator already scopes by `frame_id`, so the action runs in
  // the iframe's execution context (CDP) or against the synthesized
  // iframe (WebKit). Option bags are intentionally minimal here; they
  // ride on top of the existing Locator surface and pick up extensions
  // (timeout/force/etc.) when those land on Locator itself.

  // -- Mouse / pointer ---------------------------------------------------
}

/// The window property a child frame's `load` sets after `setContent`.
const SET_CONTENT_MARK: &str = "__fdSetContentLoaded";

/// `timeout_ms` from now, or `None` for `0`, which is no timeout.
pub(super) fn deadline_after(timeout_ms: u64) -> Option<tokio::time::Instant> {
  (timeout_ms != 0).then(|| tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms))
}

/// What is left of `deadline` as a timeout to hand a nested wait (`0`:
/// none), or `None` once it has passed.
fn remaining_timeout(deadline: Option<tokio::time::Instant>) -> Option<u64> {
  match deadline {
    None => Some(0),
    Some(d) => match d.saturating_duration_since(tokio::time::Instant::now()).as_millis() {
      0 => None,
      left => Some(u64::try_from(left).unwrap_or(u64::MAX)),
    },
  }
}

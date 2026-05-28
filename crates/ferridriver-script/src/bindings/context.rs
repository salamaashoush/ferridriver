//! `BrowserContextJs`: JS wrapper around `ferridriver::context::ContextRef`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ferridriver::context::ContextRef;
use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value, class::Trace};
use rustc_hash::FxHashMap;

use crate::bindings::convert::{FerriResultExt, init_script_from_js, serde_from_js, serde_to_js};
use crate::bindings::page::{call_predicate_truthy, url_value_to_matcher, with_page_callbacks};

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "BrowserContext")]
pub struct BrowserContextJs {
  #[qjs(skip_trace)]
  inner: Arc<ContextRef>,
  /// Per-context route registration counter. Mirrors `PageJs::next_route_id`;
  /// each `context.route(matcher, fn)` gets a unique id used as the key in
  /// the shared `PageCallbacks` userdata registry.
  #[qjs(skip_trace)]
  next_route_id: Arc<AtomicU64>,
  /// Always-true `UrlMatcher`s registered for predicate routes, keyed by id,
  /// so `context.unroute(fn)` can drop exactly the matching registration by
  /// `Arc` identity. Mirrors `PageJs::route_matchers`.
  #[qjs(skip_trace)]
  route_matchers: Arc<std::sync::Mutex<FxHashMap<u64, ferridriver::url_matcher::UrlMatcher>>>,
}

impl BrowserContextJs {
  #[must_use]
  pub fn new(inner: Arc<ContextRef>) -> Self {
    // Context route ids share the per-session `PageCallbacks` userdata
    // registry with page routes (and with other contexts), so they're
    // drawn from a process-global counter offset above any per-page id
    // range to avoid key collisions.
    static CONTEXT_ROUTE_BASE: AtomicU64 = AtomicU64::new(1 << 48);
    Self {
      inner,
      next_route_id: Arc::new(AtomicU64::new(CONTEXT_ROUTE_BASE.fetch_add(1 << 20, Ordering::Relaxed))),
      route_matchers: Arc::new(std::sync::Mutex::new(FxHashMap::default())),
    }
  }
}

#[rquickjs::methods]
impl BrowserContextJs {
  // ── Cookies ───────────────────────────────────────────────────────────────

  /// All cookies visible in this context.
  ///
  /// Returns an array of `{ name, value, domain, path, secure, httpOnly,
  /// expires, sameSite }` objects matching Playwright's cookie shape.
  #[qjs(rename = "cookies")]
  pub async fn cookies<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let cookies = self.inner.cookies().await.into_js()?;
    serde_to_js(&ctx, &cookies)
  }

  /// Append cookies to this context.
  ///
  /// `cookies` is an array matching Playwright's `SetNetworkCookieParam[]`:
  /// only `name` + `value` are required, plus either `url` OR `domain`+`path`.
  /// `secure`, `httpOnly`, `sameSite`, `expires` all default when absent.
  #[qjs(rename = "addCookies")]
  pub async fn add_cookies<'js>(&self, ctx: Ctx<'js>, cookies: Value<'js>) -> rquickjs::Result<()> {
    let parsed: Vec<ferridriver::backend::SetCookieParams> = serde_from_js(&ctx, cookies)?;
    let cookies: Vec<ferridriver::backend::CookieData> = parsed.into_iter().map(Into::into).collect();
    self.inner.add_cookies(cookies).await.into_js()
  }

  /// Playwright: `context.clearCookies(options?)`. Without options
  /// clears every cookie; with `{ name?, domain?, path? }` only
  /// cookies matching ALL specified filters are cleared. Filter
  /// values are exact-match strings — Playwright's TS surface accepts
  /// `string | RegExp` here too; regex filters are tracked under
  /// "Section B" pending a Rust core extension.
  #[qjs(rename = "clearCookies")]
  pub async fn clear_cookies<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    match options.0 {
      None => self.inner.clear_cookies().await.into_js(),
      Some(v) if v.is_undefined() || v.is_null() => self.inner.clear_cookies().await.into_js(),
      Some(v) => {
        #[derive(serde::Deserialize, Default)]
        struct Filter {
          name: Option<String>,
          domain: Option<String>,
          path: Option<String>,
        }
        let parsed: Filter = crate::bindings::convert::serde_from_js(&ctx, v)?;
        let core = ferridriver::backend::ClearCookieOptions {
          name: parsed.name,
          domain: parsed.domain,
          path: parsed.path,
        };
        self.inner.clear_cookies_filtered(&core).await.into_js()
      },
    }
  }

  /// Delete a cookie by name (optionally scoped to a domain).
  #[qjs(rename = "deleteCookie")]
  pub async fn delete_cookie(&self, name: String, domain: Opt<String>) -> rquickjs::Result<()> {
    self.inner.delete_cookie(&name, domain.0.as_deref()).await.into_js()
  }

  /// Export the current storage state — cookies + per-origin localStorage.
  ///
  /// Playwright: `storageState(options?: { path?, indexedDB? })
  ///   : Promise<{ cookies, origins }>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:460`).
  /// `path` writes the JSON to disk; `indexedDB` is accepted for parity but
  /// IndexedDB is not yet collected.
  #[qjs(rename = "storageState")]
  pub async fn storage_state<'js>(&self, ctx: Ctx<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<Value<'js>> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct JsStorageStateOptions {
      path: Option<String>,
      indexed_db: Option<bool>,
    }
    let core_opts = match options.0 {
      Some(v) if !v.is_undefined() && !v.is_null() => {
        let parsed: JsStorageStateOptions = serde_from_js(&ctx, v)?;
        Some(ferridriver::options::StorageStateOptions {
          path: parsed.path.map(std::path::PathBuf::from),
          indexed_db: parsed.indexed_db,
        })
      },
      _ => None,
    };
    let state = self.inner.storage_state(core_opts).await.into_js()?;
    serde_to_js(&ctx, &state)
  }

  // ── Permissions ───────────────────────────────────────────────────────────

  /// Grant a set of permissions (e.g. `['geolocation', 'notifications']`),
  /// optionally scoped to `origin`.
  #[qjs(rename = "grantPermissions")]
  pub async fn grant_permissions(&self, permissions: Vec<String>, origin: Opt<String>) -> rquickjs::Result<()> {
    self
      .inner
      .grant_permissions(&permissions, origin.0.as_deref())
      .await
      .into_js()
  }

  /// Revoke all previously granted permissions.
  #[qjs(rename = "clearPermissions")]
  pub async fn clear_permissions(&self) -> rquickjs::Result<()> {
    self.inner.clear_permissions().await.into_js()
  }

  // ── Emulation ─────────────────────────────────────────────────────────────

  /// Override the geolocation reported to pages in this context.
  #[qjs(rename = "setGeolocation")]
  pub async fn set_geolocation(&self, latitude: f64, longitude: f64, accuracy: f64) -> rquickjs::Result<()> {
    self
      .inner
      .set_geolocation(latitude, longitude, accuracy)
      .await
      .into_js()
  }

  /// Toggle offline mode for this context.
  #[qjs(rename = "setOffline")]
  pub async fn set_offline(&self, offline: bool) -> rquickjs::Result<()> {
    self.inner.set_offline(offline).await.into_js()
  }

  /// Playwright: `browserContext.setHTTPCredentials(httpCredentials |
  /// null)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:355`.
  /// Accepts `{ username, password, origin?, send? }` or `null` /
  /// `undefined` (clears stored credentials).
  #[qjs(rename = "setHTTPCredentials")]
  pub async fn set_http_credentials<'js>(&self, ctx: Ctx<'js>, credentials: Opt<Value<'js>>) -> rquickjs::Result<()> {
    let creds = match credentials.0 {
      None => None,
      Some(v) if v.is_undefined() || v.is_null() => None,
      Some(v) => {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct JsCreds {
          username: String,
          password: String,
          origin: Option<String>,
          send: Option<String>,
        }
        let parsed: JsCreds = serde_from_js(&ctx, v)?;
        Some(ferridriver::options::HttpCredentials {
          username: parsed.username,
          password: parsed.password,
          origin: parsed.origin,
          send: parsed.send.and_then(|s| match s.as_str() {
            "always" => Some(ferridriver::options::HttpCredentialsSend::Always),
            "unauthorized" => Some(ferridriver::options::HttpCredentialsSend::Unauthorized),
            _ => None,
          }),
        })
      },
    };
    self.inner.set_http_credentials(creds).await.into_js()
  }

  /// Set HTTP headers sent with every request in this context.
  ///
  /// `headers` is a plain object (e.g. `{ 'X-Foo': 'bar' }`).
  #[qjs(rename = "setExtraHTTPHeaders")]
  pub async fn set_extra_http_headers<'js>(&self, ctx: Ctx<'js>, headers: Value<'js>) -> rquickjs::Result<()> {
    let map: FxHashMap<String, String> = serde_from_js(&ctx, headers)?;
    self.inner.set_extra_http_headers(&map).await.into_js()
  }

  // ── Routing ─────────────────────────────────────────────────────────────

  /// Playwright: `browserContext.route(url, handler)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:377`.
  /// Routes every page in this context (current and future). Mirrors the
  /// `PageJs::route` dispatch: predicate functions register an always-true
  /// core matcher and are evaluated in the JS runtime via the session's
  /// `AsyncContext`; the JS callback / predicate live in the shared
  /// `PageCallbacks` userdata registry keyed by route id.
  #[qjs(rename = "route")]
  pub async fn route<'js>(
    &self,
    ctx: Ctx<'js>,
    url: Value<'js>,
    handler: rquickjs::Function<'js>,
  ) -> rquickjs::Result<()> {
    let async_ctx = match ctx.userdata::<crate::engine::SessionAsyncCtx>() {
      Some(ud) => ud.0.clone(),
      None => {
        return Err(rquickjs::Error::new_from_js_message(
          "context.route",
          "Error",
          "context.route requires the script engine's AsyncContext".to_string(),
        ));
      },
    };
    let id = self.next_route_id.fetch_add(1, Ordering::Relaxed);
    let saved_handler = rquickjs::Persistent::save(&ctx, handler);
    with_page_callbacks(&ctx, |r| r.insert_route_handler(id, saved_handler))?;

    let has_predicate = url.as_function().is_some();
    let matcher = if let Some(pred) = url.as_function() {
      let saved_pred = rquickjs::Persistent::save(&ctx, pred.clone());
      with_page_callbacks(&ctx, |r| r.insert_route_pred(id, saved_pred))?;
      let m = ferridriver::url_matcher::UrlMatcher::predicate(|_| true);
      self
        .route_matchers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(id, m.clone());
      m
    } else {
      url_value_to_matcher(&ctx, url)?
    };

    let rust_handler: ferridriver::route::RouteHandler = std::sync::Arc::new(move |route| {
      let async_ctx = async_ctx.clone();
      tokio::spawn(async move {
        use rquickjs::class::Class;
        let _: rquickjs::Result<()> = rquickjs::async_with!(async_ctx => |ctx| {
          if has_predicate {
            let pred = with_page_callbacks(&ctx, |r| r.get_route_pred(id))?
              .ok_or_else(|| rquickjs::Error::new_from_js_message("context.route", "Error", "route predicate gone".to_string()))?
              .restore(&ctx)?;
            let url_ctor: rquickjs::function::Constructor<'_> = ctx.globals().get("URL")?;
            let url_obj: rquickjs::Value<'_> = url_ctor.construct((route.request().url.clone(),))?;
            if !call_predicate_truthy(&pred, url_obj, &ctx).await? {
              route.continue_route(ferridriver::route::ContinueOverrides::default());
              return Ok(());
            }
          }
          let f = with_page_callbacks(&ctx, |r| r.get_route_handler(id))?
            .ok_or_else(|| rquickjs::Error::new_from_js_message("context.route", "Error", "route handler gone".to_string()))?
            .restore(&ctx)?;
          let route_class = Class::instance(ctx.clone(), crate::bindings::network::RouteJs::new(route))?;
          let _: rquickjs::Value<'_> = f.call((route_class,))?;
          Ok(())
        })
        .await;
      });
    });

    self.inner.route(matcher, rust_handler).await.into_js()?;
    Ok(())
  }

  /// Playwright: `browserContext.unroute(url, handler?)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:411`.
  /// A predicate is matched by `===` against the function passed to `route`.
  #[qjs(rename = "unroute")]
  pub async fn unroute<'js>(&self, ctx: Ctx<'js>, url: Value<'js>) -> rquickjs::Result<()> {
    if let Some(pred) = url.as_function() {
      let saved = with_page_callbacks(&ctx, |r| r.route_preds_snapshot())?;
      let mut victims: Vec<u64> = Vec::new();
      for (id, sp) in saved {
        let stored = sp.restore(&ctx)?;
        if stored.as_value() == pred.as_value() {
          victims.push(id);
        }
      }
      for id in victims {
        let m = self
          .route_matchers
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .remove(&id);
        if let Some(m) = m {
          self.inner.unroute(&m).await.into_js()?;
        }
        with_page_callbacks(&ctx, |r| r.remove_route(id))?;
      }
      return Ok(());
    }
    let matcher = url_value_to_matcher(&ctx, url)?;
    self.inner.unroute(&matcher).await.into_js()
  }

  // ── Init scripts ──────────────────────────────────────────────────────────

  /// Register a JS snippet to run on every new page in this context before
  /// page scripts execute. Mirrors Playwright's
  /// `browserContext.addInitScript(script, arg)` — see
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:356`.
  /// Accepts `Function | string | { path?, content? }` + optional `arg`
  /// exactly like the NAPI binding.
  #[qjs(rename = "addInitScript")]
  pub async fn add_init_script<'js>(
    &self,
    ctx: Ctx<'js>,
    script: Value<'js>,
    arg: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let (init, arg_json) = init_script_from_js(&ctx, script, arg.0)?;
    let disposable = self.inner.add_init_script(init, arg_json).await.into_js()?;
    let instance =
      rquickjs::class::Class::instance(ctx.clone(), crate::bindings::disposable::DisposableJs::new(disposable))?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  // ── Timeouts ──────────────────────────────────────────────────────────────

  /// Playwright: `browserContext.setDefaultTimeout(timeout)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:286`.
  /// Core stores the value behind an `Arc<AtomicU64>` so the setter works
  /// through this shared `&self` handle.
  #[qjs(rename = "setDefaultTimeout")]
  pub fn set_default_timeout(&self, timeout: f64) {
    self
      .inner
      .set_default_timeout(crate::bindings::convert::ms_f64_to_u64(timeout));
  }

  /// Playwright: `browserContext.setDefaultNavigationTimeout(timeout)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:282`.
  #[qjs(rename = "setDefaultNavigationTimeout")]
  pub fn set_default_navigation_timeout(&self, timeout: f64) {
    self
      .inner
      .set_default_navigation_timeout(crate::bindings::convert::ms_f64_to_u64(timeout));
  }

  // ── Lifecycle ─────────────────────────────────────────────────────────────

  /// Name of the session this context belongs to.
  #[qjs(rename = "name")]
  pub fn name(&self) -> String {
    self.inner.name().to_string()
  }

  /// Playwright: `browserContext.browser(): Browser | null` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:290`.
  /// Returns the parent browser, or `null` if the context was not created
  /// from a `Browser`.
  #[qjs(rename = "browser")]
  pub fn browser<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    use rquickjs::class::Class;
    match self.inner.browser() {
      Some(b) => {
        let wrapper = crate::bindings::browser::BrowserJs::new(std::sync::Arc::new(b.clone()));
        let instance = Class::instance(ctx.clone(), wrapper)?;
        rquickjs::IntoJs::into_js(instance, &ctx)
      },
      None => Ok(Value::new_null(ctx)),
    }
  }

  /// Playwright: `browserContext.isClosed(): boolean` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:298`.
  #[qjs(rename = "isClosed")]
  pub fn is_closed(&self) -> bool {
    self.inner.is_closed()
  }

  /// Close the context (tears down the underlying browser state).
  #[qjs(rename = "close")]
  pub async fn close(&self) -> rquickjs::Result<()> {
    self.inner.close().await.into_js()
  }

  // ── Page creation ──────────────────────────────────────────────────────

  /// Playwright: `browser.newContext().newPage(): Promise<Page>` —
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts` (on
  /// `BrowserContext`). Opens a new tab in this context; the returned
  /// [`crate::bindings::page::PageJs`] inherits the context's
  /// `recordVideo` configuration (if any) and every other per-context
  /// setting wired through [`ContextRef`].
  #[qjs(rename = "newPage")]
  pub async fn new_page<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    use rquickjs::class::Class;
    let page = self.inner.new_page().await.into_js()?;
    let wrapper = crate::bindings::page::pagejs_for_ctx(&ctx, page);
    let instance = Class::instance(ctx.clone(), wrapper)?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  // ── Video recording ────────────────────────────────────────────────────

  /// Playwright:
  /// `browser.newContext({ recordVideo: { dir, size? } })` —
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:10150`.
  /// Transitional API: §4.1's `BrowserContextOptions` bag will fold
  /// this into the full options struct.
  #[qjs(rename = "setRecordVideo")]
  pub async fn set_record_video<'js>(&self, ctx: Ctx<'js>, options: Value<'js>) -> rquickjs::Result<()> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct JsRecordVideoOptions {
      dir: String,
      size: Option<JsVideoSize>,
    }
    #[derive(serde::Deserialize)]
    struct JsVideoSize {
      width: f64,
      height: f64,
    }
    let parsed: JsRecordVideoOptions = serde_from_js(&ctx, options)?;
    let opts = ferridriver::options::RecordVideoOptions {
      dir: std::path::PathBuf::from(parsed.dir),
      size: parsed.size.map(|s| ferridriver::options::VideoSize {
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        width: s.width.max(0.0) as u32,
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        height: s.height.max(0.0) as u32,
      }),
    };
    self.inner.set_record_video(opts).await.into_js()
  }

  // ── Exposed bindings / functions ───────────────────────────────────────

  /// Playwright: `browserContext.exposeBinding(name, callback)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:364`.
  ///
  /// Binds `window[name]` on every page in this context (current +
  /// future). The page-side call routes back into `callback`, invoked
  /// as `callback(source, ...args)` where `source` is
  /// `{ context, page, frame }` (identity strings) and the page-side
  /// call args are spread (Playwright parity). The callback's return
  /// value (awaiting any returned promise) is delivered to the
  /// page-side caller. Returns a `{ dispose() }` Disposable.
  #[qjs(rename = "exposeBinding")]
  pub async fn expose_binding<'js>(
    &self,
    ctx: Ctx<'js>,
    name: String,
    callback: rquickjs::Function<'js>,
  ) -> rquickjs::Result<Value<'js>> {
    let binding = self.make_binding(&ctx, &name, callback, true)?;
    self.inner.expose_binding(&name, binding).await.into_js()?;
    self.make_disposable(&ctx, name)
  }

  /// Playwright: `browserContext.exposeFunction(name, callback)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:370`.
  ///
  /// `exposeFunction` is `exposeBinding` minus the `source` argument:
  /// the callback receives only the spread page-side call args.
  #[qjs(rename = "exposeFunction")]
  pub async fn expose_function<'js>(
    &self,
    ctx: Ctx<'js>,
    name: String,
    callback: rquickjs::Function<'js>,
  ) -> rquickjs::Result<Value<'js>> {
    let binding = self.make_binding(&ctx, &name, callback, false)?;
    self.inner.expose_binding(&name, binding).await.into_js()?;
    self.make_disposable(&ctx, name)
  }

  // ── Context-level events ───────────────────────────────────────────────

  /// Wait for the next context-scoped event. Currently supports
  /// `'weberror'` — returns a live [`crate::bindings::web_error::WebErrorJs`].
  /// Playwright: `browserContext.waitForEvent(event, options?)`.
  #[qjs(rename = "waitForEvent")]
  pub async fn wait_for_event<'js>(
    &self,
    ctx: Ctx<'js>,
    event: String,
    timeout_ms: Opt<f64>,
  ) -> rquickjs::Result<Value<'js>> {
    use rquickjs::class::Class;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let timeout = timeout_ms.0.unwrap_or(30_000.0) as u64;
    let ev = self
      .inner
      .wait_for_event(&event, timeout)
      .await
      .map_err(|e| rquickjs::Error::new_from_js_message("BrowserContext.waitForEvent", "Error", e.to_string()))?;
    match ev {
      ferridriver::events::ContextEvent::WebError(err) => {
        let wrapper = crate::bindings::web_error::WebErrorJs::new(err);
        let instance = Class::instance(ctx.clone(), wrapper)?;
        rquickjs::IntoJs::into_js(instance, &ctx)
      },
    }
  }
}

impl BrowserContextJs {
  /// Stash `callback` in the shared exposed-callback registry and build
  /// an [`ferridriver::ExposedBinding`] that dispatches back into the
  /// script context via the session `AsyncContext`. When `with_source`
  /// is true the `{ context, page, frame }` source object is prepended
  /// to the spread args (`exposeBinding`); otherwise only the args are
  /// spread (`exposeFunction`).
  fn make_binding<'js>(
    &self,
    ctx: &Ctx<'js>,
    name: &str,
    callback: rquickjs::Function<'js>,
    with_source: bool,
  ) -> rquickjs::Result<ferridriver::ExposedBinding> {
    let async_ctx = match ctx.userdata::<crate::engine::SessionAsyncCtx>() {
      Some(ud) => ud.0.clone(),
      None => {
        return Err(rquickjs::Error::new_from_js_message(
          "BrowserContext.exposeBinding",
          "Error",
          "exposeBinding requires the script engine's AsyncContext".to_string(),
        ));
      },
    };
    let saved = rquickjs::Persistent::save(ctx, callback);
    crate::bindings::page::insert_exposed_callback(ctx, name.to_string(), saved)?;

    let name = name.to_string();
    let binding: ferridriver::ExposedBinding = Arc::new(move |source, args| {
      let async_ctx = async_ctx.clone();
      let name = name.clone();
      Box::pin(async move {
        let out: rquickjs::Result<serde_json::Value> = rquickjs::async_with!(async_ctx => |ctx| {
          let f = crate::bindings::page::get_exposed_callback(&ctx, &name)?
            .ok_or_else(|| {
              rquickjs::Error::new_from_js_message(
                "BrowserContext.exposeBinding",
                "Error",
                "exposed callback gone".to_string(),
              )
            })?
            .restore(&ctx)?;
          // Playwright spreads the page-side call args into the
          // callback. For exposeBinding the BindingSource object is the
          // first argument; for exposeFunction it is omitted.
          let mut call_args = rquickjs::function::Args::new_unsized(ctx.clone());
          if with_source {
            let src = rquickjs::Object::new(ctx.clone())?;
            src.set("context", source.context.clone())?;
            src.set("page", source.page.clone())?;
            src.set("frame", source.frame.clone())?;
            call_args.push_arg(src)?;
          }
          for v in &args {
            // `json_to_js` (NOT serde): a transitive dep force-enables
            // `serde_json/arbitrary_precision`, under which the serde
            // path turns numbers into wrapper objects.
            call_args.push_arg(crate::bindings::convert::json_to_js(&ctx, v)?)?;
          }
          let mp: rquickjs::promise::MaybePromise<'_> = call_args.apply(&f)?;
          let res = mp.into_future::<rquickjs::Value<'_>>().await?;
          let json = match ctx.json_stringify(res)? {
            Some(s) => serde_json::from_str(&s.to_string()?).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
          };
          Ok(json)
        })
        .await;
        out.unwrap_or(serde_json::Value::Null)
      })
    });
    Ok(binding)
  }

  /// Build the `{ dispose() }` Disposable returned from
  /// `exposeBinding` / `exposeFunction`. `dispose()` removes the
  /// binding from the registry and from every page in the context
  /// (`window[name]` is deleted on each page).
  fn make_disposable<'js>(&self, ctx: &Ctx<'js>, name: String) -> rquickjs::Result<Value<'js>> {
    let obj = rquickjs::Object::new(ctx.clone())?;
    let inner = self.inner.clone();
    let dispose = rquickjs::Function::new(
      ctx.clone(),
      rquickjs::prelude::Async(move || {
        let inner = inner.clone();
        let name = name.clone();
        // Core removal drops the binding from the registry AND removes
        // `window[name]` from every page in the context. The stashed
        // QuickJS callback stays in the name-keyed registry but is never
        // invoked again (the page-side proxy is gone); re-entering the
        // engine `AsyncContext` from this JS-invoked async fn would
        // deadlock against the script's own outer `async_with`.
        async move {
          let _ = inner.remove_exposed_binding(&name).await;
        }
      }),
    )?;
    obj.set("dispose", dispose)?;
    rquickjs::IntoJs::into_js(obj, ctx)
  }
}

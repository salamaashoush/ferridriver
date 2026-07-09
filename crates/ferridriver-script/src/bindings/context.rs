//! `BrowserContextJs`: JS wrapper around `ferridriver::context::ContextRef`.

use std::sync::Arc;

use ferridriver::context::ContextRef;
use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value, class::Trace};
use rustc_hash::FxHashMap;

use crate::bindings::convert::FerriResultCtxExt;
use crate::bindings::convert::{init_script_from_js, serde_from_js, serde_to_js};
use crate::bindings::page::{
  PageCallbacks, RouteOwner, call_predicate_truthy, url_value_to_matcher, with_page_callbacks,
};

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "BrowserContext")]
pub struct BrowserContextJs {
  #[qjs(skip_trace)]
  inner: Arc<ContextRef>,
}

impl BrowserContextJs {
  #[must_use]
  pub fn new(inner: Arc<ContextRef>) -> Self {
    Self { inner }
  }

  /// This context's route-registry owner key. Keyed by core context
  /// name, not wrapper identity — `page.context()` mints a fresh
  /// `Arc<ContextRef>` per call, so `unroute(fn)` must work across
  /// wrappers of the same context.
  fn route_owner(&self) -> RouteOwner {
    RouteOwner::Context(self.inner.name().to_string())
  }
}

#[rquickjs::methods]
impl BrowserContextJs {
  /// `context.tracing` — Playwright's `Tracing` controller. Exposed as a
  /// JS property.
  #[qjs(get, rename = "tracing")]
  pub fn tracing(&self) -> crate::bindings::tracing::TracingJs {
    crate::bindings::tracing::TracingJs::new(self.inner.clone())
  }

  // ── Cookies ───────────────────────────────────────────────────────────────

  /// All cookies visible in this context.
  ///
  /// Returns an array of `{ name, value, domain, path, secure, httpOnly,
  /// expires, sameSite }` objects matching Playwright's cookie shape.
  #[qjs(rename = "cookies")]
  pub async fn cookies<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let cookies = self.inner.cookies().await.into_js_with(&ctx)?;
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
    self.inner.add_cookies(cookies).await.into_js_with(&ctx)
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
      None => self.inner.clear_cookies().await.into_js_with(&ctx),
      Some(v) if v.is_undefined() || v.is_null() => self.inner.clear_cookies().await.into_js_with(&ctx),
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
        self.inner.clear_cookies_filtered(&core).await.into_js_with(&ctx)
      },
    }
  }

  /// Delete a cookie by name (optionally scoped to a domain).
  #[qjs(rename = "deleteCookie")]
  pub async fn delete_cookie(&self, ctx: rquickjs::Ctx<'_>, name: String, domain: Opt<String>) -> rquickjs::Result<()> {
    self
      .inner
      .delete_cookie(&name, domain.0.as_deref())
      .await
      .into_js_with(&ctx)
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
    let state = self
      .inner
      .storage_state()
      .maybe_options(core_opts)
      .await
      .into_js_with(&ctx)?;
    serde_to_js(&ctx, &state)
  }

  /// Playwright: `setStorageState(storageState: string | SetStorageState):
  /// Promise<void>` (1.59). Clears existing cookies + localStorage then
  /// applies `storageState`. A string is a path to a state file; an object is
  /// the inline `{ cookies, origins }` shape.
  #[qjs(rename = "setStorageState")]
  pub async fn set_storage_state<'js>(&self, ctx: Ctx<'js>, storage_state: Value<'js>) -> rquickjs::Result<()> {
    let input: serde_json::Value = serde_from_js(&ctx, storage_state)?;
    let state = match input {
      serde_json::Value::String(path) => {
        // Async read — this job runs on the single VM event loop, so a
        // blocking `std::fs` read would stall every pump and any
        // concurrent script on the session.
        let text = tokio::fs::read_to_string(&path).await.map_err(|e| {
          crate::bindings::convert::throw_named(&ctx, "Error", format!("setStorageState: read {path}: {e}"))
        })?;
        serde_json::from_str(&text).map_err(|e| {
          crate::bindings::convert::throw_named(&ctx, "Error", format!("setStorageState: parse {path}: {e}"))
        })?
      },
      other => other,
    };
    self
      .inner
      .set_storage_state(&state)
      .await
      .map_err(|e| crate::bindings::convert::ferri_throw(&ctx, &e))?;
    Ok(())
  }

  // ── Permissions ───────────────────────────────────────────────────────────

  /// Grant a set of permissions (e.g. `['geolocation', 'notifications']`),
  /// optionally scoped to `origin`.
  #[qjs(rename = "grantPermissions")]
  pub async fn grant_permissions(
    &self,
    ctx: rquickjs::Ctx<'_>,
    permissions: Vec<String>,
    origin: Opt<String>,
  ) -> rquickjs::Result<()> {
    self
      .inner
      .grant_permissions(&permissions, origin.0.as_deref())
      .await
      .into_js_with(&ctx)
  }

  /// Revoke all previously granted permissions.
  #[qjs(rename = "clearPermissions")]
  pub async fn clear_permissions(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.clear_permissions().await.into_js_with(&ctx)
  }

  // ── Emulation ─────────────────────────────────────────────────────────────

  /// Override the geolocation reported to pages in this context.
  #[qjs(rename = "setGeolocation")]
  pub async fn set_geolocation(
    &self,
    ctx: rquickjs::Ctx<'_>,
    latitude: f64,
    longitude: f64,
    accuracy: f64,
  ) -> rquickjs::Result<()> {
    self
      .inner
      .set_geolocation(latitude, longitude, accuracy)
      .await
      .into_js_with(&ctx)
  }

  /// Toggle offline mode for this context.
  #[qjs(rename = "setOffline")]
  pub async fn set_offline(&self, ctx: rquickjs::Ctx<'_>, offline: bool) -> rquickjs::Result<()> {
    self.inner.set_offline(offline).await.into_js_with(&ctx)
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
    self.inner.set_http_credentials(creds).await.into_js_with(&ctx)
  }

  /// Set HTTP headers sent with every request in this context.
  ///
  /// `headers` is a plain object (e.g. `{ 'X-Foo': 'bar' }`).
  #[qjs(rename = "setExtraHTTPHeaders")]
  pub async fn set_extra_http_headers<'js>(&self, ctx: Ctx<'js>, headers: Value<'js>) -> rquickjs::Result<()> {
    let map: FxHashMap<String, String> = serde_from_js(&ctx, headers)?;
    self.inner.set_extra_http_headers(&map).await.into_js_with(&ctx)
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
  pub fn route<'js>(
    &self,
    ctx: Ctx<'js>,
    url: Value<'js>,
    handler: rquickjs::Function<'js>,
    options: rquickjs::function::Opt<Value<'js>>,
  ) -> rquickjs::Result<rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<()>> + 'js>> {
    let times = crate::bindings::page::parse_route_times(&options)?;
    let vm = match ctx.userdata::<crate::engine::SessionVm>() {
      Some(ud) => ud.0.clone(),
      None => {
        return Err(rquickjs::Error::new_from_js_message(
          "context.route",
          "Error",
          "context.route requires the script engine's VM handle".to_string(),
        ));
      },
    };
    let id = with_page_callbacks(&ctx, PageCallbacks::next_route_id)?;
    // Sync prologue: snapshot the registrar's grant (see
    // `SavedCallback::save` — an async-fn body first-polls off-bracket).
    let net = crate::bindings::fetch::active_net(&ctx);
    let saved_handler = crate::bindings::page::SavedCallback::save_with_net(&ctx, handler, net.clone());

    let has_predicate = url.as_function().is_some();
    let (matcher, saved_pred, registry_matcher) = if let Some(pred) = url.as_function() {
      let saved_pred = crate::bindings::page::SavedCallback::save_with_net(&ctx, pred.clone(), net);
      let m = ferridriver::url_matcher::UrlMatcher::predicate(|_| true);
      (m.clone(), Some(saved_pred), Some(m))
    } else {
      (url_value_to_matcher(&ctx, url)?, None, None)
    };
    with_page_callbacks(&ctx, |r| {
      r.insert_route(id, self.route_owner(), saved_handler, saved_pred, registry_matcher);
    })?;

    let rust_handler: ferridriver::route::RouteHandler = std::sync::Arc::new(move |route| {
      let vm = vm.clone();
      tokio::spawn(async move {
        use rquickjs::class::Class;
        let _: Result<rquickjs::Result<()>, crate::error::ScriptError> = crate::vm_with!(vm => |ctx| {
          if has_predicate {
            let saved_pred = with_page_callbacks(&ctx, |r| r.get_route_pred(id))?
              .ok_or_else(|| rquickjs::Error::new_from_js_message("context.route", "Error", "route predicate gone".to_string()))?;
            let pred = saved_pred.restore(&ctx)?;
            let url_ctor: rquickjs::function::Constructor<'_> = ctx.globals().get("URL")?;
            let url_obj: rquickjs::Value<'_> = url_ctor.construct((route.request().url.clone(),))?;
            let truthy = crate::bindings::fetch::bracket_net(
              crate::bindings::fetch::policy_cell(&ctx),
              saved_pred.net().cloned(),
              call_predicate_truthy(&pred, url_obj, &ctx),
            )
            .await?;
            if !truthy {
              route.fallback(ferridriver::route::ContinueOverrides::default());
              return Ok(());
            }
          }
          let f = with_page_callbacks(&ctx, |r| r.get_route_handler(id))?
            .ok_or_else(|| rquickjs::Error::new_from_js_message("context.route", "Error", "route handler gone".to_string()))?;
          let route_class = Class::instance(ctx.clone(), crate::bindings::network::RouteJs::new(route))?;
          // `call_bracketed_async`: an async route handler's `fetch`
          // runs in a continuation off the synchronous call (see
          // `page.route`).
          let _: rquickjs::Value<'_> = f.call_bracketed_async(&ctx, (route_class,)).await?;
          Ok(())
        })
        .await;
      });
    });

    let inner = self.inner.clone();
    Ok(rquickjs::promise::Promised::from(async move {
      inner.route(matcher, rust_handler, times).await.into_js_with(&ctx)?;
      Ok(())
    }))
  }

  /// Playwright: `browserContext.routeWebSocket(url, handler)`. Intercepts
  /// WebSocket connections matching `url` (glob string or `RegExp`) on every
  /// page in this context; the handler receives a live `WebSocketRoute`.
  /// One-shot create dispatch is shared with `page.routeWebSocket` via
  /// `build_ws_route_handler`; `onMessage`/`onClose` use the WS pump.
  #[qjs(rename = "routeWebSocket")]
  pub fn route_web_socket<'js>(
    &self,
    ctx: Ctx<'js>,
    url: Value<'js>,
    handler: rquickjs::Function<'js>,
  ) -> rquickjs::Result<rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<()>> + 'js>> {
    let vm = match ctx.userdata::<crate::engine::SessionVm>() {
      Some(ud) => ud.0.clone(),
      None => {
        return Err(rquickjs::Error::new_from_js_message(
          "context.routeWebSocket",
          "Error",
          "context.routeWebSocket requires the script engine's VM handle".to_string(),
        ));
      },
    };
    let matcher = url_value_to_matcher(&ctx, url)?;
    let handler_id = with_page_callbacks(&ctx, PageCallbacks::next_route_id)?;
    let owner = RouteOwner::Context(self.inner.name().to_string());
    // Sync prologue: snapshot the registrar's grant (see `SavedCallback::save`).
    let net = crate::bindings::fetch::active_net(&ctx);
    let saved = crate::bindings::page::SavedCallback::save_with_net(&ctx, handler, net);
    with_page_callbacks(&ctx, |r| r.insert_ws_callback(handler_id, owner.clone(), saved))?;
    let rust_handler = crate::bindings::web_socket_route::build_ws_route_handler(vm, handler_id, owner);
    let inner = self.inner.clone();
    Ok(rquickjs::promise::Promised::from(async move {
      inner.route_web_socket(matcher, rust_handler).await.into_js_with(&ctx)
    }))
  }

  /// Playwright: `browserContext.routeFromHAR(har, options?)`. Replay-only.
  #[qjs(rename = "routeFromHAR")]
  pub async fn route_from_har(
    &self,
    ctx: rquickjs::Ctx<'_>,
    har: String,
    options: rquickjs::function::Opt<Value<'_>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::page::parse_har_options(&options)?;
    self
      .inner
      .route_from_har(std::path::Path::new(&har))
      .options(opts)
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `browserContext.unroute(url, handler?)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:411`.
  /// A predicate is matched by `===` against the function passed to `route`.
  #[qjs(rename = "unroute")]
  pub async fn unroute<'js>(&self, ctx: Ctx<'js>, url: Value<'js>) -> rquickjs::Result<()> {
    if let Some(pred) = url.as_function() {
      let saved = with_page_callbacks(&ctx, |r| r.predicate_routes_for_owner(&self.route_owner()))?;
      let mut victims: Vec<u64> = Vec::new();
      for (id, sp) in saved {
        let stored = sp.restore(&ctx)?;
        if stored.as_value() == pred.as_value() {
          victims.push(id);
        }
      }
      for id in victims {
        let m = with_page_callbacks(&ctx, |r| r.remove_route(id))?;
        if let Some(m) = m {
          self.inner.unroute(&m).await.into_js_with(&ctx)?;
        }
      }
      return Ok(());
    }
    let matcher = url_value_to_matcher(&ctx, url)?;
    self.inner.unroute(&matcher).await.into_js_with(&ctx)
  }

  /// Playwright:
  /// `browserContext.unrouteAll(options?: { behavior?: 'wait' | 'ignoreErrors' | 'default' })`.
  /// Removes every route registered via `context.route` (page-scoped
  /// routes stay active), clearing the script-side predicate/handler
  /// tables for this context too.
  #[qjs(rename = "unrouteAll")]
  pub async fn unroute_all<'js>(&self, ctx: Ctx<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<()> {
    let behavior = match options.0.and_then(rquickjs::Value::into_object) {
      Some(obj) => match obj.get::<_, Option<String>>("behavior")? {
        Some(b) => Some(crate::bindings::page::options::parse_unroute_behavior(&b)?),
        None => None,
      },
      None => None,
    };
    self.inner.unroute_all(behavior).await.into_js_with(&ctx)?;
    with_page_callbacks(&ctx, |r| r.remove_routes_for_owner(&self.route_owner()))?;
    Ok(())
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
    let disposable = self.inner.add_init_script(init, arg_json).await.into_js_with(&ctx)?;
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
  pub async fn close(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self.inner.close().await.into_js_with(&ctx)?;
    // Release this context's persisted route / WS handlers — the
    // session VM outlives the context, so without this each closed
    // context leaks its `Persistent`s for the VM's remaining life.
    let owner = RouteOwner::Context(self.inner.name().to_string());
    with_page_callbacks(&ctx, |r| {
      r.remove_routes_for_owner(&owner);
      r.remove_ws_callbacks_for_owner(&owner);
    })?;
    Ok(())
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
    let page = self.inner.new_page().await.into_js_with(&ctx)?;
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
    self.inner.set_record_video(opts).await.into_js_with(&ctx)
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
  pub fn expose_binding<'js>(
    &self,
    ctx: Ctx<'js>,
    name: String,
    callback: rquickjs::Function<'js>,
  ) -> rquickjs::Result<
    rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<Value<'js>>> + 'js>,
  > {
    // Both the callback save (inside `make_binding`) and the disposable
    // build happen synchronously here, on the registrar's stack, so the
    // callback captures the tool's grant (see `SavedCallback::save`).
    let binding = self.make_binding(&ctx, &name, callback, true)?;
    let disposable = self.make_disposable(&ctx, name.clone())?;
    let inner = self.inner.clone();
    Ok(rquickjs::promise::Promised::from(async move {
      inner.expose_binding(&name, binding).await.into_js_with(&ctx)?;
      Ok(disposable)
    }))
  }

  /// Playwright: `browserContext.exposeFunction(name, callback)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:370`.
  ///
  /// `exposeFunction` is `exposeBinding` minus the `source` argument:
  /// the callback receives only the spread page-side call args.
  #[qjs(rename = "exposeFunction")]
  pub fn expose_function<'js>(
    &self,
    ctx: Ctx<'js>,
    name: String,
    callback: rquickjs::Function<'js>,
  ) -> rquickjs::Result<
    rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<Value<'js>>> + 'js>,
  > {
    let binding = self.make_binding(&ctx, &name, callback, false)?;
    let disposable = self.make_disposable(&ctx, name.clone())?;
    let inner = self.inner.clone();
    Ok(rquickjs::promise::Promised::from(async move {
      inner.expose_binding(&name, binding).await.into_js_with(&ctx)?;
      Ok(disposable)
    }))
  }

  // ── Context-level events ───────────────────────────────────────────────

  /// Wait for the next context-scoped event. Supports `'weberror'` plus
  /// the page-lifecycle mirror events (`'download'`, `'frameattached'`,
  /// `'framedetached'`, `'framenavigated'`, `'pageclose'`, `'pageload'`),
  /// resolving with the matching live class instance. Playwright:
  /// `browserContext.waitForEvent(event, options?)`.
  #[qjs(rename = "waitForEvent")]
  pub async fn wait_for_event<'js>(
    &self,
    ctx: Ctx<'js>,
    event: String,
    timeout_ms: Opt<f64>,
  ) -> rquickjs::Result<Value<'js>> {
    use ferridriver::events::ContextEvent;
    use rquickjs::IntoJs;
    use rquickjs::class::Class;
    let timeout = timeout_ms
      .0
      .map_or_else(|| self.inner.default_timeout(), crate::bindings::convert::ms_f64_to_u64);
    let ev = self.inner.wait_for_event(&event, timeout).await.into_js_with(&ctx)?;
    match ev {
      ContextEvent::WebError(err) => {
        Class::instance(ctx.clone(), crate::bindings::web_error::WebErrorJs::new(err))?.into_js(&ctx)
      },
      ContextEvent::Download(d) => {
        Class::instance(ctx.clone(), crate::bindings::download::DownloadJs::new(d))?.into_js(&ctx)
      },
      ContextEvent::FrameAttached { page, frame_id }
      | ContextEvent::FrameDetached { page, frame_id }
      | ContextEvent::FrameNavigated { page, frame_id } => Class::instance(
        ctx.clone(),
        crate::bindings::frame::FrameJs::new(page.frame_for_id(&frame_id)),
      )?
      .into_js(&ctx),
      ContextEvent::PageClose(page) | ContextEvent::PageLoad(page) => {
        Class::instance(ctx.clone(), crate::bindings::page::pagejs_for_ctx(&ctx, page))?.into_js(&ctx)
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
    let vm = match ctx.userdata::<crate::engine::SessionVm>() {
      Some(ud) => ud.0.clone(),
      None => {
        return Err(rquickjs::Error::new_from_js_message(
          "BrowserContext.exposeBinding",
          "Error",
          "exposeBinding requires the script engine's VM handle".to_string(),
        ));
      },
    };
    let saved = crate::bindings::page::SavedCallback::save(ctx, callback);
    crate::bindings::page::insert_exposed_callback(ctx, name.to_string(), saved)?;

    let name = name.to_string();
    let binding: ferridriver::ExposedBinding = Arc::new(move |source, args| {
      let vm = vm.clone();
      let name = name.clone();
      Box::pin(async move {
        let out: Result<rquickjs::Result<serde_json::Value>, crate::error::ScriptError> = crate::vm_with!(vm => |ctx| {
          let saved = crate::bindings::page::get_exposed_callback(&ctx, &name)?
            .ok_or_else(|| {
              rquickjs::Error::new_from_js_message(
                "BrowserContext.exposeBinding",
                "Error",
                "exposed callback gone".to_string(),
              )
            })?;
          let f = saved.restore(&ctx)?;
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
          let res = crate::bindings::fetch::bracket_net(
            crate::bindings::fetch::policy_cell(&ctx),
            saved.net().cloned(),
            async {
              let mp: rquickjs::promise::MaybePromise<'_> = call_args.apply(&f)?;
              mp.into_future::<rquickjs::Value<'_>>().await
            },
          )
          .await?;
          let json = match ctx.json_stringify(res)? {
            Some(s) => serde_json::from_str(&s.to_string()?).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
          };
          Ok(json)
        })
        .await;
        out.map_or(serde_json::Value::Null, |inner| {
          inner.unwrap_or(serde_json::Value::Null)
        })
      })
    });
    Ok(binding)
  }

  /// Build the `{ dispose() }` Disposable returned from
  /// `exposeBinding` / `exposeFunction`. `dispose()` removes the
  /// binding from the registry and from every page in the context
  /// (`window[name]` is deleted on each page), and releases the
  /// persisted QuickJS callback so it doesn't sit in the session VM's
  /// name-keyed registry forever.
  fn make_disposable<'js>(&self, ctx: &Ctx<'js>, name: String) -> rquickjs::Result<Value<'js>> {
    let obj = rquickjs::Object::new(ctx.clone())?;
    let inner = self.inner.clone();
    let dispose = rquickjs::Function::new(
      ctx.clone(),
      rquickjs::prelude::Async(move |ctx: Ctx<'_>| {
        // Already on the interpreter (dispose is JS-invoked), so the
        // userdata registry is directly reachable — drop the stashed
        // callback synchronously, then have core remove `window[name]`
        // from every page (the future must not borrow `ctx`).
        crate::bindings::page::remove_exposed_callback(&ctx, &name);
        let inner = inner.clone();
        let name = name.clone();
        async move {
          let _ = inner.remove_exposed_binding(&name).await;
        }
      }),
    )?;
    obj.set("dispose", dispose)?;
    rquickjs::IntoJs::into_js(obj, ctx)
  }
}

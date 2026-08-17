//! `BrowserContextJs`: JS wrapper around `ferridriver::context::ContextRef`.

use std::sync::Arc;

use ferridriver::context::ContextRef;
use rquickjs::function::{Opt, This};
use rquickjs::{Ctx, JsLifetime, Value, class::Class, class::Trace};
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

  /// `context.clock` — Playwright's fake-time controller. Exposed as a
  /// JS property.
  #[qjs(get, rename = "clock")]
  pub fn clock(&self) -> crate::bindings::clock::ClockJs {
    crate::bindings::clock::ClockJs::new(self.inner.clone())
  }

  /// `context.request` — the context-bound HTTP client sharing this
  /// context's cookies both ways (Playwright: `browserContext.request`;
  /// `page.request` returns the same context-bound client).
  #[qjs(get, rename = "request")]
  pub fn request(&self) -> crate::bindings::http_client::HttpClientJs {
    crate::bindings::http_client::HttpClientJs::new(Arc::new(self.inner.http_client()))
  }

  /// Playwright: `browserContext.newCDPSession(page)`. Attaches a raw
  /// CDP session to the page's target. Chromium-only. Playwright also
  /// accepts an OOPIF `Frame`; ferridriver currently supports the
  /// `Page` form.
  #[qjs(rename = "newCDPSession")]
  pub async fn new_cdp_session<'js>(
    &self,
    ctx: Ctx<'js>,
    page: rquickjs::Class<'js, crate::bindings::page::PageJs>,
  ) -> rquickjs::Result<Value<'js>> {
    let core_page = page.borrow().page_arc();
    let session = self.inner.new_cdp_session(&core_page).await.into_js_with(&ctx)?;
    let instance =
      rquickjs::class::Class::instance(ctx.clone(), crate::bindings::cdp_session::CdpSessionJs::new(session))?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  // ── Cookies ───────────────────────────────────────────────────────────────

  /// All cookies visible in this context.
  ///
  /// Returns an array of `{ name, value, domain, path, secure, httpOnly,
  /// expires, sameSite }` objects matching Playwright's cookie shape.
  #[qjs(rename = "cookies")]
  pub async fn cookies<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
  ) -> rquickjs::Result<Value<'js>> {
    call_site
      .scope(async move {
        let cookies = self.inner.cookies().await.into_js_with(&ctx)?;
        serde_to_js(&ctx, &cookies)
      })
      .await
  }

  /// Append cookies to this context.
  ///
  /// `cookies` is an array matching Playwright's `SetNetworkCookieParam[]`:
  /// only `name` + `value` are required, plus either `url` OR `domain`+`path`.
  /// `secure`, `httpOnly`, `sameSite`, `expires` all default when absent.
  #[qjs(rename = "addCookies")]
  pub async fn add_cookies<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    cookies: Value<'js>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let parsed: Vec<ferridriver::backend::SetCookieParams> = serde_from_js(&ctx, cookies)?;
        let cookies: Vec<ferridriver::backend::CookieData> = parsed.into_iter().map(Into::into).collect();
        self.inner.add_cookies(cookies).await.into_js_with(&ctx)
      })
      .await
  }

  /// Playwright: `context.clearCookies(options?)`. Without options
  /// clears every cookie; with `{ name?, domain?, path? }` only
  /// cookies matching ALL specified filters are cleared. Each filter
  /// is `string | RegExp` — exact match for strings, `.test()` for
  /// regexes (Playwright's `server/browserContext.ts::clearCookies`).
  #[qjs(rename = "clearCookies")]
  pub async fn clear_cookies<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        match options.0 {
          None => self.inner.clear_cookies().await.into_js_with(&ctx),
          Some(v) if v.is_undefined() || v.is_null() => self.inner.clear_cookies().await.into_js_with(&ctx),
          Some(v) => {
            let obj = v.as_object().ok_or_else(|| {
              rquickjs::Error::new_from_js_message(
                "BrowserContext.clearCookies",
                "options",
                "expected an options object".to_string(),
              )
            })?;
            let field = |key: &str| -> rquickjs::Result<Option<ferridriver::options::StringOrRegex>> {
              let value: rquickjs::Value<'_> = obj.get(key)?;
              if value.is_undefined() || value.is_null() {
                return Ok(None);
              }
              crate::bindings::page::options::string_or_regex_from_js(value).map(Some)
            };
            let core = ferridriver::backend::ClearCookieOptions {
              name: field("name")?,
              domain: field("domain")?,
              path: field("path")?,
            };
            self.inner.clear_cookies_filtered(&core).await.into_js_with(&ctx)
          },
        }
      })
      .await
  }

  /// Delete a cookie by name (optionally scoped to a domain).
  #[qjs(rename = "deleteCookie")]
  pub async fn delete_cookie(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
    name: String,
    domain: Opt<String>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        self
          .inner
          .delete_cookie(&name, domain.0.as_deref())
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Export the current storage state — cookies + per-origin localStorage.
  ///
  /// Playwright: `storageState(options?: { path?, indexedDB? })
  ///   : Promise<{ cookies, origins }>`
  /// (`/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:460`).
  /// `path` writes the JSON to disk; `indexedDB` is accepted for parity but
  /// IndexedDB is not yet collected.
  #[qjs(rename = "storageState")]
  pub async fn storage_state<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    call_site
      .scope(async move {
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
      })
      .await
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
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
    permissions: Vec<String>,
    origin: Opt<String>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        self
          .inner
          .grant_permissions(&permissions, origin.0.as_deref())
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Revoke all previously granted permissions.
  #[qjs(rename = "clearPermissions")]
  pub async fn clear_permissions(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move { self.inner.clear_permissions().await.into_js_with(&ctx) })
      .await
  }

  // ── Emulation ─────────────────────────────────────────────────────────────

  /// Playwright: `browserContext.setGeolocation(geolocation | null)` —
  /// `{ latitude, longitude, accuracy? }` sets the override, `null`
  /// clears it.
  #[qjs(rename = "setGeolocation")]
  pub async fn set_geolocation<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    geolocation: Value<'js>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let geo = if geolocation.is_null() || geolocation.is_undefined() {
          None
        } else {
          #[derive(serde::Deserialize)]
          #[serde(rename_all = "camelCase")]
          struct JsGeolocation {
            latitude: f64,
            longitude: f64,
            accuracy: Option<f64>,
          }
          let parsed: JsGeolocation = crate::bindings::convert::serde_from_js(&ctx, geolocation)?;
          Some(ferridriver::options::Geolocation {
            latitude: parsed.latitude,
            longitude: parsed.longitude,
            accuracy: parsed.accuracy.unwrap_or(1.0),
          })
        };
        self.inner.set_geolocation(geo).await.into_js_with(&ctx)
      })
      .await
  }

  /// Toggle offline mode for this context.
  #[qjs(rename = "setOffline")]
  pub async fn set_offline(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
    offline: bool,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move { self.inner.set_offline(offline).await.into_js_with(&ctx) })
      .await
  }

  /// Playwright: `browserContext.setHTTPCredentials(httpCredentials |
  /// null)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:355`.
  /// Accepts `{ username, password, origin?, send? }` or `null` /
  /// `undefined` (clears stored credentials).
  #[qjs(rename = "setHTTPCredentials")]
  pub async fn set_http_credentials<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    credentials: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
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
      })
      .await
  }

  /// Set HTTP headers sent with every request in this context.
  ///
  /// `headers` is a plain object (e.g. `{ 'X-Foo': 'bar' }`).
  #[qjs(rename = "setExtraHTTPHeaders")]
  pub async fn set_extra_http_headers<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    headers: Value<'js>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let map: FxHashMap<String, String> = serde_from_js(&ctx, headers)?;
        self.inner.set_extra_http_headers(&map).await.into_js_with(&ctx)
      })
      .await
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
    call_site: crate::bindings::CallSite,
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
              route.reject_as_unmatched();
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
    Ok(rquickjs::promise::Promised::from(call_site.scope(async move {
      inner.route(matcher, rust_handler, times).await.into_js_with(&ctx)?;
      Ok(())
    })))
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
  pub async fn route_from_har<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    har: String,
    options: rquickjs::function::Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::page::parse_har_options(&ctx, &options)?;
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
  pub async fn unroute<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    url: Value<'js>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
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
      })
      .await
  }

  /// Playwright:
  /// `browserContext.unrouteAll(options?: { behavior?: 'wait' | 'ignoreErrors' | 'default' })`.
  /// Removes every route registered via `context.route` (page-scoped
  /// routes stay active), clearing the script-side predicate/handler
  /// tables for this context too.
  #[qjs(rename = "unrouteAll")]
  pub async fn unroute_all<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
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
      })
      .await
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
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    script: Value<'js>,
    arg: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    call_site
      .scope(async move {
        let (init, arg_json) = init_script_from_js(&ctx, script, arg.0)?;
        let disposable = self.inner.add_init_script(init, arg_json).await.into_js_with(&ctx)?;
        let instance =
          rquickjs::class::Class::instance(ctx.clone(), crate::bindings::disposable::DisposableJs::new(disposable))?;
        rquickjs::IntoJs::into_js(instance, &ctx)
      })
      .await
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
  pub async fn close(&self, call_site: crate::bindings::CallSite, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
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
      })
      .await
  }

  // ── Page creation ──────────────────────────────────────────────────────

  /// Playwright: `browser.newContext().newPage(): Promise<Page>` —
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts` (on
  /// `BrowserContext`). Opens a new tab in this context; the returned
  /// [`crate::bindings::page::PageJs`] inherits the context's
  /// `recordVideo` configuration (if any) and every other per-context
  /// setting wired through [`ContextRef`].
  #[qjs(rename = "newPage")]
  pub async fn new_page<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
  ) -> rquickjs::Result<Value<'js>> {
    call_site
      .scope(async move {
        use rquickjs::class::Class;
        let page = self.inner.new_page().await.into_js_with(&ctx)?;
        let wrapper = crate::bindings::page::pagejs_for_ctx(&ctx, page);
        let instance = Class::instance(ctx.clone(), wrapper)?;
        rquickjs::IntoJs::into_js(instance, &ctx)
      })
      .await
  }

  /// Playwright: `browserContext.pages(): Page[]` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts`.
  /// All open pages in this context (Rust core resolves them from the
  /// browser state, so the list is a promise here).
  #[qjs(rename = "pages")]
  pub async fn pages<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    use rquickjs::IntoJs;
    use rquickjs::class::Class;
    let pages = self.inner.pages().await.into_js_with(&ctx)?;
    let arr = rquickjs::Array::new(ctx.clone())?;
    for (i, page) in pages.into_iter().enumerate() {
      let wrapper = crate::bindings::page::pagejs_for_ctx(&ctx, page);
      let instance = Class::instance(ctx.clone(), wrapper)?;
      arr.set(i, instance)?;
    }
    arr.into_js(&ctx)
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
    call_site: crate::bindings::CallSite,
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
    Ok(rquickjs::promise::Promised::from(call_site.scope(async move {
      inner.expose_binding(&name, binding).await.into_js_with(&ctx)?;
      Ok(disposable)
    })))
  }

  /// Playwright: `browserContext.exposeFunction(name, callback)` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:370`.
  ///
  /// `exposeFunction` is `exposeBinding` minus the `source` argument:
  /// the callback receives only the spread page-side call args.
  #[qjs(rename = "exposeFunction")]
  pub fn expose_function<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    name: String,
    callback: rquickjs::Function<'js>,
  ) -> rquickjs::Result<
    rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<Value<'js>>> + 'js>,
  > {
    let binding = self.make_binding(&ctx, &name, callback, false)?;
    let disposable = self.make_disposable(&ctx, name.clone())?;
    let inner = self.inner.clone();
    Ok(rquickjs::promise::Promised::from(call_site.scope(async move {
      inner.expose_binding(&name, binding).await.into_js_with(&ctx)?;
      Ok(disposable)
    })))
  }

  // ── Event emitter (Playwright parity) ────────────────────────────────

  /// `context.on(event, listener)` — `'page'`, `'weberror'`,
  /// `'download'`, `'frameattached'`, `'framedetached'`,
  /// `'framenavigated'`, `'pageclose'`, `'pageload'`. Returns the
  /// context so registrations chain, as Playwright's do; the listener
  /// receives the same live class instance `waitForEvent` resolves to.
  #[qjs(rename = "on")]
  pub fn on<'js>(
    this: This<Class<'js, Self>>,
    ctx: Ctx<'js>,
    event: String,
    listener: rquickjs::Function<'js>,
  ) -> rquickjs::Result<Class<'js, Self>> {
    this
      .0
      .borrow()
      .register_listener(&ctx, &event, listener, false, false)?;
    Ok(this.0)
  }

  /// Node's `addListener`, an alias of [`Self::on`].
  #[qjs(rename = "addListener")]
  pub fn add_listener<'js>(
    this: This<Class<'js, Self>>,
    ctx: Ctx<'js>,
    event: String,
    listener: rquickjs::Function<'js>,
  ) -> rquickjs::Result<Class<'js, Self>> {
    Self::on(this, ctx, event, listener)
  }

  /// `context.once(event, listener)`.
  #[qjs(rename = "once")]
  pub fn once<'js>(
    this: This<Class<'js, Self>>,
    ctx: Ctx<'js>,
    event: String,
    listener: rquickjs::Function<'js>,
  ) -> rquickjs::Result<Class<'js, Self>> {
    this.0.borrow().register_listener(&ctx, &event, listener, true, false)?;
    Ok(this.0)
  }

  /// Node's `prependListener`.
  #[qjs(rename = "prependListener")]
  pub fn prepend_listener<'js>(
    this: This<Class<'js, Self>>,
    ctx: Ctx<'js>,
    event: String,
    listener: rquickjs::Function<'js>,
  ) -> rquickjs::Result<Class<'js, Self>> {
    this.0.borrow().register_listener(&ctx, &event, listener, false, true)?;
    Ok(this.0)
  }

  /// Node's `prependOnceListener`.
  #[qjs(rename = "prependOnceListener")]
  pub fn prepend_once_listener<'js>(
    this: This<Class<'js, Self>>,
    ctx: Ctx<'js>,
    event: String,
    listener: rquickjs::Function<'js>,
  ) -> rquickjs::Result<Class<'js, Self>> {
    this.0.borrow().register_listener(&ctx, &event, listener, true, true)?;
    Ok(this.0)
  }

  /// `context.off(event, listener)` — removal by function identity.
  /// `off(event)` alone drops every listener for that event.
  #[qjs(rename = "off")]
  pub fn off<'js>(
    this: This<Class<'js, Self>>,
    ctx: Ctx<'js>,
    event: String,
    listener: Opt<Value<'js>>,
  ) -> rquickjs::Result<Class<'js, Self>> {
    this.0.borrow().off_impl(&ctx, &event, listener)?;
    Ok(this.0)
  }

  /// Node's `removeListener`, an alias of [`Self::off`].
  #[qjs(rename = "removeListener")]
  pub fn remove_listener<'js>(
    this: This<Class<'js, Self>>,
    ctx: Ctx<'js>,
    event: String,
    listener: Opt<Value<'js>>,
  ) -> rquickjs::Result<Class<'js, Self>> {
    Self::off(this, ctx, event, listener)
  }

  /// `context.removeAllListeners(type?, options?)` — see the page form
  /// for what `behavior` can and cannot observe.
  #[qjs(rename = "removeAllListeners")]
  pub fn remove_all_listeners<'js>(
    this: This<Class<'js, Self>>,
    ctx: Ctx<'js>,
    event: Opt<String>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    {
      let context = this.0.borrow();
      let name = context.inner.name().to_string();
      let removed = with_context_callbacks(&ctx, |r| {
        let ids: Vec<u64> = r
          .listeners
          .iter()
          .filter(|(_, e)| e.context == name && event.0.as_ref().is_none_or(|ev| &e.event == ev))
          .map(|(id, _)| *id)
          .collect();
        for id in &ids {
          r.listeners.remove(id);
        }
        ids
      })?;
      for id in removed {
        context.inner.off(ferridriver::events::ListenerId(id));
      }
    }
    let Some(options) = options.0.filter(|v| !v.is_undefined() && !v.is_null()) else {
      return Ok(this.0.into_value());
    };
    let wait = options
      .as_object()
      .and_then(|o| o.get::<_, Option<String>>("behavior").ok().flatten())
      .is_some_and(|behavior| behavior == "wait");
    let tx = ensure_context_event_pump(&ctx);
    let promise = rquickjs::promise::Promise::wrap_future(&ctx, async move {
      if wait {
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        if tx.send(ContextEventMsg::Drain(done_tx)).await.is_ok() {
          let _ = done_rx.await;
        }
      }
      Ok::<(), rquickjs::Error>(())
    })?;
    Ok(promise.into_value())
  }

  /// Node's `listeners(type)`, in the order they will fire.
  #[qjs(rename = "listeners")]
  pub fn listeners<'js>(&self, ctx: Ctx<'js>, event: String) -> rquickjs::Result<Vec<Value<'js>>> {
    let name = self.inner.name().to_string();
    let saved = with_context_callbacks(&ctx, |r| {
      r.listeners
        .iter()
        .filter(|(_, e)| e.context == name && e.event == event)
        .map(|(id, e)| (*id, e.listener.clone()))
        .collect::<Vec<_>>()
    })?;
    let mut out = Vec::with_capacity(saved.len());
    for id in self.inner.listener_ids_named(&event) {
      if let Some((_, cb)) = saved.iter().find(|(saved_id, _)| *saved_id == id.0) {
        out.push(cb.restore(&ctx)?.into_value());
      }
    }
    Ok(out)
  }

  /// Node's `rawListeners(type)`; see the page form.
  #[qjs(rename = "rawListeners")]
  pub fn raw_listeners<'js>(&self, ctx: Ctx<'js>, event: String) -> rquickjs::Result<Vec<Value<'js>>> {
    self.listeners(ctx, event)
  }

  /// Node's `listenerCount(type)`.
  #[qjs(rename = "listenerCount")]
  pub fn listener_count(&self, event: String) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let count = self.inner.listener_count(&event) as f64;
    count
  }

  /// Node's `eventNames()`.
  #[qjs(rename = "eventNames")]
  pub fn event_names(&self) -> Vec<String> {
    self.inner.event_names()
  }

  /// Node's `setMaxListeners(n)`; `0` disables the leak warning.
  #[qjs(rename = "setMaxListeners")]
  pub fn set_max_listeners(this: This<Class<'_, Self>>, max: f64) -> Class<'_, Self> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let max = max.max(0.0) as usize;
    this.0.borrow().inner.set_max_listeners(max);
    this.0
  }

  /// Node's `getMaxListeners()`.
  #[qjs(rename = "getMaxListeners")]
  pub fn get_max_listeners(&self) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let max = self.inner.max_listeners() as f64;
    max
  }

  // ── Context-level events ───────────────────────────────────────────────

  /// Wait for the next context-scoped event. Supports `'page'` (new
  /// page created via `newPage`) and `'weberror'` plus the
  /// page-lifecycle mirror events (`'download'`, `'frameattached'`,
  /// `'framedetached'`, `'framenavigated'`, `'pageclose'`, `'pageload'`),
  /// resolving with the matching live class instance. Playwright:
  /// `browserContext.waitForEvent(event, options?)`.
  #[qjs(rename = "waitForEvent")]
  pub async fn wait_for_event<'js>(
    &self,
    ctx: Ctx<'js>,
    event: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    use ferridriver::events::ContextEvent;
    use rquickjs::IntoJs;
    use rquickjs::class::Class;
    let timeout = crate::bindings::convert::parse_timeout_number_or_bag(&ctx, options)?
      .unwrap_or_else(|| self.inner.default_timeout());
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
      ContextEvent::Page(page) | ContextEvent::PageClose(page) | ContextEvent::PageLoad(page) => {
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

// ── Context event emitter ────────────────────────────────────────────

/// One `context.on` / `context.once` registration.
struct ContextListenerEntry {
  event: String,
  /// Core context name, so listeners registered through one wrapper are
  /// removed through another (`page.context()` mints a fresh wrapper
  /// per call).
  context: String,
  listener: crate::bindings::page::SavedCallback,
}

#[derive(Default)]
struct ContextCallbacks {
  listeners: FxHashMap<u64, ContextListenerEntry>,
}

struct ContextCallbacksUd(std::cell::RefCell<ContextCallbacks>);

// SAFETY: holds only `'static` data (`Persistent` handles and owned
// values), same rationale as the page-callbacks userdata.
#[allow(unsafe_code)]
unsafe impl JsLifetime<'_> for ContextCallbacksUd {
  type Changed<'to> = ContextCallbacksUd;
}

fn with_context_callbacks<R>(ctx: &Ctx<'_>, f: impl FnOnce(&mut ContextCallbacks) -> R) -> rquickjs::Result<R> {
  if ctx.userdata::<ContextCallbacksUd>().is_none() {
    let _ = ctx.store_userdata(ContextCallbacksUd(std::cell::RefCell::new(ContextCallbacks::default())));
  }
  let ud = ctx.userdata::<ContextCallbacksUd>().ok_or_else(|| {
    rquickjs::Error::new_from_js_message("context", "Error", "context callbacks registry missing".to_string())
  })?;
  let mut reg = ud.0.borrow_mut();
  Ok(f(&mut reg))
}

/// Message the context-event pump consumes. Same shape and the same
/// rules as the page pump: a core callback fires on a backend thread and
/// may only `send`; the VM is touched exclusively by the pump future,
/// which the session's event loop polls on the interpreter thread.
enum ContextEventMsg {
  Event {
    id: u64,
    remove_after: bool,
    event: ferridriver::events::ContextEvent,
  },
  Drain(tokio::sync::oneshot::Sender<()>),
}

struct ContextEventPumpUd(tokio::sync::mpsc::Sender<ContextEventMsg>);

#[allow(unsafe_code)]
unsafe impl JsLifetime<'_> for ContextEventPumpUd {
  type Changed<'to> = ContextEventPumpUd;
}

/// Capacity of the context-event pump channel — the page pump's
/// rationale and bound, applied to the lower-rate context stream.
const CONTEXT_EVENT_PUMP_CAPACITY: usize = 1024;

fn ensure_context_event_pump(ctx: &Ctx<'_>) -> tokio::sync::mpsc::Sender<ContextEventMsg> {
  if let Some(ud) = ctx.userdata::<ContextEventPumpUd>() {
    return ud.0.clone();
  }
  let (tx, mut rx) = tokio::sync::mpsc::channel::<ContextEventMsg>(CONTEXT_EVENT_PUMP_CAPACITY);
  let pump_ctx = ctx.clone();
  ctx.spawn(async move {
    while let Some(msg) = rx.recv().await {
      let ContextEventMsg::Event {
        id,
        remove_after,
        event,
      } = msg
      else {
        if let ContextEventMsg::Drain(done) = msg {
          let _ = done.send(());
        }
        continue;
      };
      let Ok(Some(f)) = with_context_callbacks(&pump_ctx, |r| {
        let entry = r.listeners.get(&id).map(|e| e.listener.clone());
        if remove_after {
          r.listeners.remove(&id);
        }
        entry
      }) else {
        continue;
      };
      let Ok(arg) = context_event_to_js(&pump_ctx, event) else {
        continue;
      };
      let _: rquickjs::Result<Value<'_>> = f.call_bracketed(&pump_ctx, (arg,));
    }
  });
  let _ = ctx.store_userdata(ContextEventPumpUd(tx.clone()));
  tx
}

/// Lift a context event into the live class instance Playwright hands a
/// listener — the same mapping `waitForEvent` performs.
fn context_event_to_js<'js>(ctx: &Ctx<'js>, event: ferridriver::events::ContextEvent) -> rquickjs::Result<Value<'js>> {
  use ferridriver::events::ContextEvent;
  use rquickjs::IntoJs;
  use rquickjs::class::Class;
  match event {
    ContextEvent::WebError(err) => {
      Class::instance(ctx.clone(), crate::bindings::web_error::WebErrorJs::new(err))?.into_js(ctx)
    },
    ContextEvent::Download(d) => {
      Class::instance(ctx.clone(), crate::bindings::download::DownloadJs::new(d))?.into_js(ctx)
    },
    ContextEvent::FrameAttached { page, frame_id }
    | ContextEvent::FrameDetached { page, frame_id }
    | ContextEvent::FrameNavigated { page, frame_id } => Class::instance(
      ctx.clone(),
      crate::bindings::frame::FrameJs::new(page.frame_for_id(&frame_id)),
    )?
    .into_js(ctx),
    ContextEvent::Page(page) | ContextEvent::PageClose(page) | ContextEvent::PageLoad(page) => {
      Class::instance(ctx.clone(), crate::bindings::page::pagejs_for_ctx(ctx, page))?.into_js(ctx)
    },
  }
}

impl BrowserContextJs {
  /// Shared core for the context emitter: persist the JS listener, then
  /// register a core callback that only forwards `(id, event)` to this
  /// context's pump. The backend thread never touches the VM.
  fn register_listener<'js>(
    &self,
    ctx: &Ctx<'js>,
    event: &str,
    listener: rquickjs::Function<'js>,
    once: bool,
    front: bool,
  ) -> rquickjs::Result<()> {
    let saved = crate::bindings::page::SavedCallback::save(ctx, listener);
    let tx = ensure_context_event_pump(ctx);
    let id_slot = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let id_slot_cb = id_slot.clone();
    let callback: ferridriver::events::ContextEventCallback = Arc::new(move |ev: ferridriver::events::ContextEvent| {
      let id = id_slot_cb.load(std::sync::atomic::Ordering::Relaxed);
      let msg = ContextEventMsg::Event {
        id,
        remove_after: once,
        event: ev,
      };
      if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) = tx.try_send(msg) {
        tracing::warn!(
          listener_id = id,
          capacity = CONTEXT_EVENT_PUMP_CAPACITY,
          "context event pump full (VM idle between scripts?); dropping event"
        );
      }
    });
    let id = match (once, front) {
      (false, false) => self.inner.on(event, callback),
      (true, false) => self.inner.once(event, callback),
      (false, true) => self.inner.prepend_listener(event, callback),
      (true, true) => self.inner.prepend_once_listener(event, callback),
    };
    id_slot.store(id.0, std::sync::atomic::Ordering::Relaxed);
    with_context_callbacks(ctx, |r| {
      r.listeners.insert(
        id.0,
        ContextListenerEntry {
          event: event.to_string(),
          context: self.inner.name().to_string(),
          listener: saved,
        },
      );
    })?;
    Ok(())
  }

  fn off_impl<'js>(&self, ctx: &Ctx<'js>, event: &str, listener: Opt<Value<'js>>) -> rquickjs::Result<()> {
    let name = self.inner.name().to_string();
    let Some(listener_fn) = listener.0.as_ref().and_then(|v| v.as_function().cloned()) else {
      let ids = with_context_callbacks(ctx, |r| {
        let ids: Vec<u64> = r
          .listeners
          .iter()
          .filter(|(_, e)| e.context == name && e.event == event)
          .map(|(id, _)| *id)
          .collect();
        for id in &ids {
          r.listeners.remove(id);
        }
        ids
      })?;
      for id in ids {
        self.inner.off(ferridriver::events::ListenerId(id));
      }
      return Ok(());
    };
    let target: Value<'js> = listener_fn.into_value();
    let saved = with_context_callbacks(ctx, |r| {
      r.listeners
        .iter()
        .filter(|(_, e)| e.context == name && e.event == event)
        .map(|(id, e)| (*id, e.listener.clone()))
        .collect::<Vec<_>>()
    })?;
    for (id, cb) in saved {
      if cb.restore(ctx)?.into_value() == target {
        self.inner.off(ferridriver::events::ListenerId(id));
        with_context_callbacks(ctx, |r| r.listeners.remove(&id))?;
      }
    }
    Ok(())
  }
}

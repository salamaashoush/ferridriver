//! `BrowserContextJs`: JS wrapper around `ferridriver::context::ContextRef`.

mod events;
mod helpers;

pub(crate) use helpers::{clear_cookie_options, cookie_urls};

use std::sync::Arc;

use ferridriver::context::ContextRef;
use rquickjs::function::{Opt, This};
use rquickjs::{Ctx, JsLifetime, Value, class::Class, class::Trace};
use rustc_hash::FxHashMap;

use crate::bindings::convert::FerriResultCtxExt;
use crate::bindings::convert::throw_named;
use crate::bindings::convert::{init_script_from_js, serde_from_js, serde_to_js};
use crate::bindings::page::{RouteOwner, with_page_callbacks};
use helpers::page_array;

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

crate::bindings::macros::qjs_impl! {
  #[expect(
    clippy::needless_pass_by_value,
    reason = "rquickjs hands methods their Ctx and JS arguments by value"
  )]
  impl BrowserContextJs {
    @mixin event_emitter();
  /// `context.tracing` — Playwright's `Tracing` controller. Exposed as a
  /// JS property.
  #[qjs(get, rename = "tracing")]
  #[must_use]
  pub fn tracing(&self) -> crate::bindings::tracing::TracingJs {
    crate::bindings::tracing::TracingJs::new(Arc::clone(&self.inner))
  }

  /// `context.clock` — Playwright's fake-time controller. Exposed as a
  /// JS property.
  #[qjs(get, rename = "clock")]
  #[must_use]
  pub fn clock(&self) -> crate::bindings::clock::ClockJs {
    crate::bindings::clock::ClockJs::new(Arc::clone(&self.inner))
  }

  /// `context.credentials` — the virtual WebAuthn authenticator.
  #[qjs(get, rename = "credentials")]
  #[must_use]
  pub fn credentials(&self) -> crate::bindings::credentials::CredentialsJs {
    crate::bindings::credentials::CredentialsJs::new(self.inner.credentials())
  }

  /// `context.request` — the context-bound HTTP client sharing this
  /// context's cookies both ways (Playwright: `browserContext.request`;
  /// `page.request` returns the same context-bound client).
  #[qjs(get, rename = "request")]
  #[must_use]
  pub fn request(&self) -> crate::bindings::http_client::HttpClientJs {
    crate::bindings::http_client::HttpClientJs::new(Arc::new(self.inner.http_client()))
  }

  /// Playwright: `browserContext.newCDPSession(page)`. Attaches a raw
  /// CDP session to the page's target. Chromium-only. Playwright also
  /// accepts an OOPIF `Frame`; ferridriver currently supports the
  /// `Page` form.
  ///
  /// # Errors
  ///
  /// Rejects with the core error (`TargetClosedError`, or a `FerriError` off
  /// Chromium) when the page's target cannot be attached.
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
  ///
  /// Playwright: `cookies(urls?: string | string[])` — with URLs, only
  /// the cookies a request to one of them would carry.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `urls` is neither a string nor a string
  /// array, or with the core error when the browser cannot read its cookies.
  #[qjs(rename = "cookies")]
  pub async fn cookies<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    urls: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    call_site
      .scope(async move {
        let urls = cookie_urls(&ctx, urls.0)?;
        let cookies = self.inner.cookies_for(&urls).await.into_js_with(&ctx)?;
        serde_to_js(&ctx, &cookies)
      })
      .await
  }

  /// Append cookies to this context.
  ///
  /// `cookies` is an array matching Playwright's `SetNetworkCookieParam[]`:
  /// only `name` + `value` are required, plus either `url` OR `domain`+`path`.
  /// `secure`, `httpOnly`, `sameSite`, `expires` all default when absent.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `cookies` is not an array of cookie
  /// objects, or with the core error when the browser refuses one.
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
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` is not an object or a filter is
  /// neither a string nor a `RegExp`, or with the core error from the browser.
  #[qjs(rename = "clearCookies")]
  pub async fn clear_cookies<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    options: rquickjs::function::Opt<rquickjs::Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let filter = clear_cookie_options(&ctx, options.0, "BrowserContext.clearCookies")?;
        self
          .inner
          .clear_cookies()
          .maybe_options(filter)
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Delete a cookie by name (optionally scoped to a domain).
  ///
  /// # Errors
  ///
  /// Rejects with the core error when the browser cannot delete the cookie.
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

  /// Export the current storage state: cookies and per-origin
  /// `localStorage`, plus `IndexedDB` and the origin private file system
  /// when asked for.
  ///
  /// Playwright: `storageState(options?: { path?, indexedDB?, opfs?,
  ///   credentials? }): Promise<StorageState>`
  /// (`packages/playwright-core/src/client/browserContext.ts`). `path`
  /// writes the JSON to disk.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for a malformed `options` bag, a
  /// `PermissionDeniedError` when `path` is outside the realm's `write` grant, or
  /// the core error when collecting or writing the state fails.
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
          // camelCase would spell this `indexedDb`, a key no caller writes.
          #[serde(rename = "indexedDB")]
          indexed_db: Option<bool>,
          opfs: Option<bool>,
          credentials: Option<bool>,
        }
        let core_opts = match options.0 {
          Some(v) if !v.is_undefined() && !v.is_null() => {
            let parsed: JsStorageStateOptions = serde_from_js(&ctx, v)?;
            if let Some(path) = &parsed.path {
              crate::bindings::convert::writable(&ctx, std::path::Path::new(path))?;
            }
            Some(ferridriver::options::StorageStateOptions {
              path: parsed.path.map(std::path::PathBuf::from),
              indexed_db: parsed.indexed_db,
              opfs: parsed.opfs,
              credentials: parsed.credentials,
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
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `storageState` cannot be read as JSON, an
  /// `Error` when the state file cannot be read or parsed, or the core error when
  /// applying the state fails.
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

  /// Playwright: `browserContext.grantPermissions(permissions, options?:
  /// { origin? })`.
  ///
  /// # Errors
  ///
  /// Rejects with the core error when the browser refuses the grant.
  #[qjs(rename = "grantPermissions")]
  pub async fn grant_permissions<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'js>,
    permissions: Vec<String>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let origin = match options.0.as_ref().and_then(Value::as_object) {
      Some(bag) => bag.get::<_, Option<String>>("origin")?,
      None => None,
    };
    call_site
      .scope(async move {
        self
          .inner
          .grant_permissions(&permissions, origin.as_deref())
          .await
          .into_js_with(&ctx)
      })
      .await
  }

  /// Revoke all previously granted permissions.
  ///
  /// # Errors
  ///
  /// Rejects with the core error when the browser cannot clear the grants.
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
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `geolocation` lacks a numeric `latitude` or
  /// `longitude`, or with the core error from the browser.
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
  ///
  /// # Errors
  ///
  /// Rejects with the core error when the browser cannot change the network
  /// state.
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

  /// `browserContext.setNetworkConditions(preset | conditions | null)`.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `conditions` is neither a known preset nor a
  /// conditions object, or with the core error from the browser.
  #[qjs(rename = "setNetworkConditions")]
  pub async fn set_network_conditions<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    conditions: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let conditions = match conditions.0 {
          Some(v) if !v.is_undefined() && !v.is_null() => {
            Some(serde_from_js::<ferridriver::options::NetworkConditions>(&ctx, v)?)
          },
          _ => None,
        };
        self.inner.set_network_conditions(conditions).await.into_js_with(&ctx)
      })
      .await
  }

  /// `browserContext.setCPUThrottling(rate | null)`.
  ///
  /// # Errors
  ///
  /// Rejects with the core error when the browser cannot throttle the CPU.
  #[qjs(rename = "setCPUThrottling")]
  pub async fn set_cpu_throttling(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: rquickjs::Ctx<'_>,
    rate: Opt<Option<f64>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move { self.inner.set_cpu_throttling(rate.0.flatten()).await.into_js_with(&ctx) })
      .await
  }

  /// Playwright: `browserContext.setHTTPCredentials(httpCredentials:
  /// HttpCredentials | HttpCredentials[] | null)`
  /// (`packages/playwright-core/src/client/browserContext.ts`). `null`,
  /// `undefined` and an empty list clear the stored credentials.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for malformed credentials, or with the core error
  /// from the browser.
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
          Some(v) if !v.is_undefined() && !v.is_null() => {
            serde_from_js::<crate::bindings::browser::JsHttpCredentialsInput>(&ctx, v)?.into_core()
          },
          _ => None,
        };
        self.inner.set_http_credentials(creds).await.into_js_with(&ctx)
      })
      .await
  }

  /// Set HTTP headers sent with every request in this context.
  ///
  /// `headers` is a plain object (e.g. `{ 'X-Foo': 'bar' }`).
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `headers` is not an object of string values,
  /// or with the core error from the browser.
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
  ///
  /// # Errors
  ///
  /// Throws an `Error` when the session has no VM handle, or a `TypeError` when
  /// `url` is not a string, `RegExp` or predicate. The returned promise rejects
  /// with the core error when the route cannot be installed.
  #[qjs(rename = "route")]
  pub fn route<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    url: Value<'js>,
    handler: rquickjs::Function<'js>,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    options: rquickjs::function::Opt<Value<'js>>,
  ) -> rquickjs::Result<rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<()>> + 'js>> {
    let Some(vm) = ferrijs::vm_handle(&ctx) else {
      return Err(throw_named(
        &ctx,
        "Error",
        "context.route requires the script engine's VM handle",
      ));
    };
    let prepared = crate::bindings::page::routing::prepare_route(
      &ctx,
      vm,
      self.route_owner(),
      "context.route",
      &url,
      handler,
      &options,
    )?;
    let inner = Arc::clone(&self.inner);
    Ok(rquickjs::promise::Promised::from(call_site.scope(async move {
      inner
        .route(prepared.matcher, prepared.handler, prepared.times)
        .await
        .into_js_with(&ctx)?;
      Ok(())
    })))
  }

  /// Playwright: `browserContext.routeWebSocket(url, handler)`. Intercepts
  /// WebSocket connections matching `url` (glob string or `RegExp`) on every
  /// page in this context; the handler receives a live `WebSocketRoute`.
  /// One-shot create dispatch is shared with `page.routeWebSocket` via
  /// `build_ws_route_handler`; `onMessage`/`onClose` use the WS pump.
  ///
  /// # Errors
  ///
  /// Throws an `Error` when the session has no VM handle, or a `TypeError` when
  /// `url` is neither a string nor a `RegExp`. The returned promise rejects with
  /// the core error when the route cannot be installed.
  #[qjs(rename = "routeWebSocket")]
  pub fn route_web_socket<'js>(
    &self,
    ctx: Ctx<'js>,
    #[expect(
      clippy::needless_pass_by_value,
      reason = "rquickjs extracts JS arguments by value; a `&T` parameter would mean a borrowed class instance"
    )]
    url: Value<'js>,
    handler: rquickjs::Function<'js>,
  ) -> rquickjs::Result<rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<()>> + 'js>> {
    let Some(vm) = ferrijs::vm_handle(&ctx) else {
      return Err(throw_named(
        &ctx,
        "Error",
        "context.routeWebSocket requires the script engine's VM handle",
      ));
    };
    let (matcher, rust_handler) =
      crate::bindings::page::routing::prepare_ws_route(&ctx, vm, self.route_owner(), &url, handler)?;
    let inner = Arc::clone(&self.inner);
    Ok(rquickjs::promise::Promised::from(async move {
      inner.route_web_socket(matcher, rust_handler).await.into_js_with(&ctx)
    }))
  }

  /// Playwright: `browserContext.routeFromHAR(har, options?)`. Replay-only.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for an invalid `url` or `notFound` option, a
  /// `PermissionDeniedError` when the archive is outside the realm's grants, or the
  /// core error when the archive cannot be loaded.
  #[qjs(rename = "routeFromHAR")]
  pub async fn route_from_har<'js>(
    &self,
    ctx: rquickjs::Ctx<'js>,
    har: String,
    options: rquickjs::function::Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let opts = crate::bindings::page::parse_har_options(&ctx, &options)?;
    crate::bindings::page::check_har_paths(&ctx, &har, &opts)?;
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
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `url` is not a string, `RegExp` or
  /// predicate, or with the core error when the route cannot be removed.
  #[qjs(rename = "unroute")]
  pub async fn unroute<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    url: Value<'js>,
    handler: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        match crate::bindings::page::routing::prepare_unroute(&ctx, &self.route_owner(), &url, &handler)? {
          crate::bindings::page::routing::UnroutePlan::Noop => Ok(()),
          crate::bindings::page::routing::UnroutePlan::Predicates { matchers, handler_id } => {
            for m in matchers {
              self.inner.unroute(&m, handler_id).await.into_js_with(&ctx)?;
            }
            Ok(())
          },
          crate::bindings::page::routing::UnroutePlan::Single { matcher, handler_id } => {
            self.inner.unroute(&matcher, handler_id).await.into_js_with(&ctx)
          },
        }
      })
      .await
  }

  /// Playwright:
  /// `browserContext.unrouteAll(options?: { behavior?: 'wait' | 'ignoreErrors' | 'default' })`.
  /// Removes every route registered via `context.route` (page-scoped
  /// routes stay active), clearing the script-side predicate/handler
  /// tables for this context too.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` for an unknown `behavior`, or with the core error
  /// when a route cannot be removed.
  #[qjs(rename = "unrouteAll")]
  pub async fn unroute_all<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    call_site
      .scope(async move {
        let behavior = crate::bindings::page::routing::parse_unroute_all_behavior(&ctx, options)?;
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
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `script` is not a function, string or
  /// `{ path?, content? }` or `arg` cannot be serialised, a `PermissionDeniedError`
  /// when `path` is outside the realm's `read` grant, or the core error when the
  /// script cannot be installed.
  #[qjs(rename = "addInitScript")]
  pub async fn add_init_script<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
    script: Value<'js>,
    arg: Opt<Value<'js>>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    call_site
      .scope(async move {
        let functions = crate::bindings::convert::ArgFunctions::from_options(options.0.as_ref(), None)?;
        let disposable = if let crate::bindings::convert::ArgFunctions::Expose { .. } = functions {
          let init = crate::bindings::convert::init_script_source_from_js(&ctx, script)?;
          let arg = crate::bindings::convert::quickjs_arg_with_functions(&ctx, arg.0, functions)?;
          self.inner.add_init_script_exposing_functions(init, arg).await
        } else {
          let (init, arg_json) = init_script_from_js(&ctx, script, arg.0)?;
          self.inner.add_init_script(init, arg_json).await
        }
        .into_js_with(&ctx)?;
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
  #[must_use]
  pub fn name(&self) -> String {
    self.inner.name().to_string()
  }

  /// Playwright: `browserContext.browser(): Browser | null` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:290`.
  /// Returns the parent browser, or `null` if the context was not created
  /// from a `Browser`.
  ///
  /// # Errors
  ///
  /// Throws when the engine cannot allocate the `Browser` object.
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
  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.inner.is_closed()
  }

  /// Close the context (tears down the underlying browser state).
  ///
  /// # Errors
  ///
  /// Rejects with the core error when the context cannot be closed.
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
  ///
  /// # Errors
  ///
  /// Rejects with the core error when the browser cannot open the page.
  #[qjs(rename = "newPage")]
  pub async fn new_page<'js>(
    &self,
    call_site: crate::bindings::CallSite,
    ctx: Ctx<'js>,
  ) -> rquickjs::Result<Value<'js>> {
    call_site
      .scope(async move {
        let page = self.inner.new_page().await.into_js_with(&ctx)?;
        let instance = crate::bindings::identity::page_object(&ctx, page)?;
        rquickjs::IntoJs::into_js(instance, &ctx)
      })
      .await
  }

  /// Playwright: `browserContext.pages(): Page[]` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts`.
  /// The pages open in this context, in the order they were opened.
  ///
  /// # Errors
  ///
  /// Throws when the engine cannot allocate the array or a `Page` object.
  #[qjs(rename = "pages")]
  pub fn pages<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<rquickjs::Array<'js>> {
    page_array(&ctx, self.inner.pages())
  }

  /// [`Self::pages`] after asking the browser which windows it has. A
  /// classic `WebDriver` session is not told of a window opened behind
  /// its back, or of one that closed; every other backend is, and this
  /// only waits for it. Not in Playwright, which drives no such browser.
  ///
  /// # Errors
  ///
  /// Rejects with the core error when the browser cannot list its windows.
  #[qjs(rename = "discoverPages")]
  pub async fn discover_pages<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<rquickjs::Array<'js>> {
    let pages = self.inner.discover_pages().await.into_js_with(&ctx)?;
    page_array(&ctx, pages)
  }

  // ── Video recording ────────────────────────────────────────────────────

  /// Playwright:
  /// `browser.newContext({ recordVideo: { dir, size? } })` —
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:10150`.
  /// Transitional API: §4.1's `BrowserContextOptions` bag will fold
  /// this into the full options struct.
  ///
  /// # Errors
  ///
  /// Rejects with a `TypeError` when `options` lacks a string `dir`, a
  /// `PermissionDeniedError` when `dir` is outside the realm's `write` grant, or the
  /// core error from the browser.
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
    crate::bindings::convert::writable(&ctx, std::path::Path::new(&parsed.dir))?;
    let opts = ferridriver::options::RecordVideoOptions {
      dir: std::path::PathBuf::from(parsed.dir),
      size: parsed.size.map(|s| ferridriver::options::VideoSize {
        width: crate::bindings::convert::f64_to_u32(s.width),
        height: crate::bindings::convert::f64_to_u32(s.height),
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
  ///
  /// # Errors
  ///
  /// Throws an `Error` when the session has no VM handle. The returned promise
  /// rejects with the core error when the binding cannot be installed.
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
    let binding = Self::make_binding(&ctx, &name, callback, true)?;
    let disposable = self.make_disposable(&ctx, name.clone())?;
    let inner = Arc::clone(&self.inner);
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
  ///
  /// # Errors
  ///
  /// Throws an `Error` when the session has no VM handle. The returned promise
  /// rejects with the core error when the function cannot be installed.
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
    let binding = Self::make_binding(&ctx, &name, callback, false)?;
    let disposable = self.make_disposable(&ctx, name.clone())?;
    let inner = Arc::clone(&self.inner);
    Ok(rquickjs::promise::Promised::from(call_site.scope(async move {
      inner.expose_binding(&name, binding).await.into_js_with(&ctx)?;
      Ok(disposable)
    })))
  }

  // ── Event emitter (Playwright parity) ────────────────────────────────

  /// `context.removeAllListeners(type?, options?)`.
  ///
  /// # Errors
  ///
  /// Throws an `Error` when the context's listener registry is unavailable.
  #[qjs(rename = "removeAllListeners")]
  pub fn remove_all_listeners<'js>(
    this: This<Class<'js, Self>>,
    ctx: Ctx<'js>,
    event: Opt<String>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    Self::remove_all_listeners_impl(this, &ctx, event.0.as_deref(), options)
  }

  /// Node's `listeners(type)`, in the order they will fire.
  ///
  /// # Errors
  ///
  /// Throws an `Error` when the context's listener registry is unavailable.
  #[qjs(rename = "listeners")]
  pub fn listeners<'js>(&self, ctx: Ctx<'js>, event: String) -> rquickjs::Result<Vec<Value<'js>>> {
    self.listeners_impl(&ctx, &event)
  }

  // ── Context-level events ───────────────────────────────────────────────

  /// Playwright: `browserContext.waitForEvent(event, optionsOrPredicate?)`,
  /// resolving with the live class instance the event carries.
  #[qjs(rename = "waitForEvent")]
  #[must_use]
  pub fn wait_for_event<'js>(
    &self,
    ctx: Ctx<'js>,
    event: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::promise::Promised<impl std::future::Future<Output = rquickjs::Result<Value<'js>>> + 'js> {
    self.wait_for_event_impl(ctx, event, options)
  }
}
}

//! `BrowserContextJs`: JS wrapper around `ferridriver::context::ContextRef`.

use std::sync::Arc;

use ferridriver::context::ContextRef;
use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value, class::Trace};
use rustc_hash::FxHashMap;

use crate::bindings::convert::{FerriResultExt, init_script_from_js, serde_from_js, serde_to_js};

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

  /// Set HTTP headers sent with every request in this context.
  ///
  /// `headers` is a plain object (e.g. `{ 'X-Foo': 'bar' }`).
  #[qjs(rename = "setExtraHTTPHeaders")]
  pub async fn set_extra_http_headers<'js>(&self, ctx: Ctx<'js>, headers: Value<'js>) -> rquickjs::Result<()> {
    let map: FxHashMap<String, String> = serde_from_js(&ctx, headers)?;
    self.inner.set_extra_http_headers(&map).await.into_js()
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

  // `set_default_timeout` takes `&mut self` on core, which rquickjs can't
  // safely expose on `&self`. Expose read-only for now; callers that need to
  // change it can do so via the page's own timeout setters.

  // ── Lifecycle ─────────────────────────────────────────────────────────────

  /// Name of the session this context belongs to.
  #[qjs(rename = "name")]
  pub fn name(&self) -> String {
    self.inner.name().to_string()
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

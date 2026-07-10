//! `BrowserJs`: JS wrapper around [`ferridriver::Browser`].
//!
//! Exposes `browser.newContext(options?)` so scripts can exercise
//! [`ferridriver::options::BrowserContextOptions`] at its natural
//! Playwright entry point. The `browser` global is installed by
//! [`crate::bindings::install_browser`] when the run context carries a
//! `Browser` handle (see `engine::RunContext`). Tests that only need
//! the ambient `context` can continue to ignore it.
//!
//! Playwright reference:
//! `/tmp/playwright/packages/playwright-core/types/types.d.ts:22229`
//! (`BrowserContextOptions`) and `:9851` (`browser.newContext`).

use std::sync::Arc;

use ferridriver::Browser;
use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value, class::Class, class::Trace};

use super::context::BrowserContextJs;
use crate::bindings::convert::FerriResultCtxExt;
use crate::bindings::convert::serde_from_js;

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Browser")]
pub struct BrowserJs {
  #[qjs(skip_trace)]
  inner: Arc<Browser>,
}

impl BrowserJs {
  #[must_use]
  pub fn new(inner: Arc<Browser>) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl BrowserJs {
  /// Playwright: `browser.newContext(options?)` —
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts:9851`.
  /// Accepts the full `BrowserContextOptions` bag via the
  /// isomorphic serde lowering.
  #[qjs(rename = "newContext")]
  pub async fn new_context<'js>(&self, ctx: Ctx<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<Value<'js>> {
    let core_opts = match options.0 {
      None => None,
      Some(v) if v.is_undefined() || v.is_null() => None,
      Some(v) => {
        let parsed: JsBrowserContextOptions = serde_from_js(&ctx, v)?;
        Some(parsed.into_core())
      },
    };
    let ctx_ref = Arc::new(
      self
        .inner
        .new_context()
        .maybe_options(core_opts)
        .await
        .into_js_with(&ctx)?,
    );
    let wrapper = BrowserContextJs::new(ctx_ref);
    let instance = Class::instance(ctx.clone(), wrapper)?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  /// Playwright: `browser.version()` —
  /// `/tmp/playwright/packages/playwright-core/types/types.d.ts` on
  /// `Browser`. Returns the product version string captured at launch.
  #[qjs(rename = "version")]
  pub fn version(&self) -> String {
    self.inner.version().to_string()
  }

  /// Playwright: `browser.isConnected(): boolean` (sync).
  #[qjs(rename = "isConnected")]
  pub fn is_connected(&self) -> bool {
    self.inner.is_connected()
  }

  /// Playwright: `browser.close()`. Accepts no options here — the
  /// `BrowserCloseOptions { reason }` form can be added once a script
  /// case demands it.
  #[qjs(rename = "close")]
  pub async fn close(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    self
      .inner
      .close()
      .await
      .map_err(|e| crate::bindings::convert::ferri_throw(&ctx, &e))?;
    Ok(())
  }

  /// Playwright: `browser.waitForEvent(event, options?)`. Supports
  /// `'context'` — resolves with the live `BrowserContext` created by
  /// the next `browser.newContext(...)`.
  #[qjs(rename = "waitForEvent")]
  pub async fn wait_for_event<'js>(
    &self,
    ctx: Ctx<'js>,
    event: String,
    timeout_ms: Opt<f64>,
  ) -> rquickjs::Result<Value<'js>> {
    let timeout = timeout_ms.0.map_or(30000, crate::bindings::convert::ms_f64_to_u64);
    let ev = self.inner.wait_for_event(&event, timeout).await.into_js_with(&ctx)?;
    match ev {
      ferridriver::events::BrowserEvent::Context(ctx_ref) => {
        let wrapper = BrowserContextJs::new(Arc::new(ctx_ref));
        let instance = Class::instance(ctx.clone(), wrapper)?;
        rquickjs::IntoJs::into_js(instance, &ctx)
      },
    }
  }

  /// Playwright: `browser.contexts()`. Returns the list of contexts as
  /// JS handles.
  #[qjs(rename = "contexts")]
  pub fn contexts<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let contexts = self.inner.contexts();
    let arr = rquickjs::Array::new(ctx.clone())?;
    for (i, c) in contexts.into_iter().enumerate() {
      let wrapper = BrowserContextJs::new(std::sync::Arc::new(c));
      let instance = Class::instance(ctx.clone(), wrapper)?;
      arr.set(i, instance)?;
    }
    rquickjs::IntoJs::into_js(arr, &ctx)
  }

  /// Playwright: `browser.newPage()`. Creates a page in the default context.
  #[qjs(rename = "newPage")]
  pub async fn new_page<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let page = self.inner.new_page().await.into_js_with(&ctx)?;
    let wrapper = crate::bindings::page::pagejs_for_ctx(&ctx, page);
    let instance = Class::instance(ctx.clone(), wrapper)?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  /// Playwright: `browser.newBrowserCDPSession()`. Attaches a raw CDP
  /// session to the browser target. Chromium-only.
  #[qjs(rename = "newBrowserCDPSession")]
  pub async fn new_browser_cdp_session<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let session = self.inner.new_browser_cdp_session().await.into_js_with(&ctx)?;
    let instance = Class::instance(ctx.clone(), crate::bindings::cdp_session::CdpSessionJs::new(session))?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  /// The active page of the default context (mirrors NAPI `browser.page()`).
  #[qjs(rename = "page")]
  pub async fn page<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Value<'js>> {
    let page = self.inner.page().await.into_js_with(&ctx)?;
    let wrapper = crate::bindings::page::pagejs_for_ctx(&ctx, page);
    let instance = Class::instance(ctx.clone(), wrapper)?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  /// Playwright: `browser.bind(title, options?): Promise<{ endpoint }>` —
  /// `/tmp/playwright/packages/playwright-core/src/client/browser.ts:132`.
  /// Publishes this browser under session `title` and returns the endpoint
  /// clients connect to. `host`/`port` bind over TCP; otherwise a Unix socket.
  #[qjs(rename = "bind")]
  pub async fn bind<'js>(
    &self,
    ctx: Ctx<'js>,
    title: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let opts = match options.0 {
      Some(v) if !v.is_undefined() && !v.is_null() => {
        let parsed: JsBindOptions = serde_from_js(&ctx, v)?;
        parsed.into_core()
      },
      _ => ferridriver_session::BindOptions::default(),
    };
    let endpoint = ferridriver_session::bind_global(&self.inner, &title, opts, None)
      .await
      .map_err(|e| crate::bindings::convert::throw_named(&ctx, "Error", e.to_string()))?;
    let result = rquickjs::Object::new(ctx.clone())?;
    result.set("endpoint", endpoint)?;
    rquickjs::IntoJs::into_js(result, &ctx)
  }

  /// Playwright: `browser.unbind(): Promise<void>`. Stops the session server
  /// and removes the registry entry for whatever this browser is bound under.
  /// A no-op if never bound.
  #[qjs(rename = "unbind")]
  #[allow(clippy::unused_async, clippy::unused_async_trait_impl)] // QuickJS method must be async to return a JS Promise
  pub async fn unbind(&self, ctx: rquickjs::Ctx<'_>) -> rquickjs::Result<()> {
    ferridriver_session::unbind_browser(&self.inner)
      .map_err(|e| crate::bindings::convert::throw_named(&ctx, "Error", e.to_string()))?;
    Ok(())
  }
}

/// JS-side shape for [`BrowserJs::bind`]'s option bag.
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsBindOptions {
  workspace_dir: Option<String>,
  metadata: Option<serde_json::Value>,
  host: Option<String>,
  port: Option<u16>,
}

impl JsBindOptions {
  fn into_core(self) -> ferridriver_session::BindOptions {
    ferridriver_session::BindOptions {
      workspace_dir: self.workspace_dir,
      metadata: self.metadata,
      host: self.host,
      port: self.port,
    }
  }
}

/// JS-side shape for the options bag. Deserialised via serde-through-JSON
/// so aliased field names, null/undefined handling, and nested shapes
/// all match Playwright's client-side parsing. Convert with
/// [`Self::into_core`].
///
/// `pub(super)` so `super::browser_type::BrowserTypeJs` can reuse the
/// same parser for `launchPersistentContext`'s merged options bag.
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct JsBrowserContextOptions {
  accept_downloads: Option<bool>,
  #[serde(rename = "baseURL")]
  base_url: Option<String>,
  #[serde(rename = "bypassCSP")]
  bypass_csp: Option<bool>,
  color_scheme: Option<serde_json::Value>,
  contrast: Option<serde_json::Value>,
  device_scale_factor: Option<f64>,
  #[serde(rename = "extraHTTPHeaders")]
  extra_http_headers: Option<rustc_hash::FxHashMap<String, String>>,
  forced_colors: Option<serde_json::Value>,
  geolocation: Option<JsGeolocation>,
  has_touch: Option<bool>,
  http_credentials: Option<JsHttpCredentials>,
  #[serde(rename = "ignoreHTTPSErrors")]
  ignore_https_errors: Option<bool>,
  is_mobile: Option<bool>,
  java_script_enabled: Option<bool>,
  locale: Option<String>,
  offline: Option<bool>,
  permissions: Option<Vec<String>>,
  proxy: Option<JsProxyConfig>,
  record_video: Option<JsRecordVideoOptions>,
  reduced_motion: Option<serde_json::Value>,
  screen: Option<JsScreenSize>,
  service_workers: Option<String>,
  /// `string` = file path; `object` = inline state JSON. Playwright:
  /// `storageState: string | { cookies, origins }`.
  storage_state: Option<serde_json::Value>,
  strict_selectors: Option<bool>,
  timezone_id: Option<String>,
  user_agent: Option<String>,
  /// JS `null` → explicit opt-out; omitted → browser default.
  viewport: Option<serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct JsGeolocation {
  latitude: f64,
  longitude: f64,
  accuracy: Option<f64>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsHttpCredentials {
  username: String,
  password: String,
  origin: Option<String>,
  send: Option<String>,
}

#[derive(serde::Deserialize)]
struct JsProxyConfig {
  server: String,
  bypass: Option<String>,
  username: Option<String>,
  password: Option<String>,
}

#[derive(serde::Deserialize)]
struct JsScreenSize {
  width: i64,
  height: i64,
}

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

#[derive(serde::Deserialize)]
struct JsViewportSize {
  width: i64,
  height: i64,
}

fn lower_media(v: Option<serde_json::Value>) -> ferridriver::options::MediaOverride {
  match v {
    Some(serde_json::Value::Null) => ferridriver::options::MediaOverride::Disabled,
    Some(serde_json::Value::String(s)) => ferridriver::options::MediaOverride::Set(s),
    None | Some(_) => ferridriver::options::MediaOverride::Unchanged,
  }
}

impl JsBrowserContextOptions {
  #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
  pub(super) fn into_core(self) -> ferridriver::options::BrowserContextOptions {
    use ferridriver::options as fo;

    let viewport = match self.viewport {
      None => fo::ViewportOption::Default,
      Some(serde_json::Value::Null) => fo::ViewportOption::Null,
      Some(v) => {
        let parsed: Result<JsViewportSize, _> = serde_json::from_value(v);
        match parsed {
          Ok(vp) => fo::ViewportOption::Size {
            width: vp.width,
            height: vp.height,
          },
          Err(_) => fo::ViewportOption::Default,
        }
      },
    };

    let http_credentials = self.http_credentials.map(|c| fo::HttpCredentials {
      username: c.username,
      password: c.password,
      origin: c.origin,
      send: c.send.and_then(|s| match s.as_str() {
        "always" => Some(fo::HttpCredentialsSend::Always),
        "unauthorized" => Some(fo::HttpCredentialsSend::Unauthorized),
        _ => None,
      }),
    });
    let proxy = self.proxy.map(|p| fo::ProxyConfig {
      server: p.server,
      bypass: p.bypass,
      username: p.username,
      password: p.password,
    });
    let record_video = self.record_video.map(|rv| fo::RecordVideoOptions {
      dir: std::path::PathBuf::from(rv.dir),
      size: rv.size.map(|s| fo::VideoSize {
        width: s.width.max(0.0) as u32,
        height: s.height.max(0.0) as u32,
      }),
    });
    let screen = self.screen.map(|s| fo::ScreenSize {
      width: s.width,
      height: s.height,
    });
    let service_workers = self.service_workers.and_then(|s| match s.as_str() {
      "allow" => Some(fo::ServiceWorkerPolicy::Allow),
      "block" => Some(fo::ServiceWorkerPolicy::Block),
      _ => None,
    });

    fo::BrowserContextOptions {
      accept_downloads: self.accept_downloads,
      base_url: self.base_url,
      bypass_csp: self.bypass_csp,
      color_scheme: lower_media(self.color_scheme),
      contrast: lower_media(self.contrast),
      device_scale_factor: self.device_scale_factor,
      extra_http_headers: self.extra_http_headers,
      forced_colors: lower_media(self.forced_colors),
      geolocation: self.geolocation.map(|g| fo::Geolocation {
        latitude: g.latitude,
        longitude: g.longitude,
        accuracy: g.accuracy.unwrap_or(0.0),
      }),
      has_touch: self.has_touch,
      http_credentials,
      ignore_https_errors: self.ignore_https_errors,
      is_mobile: self.is_mobile,
      java_script_enabled: self.java_script_enabled,
      locale: self.locale,
      offline: self.offline,
      permissions: self.permissions,
      proxy,
      record_har: None,
      record_video,
      reduced_motion: lower_media(self.reduced_motion),
      screen,
      service_workers,
      storage_state: self.storage_state.map(|v| match v {
        serde_json::Value::String(path) => fo::StorageStateInput::Path(std::path::PathBuf::from(path)),
        other => fo::StorageStateInput::Inline(other),
      }),
      strict_selectors: self.strict_selectors,
      timezone_id: self.timezone_id,
      user_agent: self.user_agent,
      viewport,
    }
  }
}

//! `BrowserTypeJs`: JS wrapper around [`ferridriver::BrowserType`].
//!
//! Exposes `chromium()` / `firefox()` / `webkit()` as global factories
//! mirroring Playwright's
//! `import { chromium, firefox, webkit } from 'playwright'`. Each
//! returns a `BrowserType` carrying `name()`, `executablePath()`,
//! `launch()`, `connect()`, `connectOverCDP()`, and
//! `launchPersistentContext()`.
//!
//! Playwright reference:
//! `/tmp/playwright/packages/playwright-core/src/client/browserType.ts`.

use std::sync::Arc;

use ferridriver::options::{
  self as core_opts, BrowserTypeOptions, ChromiumTransport, ConnectOptions, ConnectOverCdpOptions, LaunchOptions,
  LaunchPersistentContextOptions,
};
use ferridriver::{Browser, BrowserType};
use rquickjs::function::Opt;
use rquickjs::{Ctx, JsLifetime, Value, class::Class, class::Trace};

use super::browser::BrowserJs;
use super::context::BrowserContextJs;
use crate::bindings::convert::{serde_from_js, to_rq_error};

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "BrowserType")]
pub struct BrowserTypeJs {
  #[qjs(skip_trace)]
  inner: BrowserType,
}

impl BrowserTypeJs {
  #[must_use]
  pub fn new(inner: BrowserType) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl BrowserTypeJs {
  /// Playwright `BrowserType.name()`.
  #[qjs(rename = "name")]
  pub fn name(&self) -> String {
    self.inner.name().to_string()
  }

  /// Playwright `BrowserType.executablePath()`.
  #[qjs(rename = "executablePath")]
  pub fn executable_path(&self) -> Option<String> {
    self.inner.executable_path().map(|p| p.to_string_lossy().into_owned())
  }

  /// Playwright `browserType.launch(options?)`.
  #[qjs(rename = "launch")]
  pub async fn launch<'js>(&self, ctx: Ctx<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<Value<'js>> {
    let core = match options.0 {
      None => LaunchOptions::default(),
      Some(v) if v.is_undefined() || v.is_null() => LaunchOptions::default(),
      Some(v) => parse_launch_options(&ctx, v)?,
    };
    let inner = self.inner.launch(core).await.map_err(|e| to_rq_error(&e))?;
    let wrapper = BrowserJs::new(Arc::new(inner));
    let instance = Class::instance(ctx.clone(), wrapper)?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  /// Playwright `browserType.connect(wsEndpoint, options?)`.
  #[qjs(rename = "connect")]
  pub async fn connect<'js>(
    &self,
    ctx: Ctx<'js>,
    ws_endpoint: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let core = match options.0 {
      None => ConnectOptions::default(),
      Some(v) if v.is_undefined() || v.is_null() => ConnectOptions::default(),
      Some(v) => parse_connect_options(&ctx, v)?,
    };
    let inner = self
      .inner
      .connect(&ws_endpoint, core)
      .await
      .map_err(|e| to_rq_error(&e))?;
    let wrapper = BrowserJs::new(Arc::new(inner));
    let instance = Class::instance(ctx.clone(), wrapper)?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  /// Playwright `browserType.connectOverCDP(endpointURL, options?)`. Chromium-only.
  #[qjs(rename = "connectOverCDP")]
  pub async fn connect_over_cdp<'js>(
    &self,
    ctx: Ctx<'js>,
    endpoint_url: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let core = match options.0 {
      None => ConnectOverCdpOptions::default(),
      Some(v) if v.is_undefined() || v.is_null() => ConnectOverCdpOptions::default(),
      Some(v) => parse_connect_over_cdp_options(&ctx, v)?,
    };
    let inner = self
      .inner
      .connect_over_cdp(&endpoint_url, core)
      .await
      .map_err(|e| to_rq_error(&e))?;
    let wrapper = BrowserJs::new(Arc::new(inner));
    let instance = Class::instance(ctx.clone(), wrapper)?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }

  /// Playwright `browserType.launchPersistentContext(userDataDir, options?)`.
  #[qjs(rename = "launchPersistentContext")]
  pub async fn launch_persistent_context<'js>(
    &self,
    ctx: Ctx<'js>,
    user_data_dir: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    let (launch, context) = match options.0 {
      None => (LaunchOptions::default(), core_opts::BrowserContextOptions::default()),
      Some(v) if v.is_undefined() || v.is_null() => {
        (LaunchOptions::default(), core_opts::BrowserContextOptions::default())
      },
      Some(v) => {
        let launch = parse_launch_options(&ctx, v.clone())?;
        let context = parse_context_options(&ctx, v)?;
        (launch, context)
      },
    };
    let core = LaunchPersistentContextOptions { launch, context };
    let ctx_ref = self
      .inner
      .launch_persistent_context(std::path::Path::new(&user_data_dir), core)
      .await
      .map_err(|e| to_rq_error(&e))?;
    let wrapper = BrowserContextJs::new(Arc::new(ctx_ref));
    let instance = Class::instance(ctx.clone(), wrapper)?;
    rquickjs::IntoJs::into_js(instance, &ctx)
  }
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsLaunchOptions {
  headless: Option<bool>,
  executable_path: Option<String>,
  args: Option<Vec<String>>,
  channel: Option<String>,
  slow_mo: Option<u64>,
  timeout: Option<u64>,
  downloads_path: Option<String>,
  traces_dir: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsConnectOptions {
  headers: Option<rustc_hash::FxHashMap<String, String>>,
  slow_mo: Option<u64>,
  timeout: Option<u64>,
  expose_network: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct JsConnectOverCdpOptions {
  headers: Option<rustc_hash::FxHashMap<String, String>>,
  slow_mo: Option<u64>,
  timeout: Option<u64>,
}

fn parse_launch_options<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<LaunchOptions> {
  let parsed: JsLaunchOptions = serde_from_js(ctx, value)?;
  Ok(LaunchOptions {
    headless: parsed.headless,
    executable_path: parsed.executable_path,
    args: parsed.args.unwrap_or_default(),
    channel: parsed.channel,
    env: None,
    slow_mo: parsed.slow_mo,
    timeout: parsed.timeout,
    downloads_path: parsed.downloads_path.map(std::path::PathBuf::from),
    ignore_default_args: None,
    handle_sighup: None,
    handle_sigint: None,
    handle_sigterm: None,
    chromium_sandbox: None,
    firefox_user_prefs: None,
    proxy: None,
    traces_dir: parsed.traces_dir.map(std::path::PathBuf::from),
  })
}

fn parse_connect_options<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<ConnectOptions> {
  let parsed: JsConnectOptions = serde_from_js(ctx, value)?;
  Ok(ConnectOptions {
    headers: parsed.headers,
    slow_mo: parsed.slow_mo,
    timeout: parsed.timeout,
    expose_network: parsed.expose_network,
  })
}

fn parse_connect_over_cdp_options<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<ConnectOverCdpOptions> {
  let parsed: JsConnectOverCdpOptions = serde_from_js(ctx, value)?;
  Ok(ConnectOverCdpOptions {
    headers: parsed.headers,
    slow_mo: parsed.slow_mo,
    timeout: parsed.timeout,
  })
}

fn parse_context_options<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<core_opts::BrowserContextOptions> {
  // Re-use the same JS-side schema as `browser.newContext(...)`. The
  // launch-options keys overlap (e.g. `headless`) but serde ignores
  // unknown fields per the `BrowserJs` parser.
  let parsed: super::browser::JsBrowserContextOptions = serde_from_js(ctx, value)?;
  Ok(parsed.into_core())
}

fn chromium_factory<'js>(ctx: Ctx<'js>, opts: Opt<Value<'js>>) -> rquickjs::Result<Class<'js, BrowserTypeJs>> {
  let transport = match opts.0 {
    None => None,
    Some(v) if v.is_undefined() || v.is_null() => None,
    Some(v) => {
      #[derive(serde::Deserialize, Default)]
      struct ChromiumOpts {
        transport: Option<String>,
      }
      let parsed: ChromiumOpts = serde_from_js(&ctx, v)?;
      parsed.transport.and_then(|t| match t.as_str() {
        "ws" => Some(ChromiumTransport::Ws),
        "pipe" => Some(ChromiumTransport::Pipe),
        _ => None,
      })
    },
  };
  let bt = BrowserType::chromium_with(&BrowserTypeOptions { transport });
  Class::instance(ctx, BrowserTypeJs::new(bt))
}

fn firefox_factory(ctx: Ctx<'_>) -> rquickjs::Result<Class<'_, BrowserTypeJs>> {
  Class::instance(ctx, BrowserTypeJs::new(BrowserType::firefox()))
}

fn webkit_factory(ctx: Ctx<'_>) -> rquickjs::Result<Class<'_, BrowserTypeJs>> {
  Class::instance(ctx, BrowserTypeJs::new(BrowserType::webkit()))
}

/// Install the top-level `chromium`, `firefox`, and `webkit` globals.
/// Mirrors Playwright's `import { chromium, firefox, webkit }` exactly:
/// `chromium()` is ALWAYS Chromium, `firefox()` ALWAYS Firefox,
/// `webkit()` ALWAYS WebKit. The wire backend is a per-product detail
/// (Chromium pipe vs `chromium({transport:'ws'})`; Firefox speaks
/// BiDi) — never a product swap.
pub fn install_browser_type(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  Class::<BrowserTypeJs>::define(&ctx.globals())?;

  ctx
    .globals()
    .set("chromium", rquickjs::Function::new(ctx.clone(), chromium_factory)?)?;
  ctx
    .globals()
    .set("firefox", rquickjs::Function::new(ctx.clone(), firefox_factory)?)?;
  ctx
    .globals()
    .set("webkit", rquickjs::Function::new(ctx.clone(), webkit_factory)?)?;
  crate::bindings::runtime::mirror_global(ctx, "chromium")?;
  crate::bindings::runtime::mirror_global(ctx, "firefox")?;
  crate::bindings::runtime::mirror_global(ctx, "webkit")?;

  // Suppress the unused-import warning for `Browser`, which is only
  // here to keep doc-link references valid in a future binding.
  let _ = std::marker::PhantomData::<Browser>;

  Ok(())
}

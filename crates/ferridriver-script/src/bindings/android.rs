//! `_android` — QuickJS binding for [`ferridriver::android::Android`].
//!
//! Mirrors Playwright's `client/android.ts`: `_android.devices(options?)`
//! and, on an `AndroidDevice`, `serial()`, `model()`, `shell(command)`,
//! `screenshot(options?)`, `push(file, path, options?)`,
//! `installApk(file, options?)`, `launchBrowser(options?)` and `close()`.
//!
//! A device is authority the realm has to hold like any other. The ADB
//! server is a host and a port, judged by the realm's network grant; a
//! file named by path is read under its read grant, and a screenshot
//! written to a path under its write grant.

use ferridriver::android::{Android, AndroidDevice, AndroidDevicesOptions, AndroidFile, AndroidLaunchBrowserOptions};
use rquickjs::class::Trace;
use rquickjs::function::Opt;
use rquickjs::{Class, Ctx, JsLifetime, Value};

use crate::bindings::convert::{FerriResultCtxExt, serde_from_js};

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "Android")]
pub struct AndroidJs {
  #[qjs(skip_trace)]
  inner: Android,
}

fn bag<'js, T: serde::de::DeserializeOwned + Default>(ctx: &Ctx<'js>, value: Opt<Value<'js>>) -> rquickjs::Result<T> {
  match value.0 {
    Some(value) if !value.is_undefined() && !value.is_null() => serde_from_js(ctx, value),
    _ => Ok(T::default()),
  }
}

/// Playwright's `string | Buffer` file argument: a path, read under the
/// realm's read grant, or the bytes themselves.
fn file<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> rquickjs::Result<AndroidFile> {
  if let Some(path) = value.as_string() {
    let path = std::path::PathBuf::from(path.to_string()?);
    ferrijs_std::permissions::check_read(ctx, &path)?;
    return Ok(AndroidFile::Path(path));
  }
  Ok(AndroidFile::Bytes(ferrijs::std::node::bytes::value_to_bytes(
    ctx, value, None,
  )?))
}

#[rquickjs::methods]
impl AndroidJs {
  /// Playwright: `android.devices(options?: { host?, port?,
  /// omitDriverInstall? }): Promise<AndroidDevice[]>`.
  #[qjs(rename = "devices")]
  pub async fn devices<'js>(
    &self,
    ctx: Ctx<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Vec<Class<'js, AndroidDeviceJs>>> {
    #[derive(serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    struct Options {
      host: Option<String>,
      port: Option<u16>,
      omit_driver_install: Option<bool>,
    }
    let options: Options = bag(&ctx, options)?;
    if let Some(policy) = crate::bindings::net_policy::realm_policy(&ctx) {
      policy
        .check(
          options.host.as_deref().unwrap_or("127.0.0.1"),
          Some(options.port.unwrap_or(5037)),
        )
        .map_err(|denied| ferrijs_std::permissions::throw_denied(&ctx, &denied))?;
    }
    let devices = self
      .inner
      .devices(AndroidDevicesOptions {
        host: options.host,
        port: options.port,
        omit_driver_install: options.omit_driver_install,
      })
      .await
      .into_js_with(&ctx)?;
    devices
      .into_iter()
      .map(|device| crate::bindings::identity::android_device_object(&ctx, device))
      .collect()
  }
}

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "AndroidDevice")]
pub struct AndroidDeviceJs {
  #[qjs(skip_trace)]
  inner: AndroidDevice,
}

impl AndroidDeviceJs {
  #[must_use]
  pub fn new(inner: AndroidDevice) -> Self {
    Self { inner }
  }
}

#[rquickjs::methods]
impl AndroidDeviceJs {
  /// Playwright: `androidDevice.serial(): string`.
  #[qjs(rename = "serial")]
  pub fn serial(&self) -> String {
    self.inner.serial().to_owned()
  }

  /// Playwright: `androidDevice.model(): string`.
  #[qjs(rename = "model")]
  pub fn model(&self) -> String {
    self.inner.model().to_owned()
  }

  /// Playwright: `androidDevice.shell(command: string): Promise<Buffer>`.
  #[qjs(rename = "shell")]
  pub async fn shell(&self, ctx: Ctx<'_>, command: String) -> rquickjs::Result<Vec<u8>> {
    self.inner.shell(&command).await.into_js_with(&ctx)
  }

  /// Playwright: `androidDevice.screenshot(options?: { path?: string }):
  /// Promise<Buffer>`.
  #[qjs(rename = "screenshot")]
  pub async fn screenshot<'js>(&self, ctx: Ctx<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<Vec<u8>> {
    #[derive(serde::Deserialize, Default)]
    struct Options {
      path: Option<std::path::PathBuf>,
    }
    let options: Options = bag(&ctx, options)?;
    if let Some(path) = &options.path {
      ferrijs_std::permissions::check_write(&ctx, path)?;
    }
    self.inner.screenshot(options.path.as_deref()).await.into_js_with(&ctx)
  }

  /// Playwright: `androidDevice.push(file: string | Buffer, path: string,
  /// options?: { mode?: number }): Promise<void>`.
  #[qjs(rename = "push")]
  pub async fn push<'js>(
    &self,
    ctx: Ctx<'js>,
    source: Value<'js>,
    path: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    #[derive(serde::Deserialize, Default)]
    struct Options {
      mode: Option<u32>,
    }
    let options: Options = bag(&ctx, options)?;
    let source = file(&ctx, &source)?;
    self.inner.push(source, &path, options.mode).await.into_js_with(&ctx)
  }

  /// Playwright: `androidDevice.installApk(file: string | Buffer,
  /// options?: { args?: string[] }): Promise<void>`.
  #[qjs(rename = "installApk")]
  pub async fn install_apk<'js>(
    &self,
    ctx: Ctx<'js>,
    source: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    #[derive(serde::Deserialize, Default)]
    struct Options {
      args: Option<Vec<String>>,
    }
    let options: Options = bag(&ctx, options)?;
    let source = file(&ctx, &source)?;
    self.inner.install_apk(source, options.args).await.into_js_with(&ctx)
  }

  /// Playwright: `androidDevice.launchBrowser(options?: BrowserContextOptions
  /// & { pkg?: string, args?: string[] }): Promise<BrowserContext>`.
  #[qjs(rename = "launchBrowser")]
  pub async fn launch_browser<'js>(&self, ctx: Ctx<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<Value<'js>> {
    #[derive(serde::Deserialize, Default)]
    struct Launch {
      pkg: Option<String>,
      #[serde(default)]
      args: Vec<String>,
    }
    let (launch, mut context) = match options.0 {
      Some(value) if !value.is_undefined() && !value.is_null() => (
        serde_from_js::<Launch>(&ctx, value.clone())?,
        serde_from_js::<crate::bindings::browser::JsBrowserContextOptions>(&ctx, value)?.into_core(),
      ),
      _ => (
        Launch::default(),
        ferridriver::options::BrowserContextOptions::default(),
      ),
    };
    // A device has one context, so its proxy is the browser's.
    let proxy = context.proxy.take();
    let context = self
      .inner
      .launch_browser(AndroidLaunchBrowserOptions {
        pkg: launch.pkg,
        args: launch.args,
        proxy,
        context,
      })
      .await
      .into_js_with(&ctx)?;
    let wrapper = crate::bindings::context::BrowserContextJs::new(std::sync::Arc::new(context));
    rquickjs::IntoJs::into_js(Class::instance(ctx.clone(), wrapper)?, &ctx)
  }

  /// Playwright: `androidDevice.close(): Promise<void>`.
  #[qjs(rename = "close")]
  pub async fn close(&self) {
    self.inner.close().await;
  }
}

/// Install the `_android` global, Playwright's `playwright._android`.
pub fn install_android(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  Class::<AndroidJs>::define(&ctx.globals())?;
  Class::<AndroidDeviceJs>::define(&ctx.globals())?;
  ctx.globals().set(
    "_android",
    Class::instance(ctx.clone(), AndroidJs { inner: Android::new() })?,
  )?;
  crate::bindings::runtime::mirror_global(ctx, "_android")
}

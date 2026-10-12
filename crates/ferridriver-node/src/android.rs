//! NAPI binding for [`ferridriver::android`].
//!
//! Mirrors Playwright's `client/android.ts`: the module's `_android`
//! object with `devices(options?)`, and on an `AndroidDevice` `serial()`,
//! `model()`, `shell(command)`, `screenshot(options?)`,
//! `push(file, path, options?)`, `installApk(file, options?)`,
//! `launchBrowser(options?)` and `close()`, the selector actions (`wait`,
//! `fill`, `press`, `tap`, `longTap`, `drag`, `fling`, `pinchClose`,
//! `pinchOpen`, `scroll`, `swipe`, `info`) and `input`.

use napi::Result;
use napi::bindgen_prelude::{Buffer, Either, FromNapiValue, Object, TypeName, ValidateNapiValue};
use napi_derive::napi;

use crate::error::IntoNapi;

/// Playwright: `android.devices(options?)`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct AndroidDevicesOptions {
  /// Where the ADB server listens. `127.0.0.1` when absent.
  pub host: Option<String>,
  /// The ADB server's port. `5037` when absent.
  pub port: Option<u32>,
  /// Leave the device's driver as it is instead of installing it.
  pub omit_driver_install: Option<bool>,
}

/// Playwright: `androidDevice.screenshot(options?)`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct AndroidScreenshotOptions {
  /// Also write the PNG to this path.
  pub path: Option<String>,
}

/// Playwright: `androidDevice.push(file, path, options?)`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct AndroidPushOptions {
  /// The file's mode on the device. `0o644` when absent.
  pub mode: Option<u32>,
}

/// Playwright: `androidDevice.installApk(file, options?)`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct AndroidInstallApkOptions {
  /// Arguments for `cmd package install`. `-r -t -S` when absent.
  pub args: Option<Vec<String>>,
}

/// Playwright's `launchBrowser` options: a context's options and, in the
/// same bag, the package and its extra switches.
pub struct AndroidLaunchBrowserOptions {
  context: crate::context::NapiBrowserContextOptions,
  pkg: Option<String>,
  args: Vec<String>,
}

impl TypeName for AndroidLaunchBrowserOptions {
  fn type_name() -> &'static str {
    "AndroidLaunchBrowserOptions"
  }

  fn value_type() -> napi::ValueType {
    napi::ValueType::Object
  }
}

impl ValidateNapiValue for AndroidLaunchBrowserOptions {}

impl FromNapiValue for AndroidLaunchBrowserOptions {
  unsafe fn from_napi_value(env: napi::sys::napi_env, value: napi::sys::napi_value) -> Result<Self> {
    let context = unsafe { crate::context::NapiBrowserContextOptions::from_napi_value(env, value) }?;
    let bag = unsafe { Object::from_napi_value(env, value) }?;
    Ok(Self {
      context,
      pkg: bag.get("pkg")?,
      args: bag.get("args")?.unwrap_or_default(),
    })
  }
}

/// Playwright's `AndroidSelector`, read from the object a caller passes:
/// a text condition is a string or a `RegExp`, and a selector nests in
/// `hasChild` and `hasDescendant`, neither of which a derived object
/// type can say.
pub struct AndroidSelector(ferridriver::android::AndroidSelector);

impl TypeName for AndroidSelector {
  fn type_name() -> &'static str {
    "AndroidSelector"
  }

  fn value_type() -> napi::ValueType {
    napi::ValueType::Object
  }
}

impl ValidateNapiValue for AndroidSelector {}

impl AndroidSelector {
  fn text(bag: &Object<'_>, name: &str) -> Result<Option<ferridriver::android::AndroidText>> {
    let Some(value) = bag.get::<Either<String, crate::types::JsRegExpLike>>(name)? else {
      return Ok(None);
    };
    Ok(Some(match value {
      Either::A(exact) => ferridriver::android::AndroidText::Exact(exact),
      Either::B(pattern) => ferridriver::android::AndroidText::Pattern(pattern.source),
    }))
  }

  fn read(bag: &Object<'_>) -> Result<ferridriver::android::AndroidSelector> {
    let nested = |name: &str| -> Result<Option<(Box<ferridriver::android::AndroidSelector>, Option<u32>)>> {
      let Some(holder) = bag.get::<Object<'_>>(name)? else {
        return Ok(None);
      };
      let inner = holder
        .get::<Object<'_>>("selector")?
        .ok_or_else(|| napi::Error::from_reason(format!("selector.{name}: expected {{ selector }}")))?;
      Ok(Some((Box::new(Self::read(&inner)?), holder.get("maxDepth")?)))
    };
    Ok(ferridriver::android::AndroidSelector {
      checkable: bag.get("checkable")?,
      checked: bag.get("checked")?,
      clazz: Self::text(bag, "clazz")?,
      clickable: bag.get("clickable")?,
      depth: bag.get("depth")?,
      desc: Self::text(bag, "desc")?,
      enabled: bag.get("enabled")?,
      focusable: bag.get("focusable")?,
      focused: bag.get("focused")?,
      has_child: nested("hasChild")?.map(|(child, _)| child),
      has_descendant: nested("hasDescendant")?,
      long_clickable: bag.get("longClickable")?,
      pkg: Self::text(bag, "pkg")?,
      res: Self::text(bag, "res")?,
      scrollable: bag.get("scrollable")?,
      selected: bag.get("selected")?,
      text: Self::text(bag, "text")?,
    })
  }
}

impl FromNapiValue for AndroidSelector {
  unsafe fn from_napi_value(env: napi::sys::napi_env, value: napi::sys::napi_value) -> Result<Self> {
    let bag = unsafe { Object::from_napi_value(env, value) }?;
    Self::read(&bag).map(Self)
  }
}

/// The options of a selector action that only waits.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct AndroidTimeoutOptions {
  /// Milliseconds; the device's default when absent. The driver reads
  /// `0` as "look once".
  pub timeout: Option<f64>,
}

/// Playwright: `androidDevice.wait(selector, options?)`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct AndroidWaitOptions {
  /// Sent as Playwright sends it; its driver does not read it and waits
  /// for a match either way.
  #[napi(ts_type = "'gone'")]
  pub state: Option<String>,
  pub timeout: Option<f64>,
}

/// Playwright: `androidDevice.tap(selector, options?)`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct AndroidTapOptions {
  /// How long the tap is held, in milliseconds.
  pub duration: Option<f64>,
  pub timeout: Option<f64>,
}

/// The options of a gesture.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct AndroidSpeedOptions {
  /// Pixels per second.
  pub speed: Option<f64>,
  pub timeout: Option<f64>,
}

/// Playwright: `AndroidElementInfo`.
#[napi(object, object_from_js = false)]
#[derive(Debug, Clone)]
pub struct AndroidElementInfo {
  pub clazz: String,
  pub desc: String,
  pub res: String,
  pub pkg: String,
  pub text: String,
  pub bounds: AndroidBounds,
  pub checkable: bool,
  pub checked: bool,
  pub clickable: bool,
  pub enabled: bool,
  pub focusable: bool,
  pub focused: bool,
  pub long_clickable: bool,
  pub scrollable: bool,
  pub selected: bool,
}

/// The bounds of a view on the device's screen.
#[napi(object, object_from_js = false)]
#[derive(Debug, Clone)]
pub struct AndroidBounds {
  pub x: f64,
  pub y: f64,
  pub width: f64,
  pub height: f64,
}

fn limit(timeout: Option<f64>) -> Option<u64> {
  timeout.map(crate::types::f64_to_u64)
}

fn direction(direction: &str) -> Result<ferridriver::android::AndroidDirection> {
  ferridriver::android::AndroidDirection::parse(direction).into_napi()
}

fn file(source: Either<String, Buffer>) -> ferridriver::android::AndroidFile {
  match source {
    Either::A(path) => ferridriver::android::AndroidFile::Path(path.into()),
    Either::B(bytes) => ferridriver::android::AndroidFile::Bytes(bytes.to_vec()),
  }
}

/// Playwright `Android`, the module's `_android`.
#[napi]
#[derive(Default)]
pub struct Android {
  inner: ferridriver::android::Android,
}

#[napi]
impl Android {
  /// Playwright: `android.devices(options?: { host?, port?,
  /// omitDriverInstall? }): Promise<AndroidDevice[]>`. The usable devices
  /// of the ADB server at `host:port`, `127.0.0.1:5037` when absent.
  #[napi(ts_return_type = "Promise<Array<AndroidDevice>>")]
  pub async fn devices(
    &self,
    options: Option<AndroidDevicesOptions>,
  ) -> Result<Vec<crate::identity::AndroidDeviceObject>> {
    let options = options.unwrap_or_default();
    let port = options
      .port
      .map(|port| u16::try_from(port).map_err(|_| napi::Error::from_reason(format!("port {port} is out of range"))))
      .transpose()?;
    let devices = self
      .inner
      .devices(ferridriver::android::AndroidDevicesOptions {
        host: options.host,
        port,
        omit_driver_install: options.omit_driver_install,
      })
      .await
      .into_napi()?;
    Ok(devices.into_iter().map(crate::identity::AndroidDeviceObject).collect())
  }
}

/// Playwright `AndroidDevice`: a device behind an ADB server.
#[napi]
pub struct AndroidDevice {
  inner: ferridriver::android::AndroidDevice,
}

impl AndroidDevice {
  pub(crate) fn from_core(inner: ferridriver::android::AndroidDevice) -> Self {
    Self { inner }
  }
}

#[napi]
impl AndroidDevice {
  /// Playwright: `androidDevice.serial(): string`.
  #[napi]
  pub fn serial(&self) -> String {
    self.inner.serial().to_owned()
  }

  /// Playwright: `androidDevice.model(): string`.
  #[napi]
  pub fn model(&self) -> String {
    self.inner.model().to_owned()
  }

  /// Playwright: `androidDevice.shell(command: string): Promise<Buffer>`.
  #[napi]
  pub async fn shell(&self, command: String) -> Result<Buffer> {
    self.inner.shell(&command).await.map(Buffer::from).into_napi()
  }

  /// Playwright: `androidDevice.screenshot(options?: { path?: string }):
  /// Promise<Buffer>`.
  #[napi]
  pub async fn screenshot(&self, options: Option<AndroidScreenshotOptions>) -> Result<Buffer> {
    let path = options.and_then(|options| options.path).map(std::path::PathBuf::from);
    self
      .inner
      .screenshot(path.as_deref())
      .await
      .map(Buffer::from)
      .into_napi()
  }

  /// Playwright: `androidDevice.push(file: string | Buffer, path: string,
  /// options?: { mode?: number }): Promise<void>`.
  #[napi(ts_args_type = "file: string | Buffer, path: string, options?: AndroidPushOptions")]
  pub async fn push(
    &self,
    source: Either<String, Buffer>,
    path: String,
    options: Option<AndroidPushOptions>,
  ) -> Result<()> {
    let source = file(source);
    self
      .inner
      .push(source, &path, options.and_then(|options| options.mode))
      .await
      .into_napi()
  }

  /// Playwright: `androidDevice.installApk(file: string | Buffer,
  /// options?: { args?: string[] }): Promise<void>`.
  #[napi(ts_args_type = "file: string | Buffer, options?: AndroidInstallApkOptions")]
  pub async fn install_apk(
    &self,
    source: Either<String, Buffer>,
    options: Option<AndroidInstallApkOptions>,
  ) -> Result<()> {
    let source = file(source);
    self
      .inner
      .install_apk(source, options.and_then(|options| options.args))
      .await
      .into_napi()
  }

  /// Playwright: `androidDevice.setDefaultTimeout(timeout: number): void`.
  #[napi]
  pub fn set_default_timeout(&self, timeout: f64) {
    self.inner.set_default_timeout(crate::types::f64_to_u64(timeout));
  }

  /// Playwright: `androidDevice.input: AndroidInput`.
  #[napi(getter)]
  pub fn input(&self) -> AndroidInput {
    AndroidInput {
      inner: self.inner.input(),
    }
  }

  /// Playwright: `androidDevice.wait(selector, options?: { state?: 'gone',
  /// timeout? }): Promise<void>`.
  #[napi(ts_args_type = "selector: import('./types').AndroidSelector, options?: AndroidWaitOptions")]
  pub async fn wait(&self, selector: AndroidSelector, options: Option<AndroidWaitOptions>) -> Result<()> {
    let options = options.unwrap_or_default();
    let gone = match options.state.as_deref() {
      None => false,
      Some("gone") => true,
      Some(other) => {
        return Err(napi::Error::from_reason(format!(
          "state: expected 'gone', got '{other}'"
        )));
      },
    };
    self
      .inner
      .wait(&selector.0, gone, limit(options.timeout))
      .await
      .into_napi()
  }

  /// Playwright: `androidDevice.fill(selector, text, options?): Promise<void>`.
  #[napi(ts_args_type = "selector: import('./types').AndroidSelector, text: string, options?: AndroidTimeoutOptions")]
  pub async fn fill(
    &self,
    selector: AndroidSelector,
    text: String,
    options: Option<AndroidTimeoutOptions>,
  ) -> Result<()> {
    self
      .inner
      .fill(&selector.0, &text, limit(options.and_then(|options| options.timeout)))
      .await
      .into_napi()
  }

  /// Playwright: `androidDevice.press(selector, key, options?): Promise<void>`.
  #[napi(
    ts_args_type = "selector: import('./types').AndroidSelector, key: import('./types').AndroidKey, options?: AndroidTimeoutOptions"
  )]
  pub async fn press(
    &self,
    selector: AndroidSelector,
    key: String,
    options: Option<AndroidTimeoutOptions>,
  ) -> Result<()> {
    self
      .inner
      .press(&selector.0, &key, limit(options.and_then(|options| options.timeout)))
      .await
      .into_napi()
  }

  /// Playwright: `androidDevice.tap(selector, options?: { duration?,
  /// timeout? }): Promise<void>`.
  #[napi(ts_args_type = "selector: import('./types').AndroidSelector, options?: AndroidTapOptions")]
  pub async fn tap(&self, selector: AndroidSelector, options: Option<AndroidTapOptions>) -> Result<()> {
    let options = options.unwrap_or_default();
    self
      .inner
      .tap(&selector.0, options.duration, limit(options.timeout))
      .await
      .into_napi()
  }

  /// Playwright: `androidDevice.longTap(selector, options?): Promise<void>`.
  #[napi(ts_args_type = "selector: import('./types').AndroidSelector, options?: AndroidTimeoutOptions")]
  pub async fn long_tap(&self, selector: AndroidSelector, options: Option<AndroidTimeoutOptions>) -> Result<()> {
    self
      .inner
      .long_tap(&selector.0, limit(options.and_then(|options| options.timeout)))
      .await
      .into_napi()
  }

  /// Playwright: `androidDevice.drag(selector, dest, options?: { speed?,
  /// timeout? }): Promise<void>`.
  #[napi(ts_args_type = "selector: import('./types').AndroidSelector, dest: Point, options?: AndroidSpeedOptions")]
  pub async fn drag(
    &self,
    selector: AndroidSelector,
    dest: crate::types::Point,
    options: Option<AndroidSpeedOptions>,
  ) -> Result<()> {
    let options = options.unwrap_or_default();
    self
      .inner
      .drag(&selector.0, dest.into(), options.speed, limit(options.timeout))
      .await
      .into_napi()
  }

  /// Playwright: `androidDevice.fling(selector, direction, options?): Promise<void>`.
  #[napi(
    ts_args_type = "selector: import('./types').AndroidSelector, direction: 'down' | 'up' | 'left' | 'right', options?: AndroidSpeedOptions"
  )]
  pub async fn fling(
    &self,
    selector: AndroidSelector,
    direction: String,
    options: Option<AndroidSpeedOptions>,
  ) -> Result<()> {
    let options = options.unwrap_or_default();
    self
      .inner
      .fling(
        &selector.0,
        self::direction(&direction)?,
        options.speed,
        limit(options.timeout),
      )
      .await
      .into_napi()
  }

  /// Playwright: `androidDevice.pinchClose(selector, percent, options?): Promise<void>`.
  #[napi(ts_args_type = "selector: import('./types').AndroidSelector, percent: number, options?: AndroidSpeedOptions")]
  pub async fn pinch_close(
    &self,
    selector: AndroidSelector,
    percent: f64,
    options: Option<AndroidSpeedOptions>,
  ) -> Result<()> {
    let options = options.unwrap_or_default();
    self
      .inner
      .pinch_close(&selector.0, percent, options.speed, limit(options.timeout))
      .await
      .into_napi()
  }

  /// Playwright: `androidDevice.pinchOpen(selector, percent, options?): Promise<void>`.
  #[napi(ts_args_type = "selector: import('./types').AndroidSelector, percent: number, options?: AndroidSpeedOptions")]
  pub async fn pinch_open(
    &self,
    selector: AndroidSelector,
    percent: f64,
    options: Option<AndroidSpeedOptions>,
  ) -> Result<()> {
    let options = options.unwrap_or_default();
    self
      .inner
      .pinch_open(&selector.0, percent, options.speed, limit(options.timeout))
      .await
      .into_napi()
  }

  /// Playwright: `androidDevice.scroll(selector, direction, percent,
  /// options?): Promise<void>`.
  #[napi(
    ts_args_type = "selector: import('./types').AndroidSelector, direction: 'down' | 'up' | 'left' | 'right', percent: number, options?: AndroidSpeedOptions"
  )]
  pub async fn scroll(
    &self,
    selector: AndroidSelector,
    direction: String,
    percent: f64,
    options: Option<AndroidSpeedOptions>,
  ) -> Result<()> {
    let options = options.unwrap_or_default();
    self
      .inner
      .scroll(
        &selector.0,
        self::direction(&direction)?,
        percent,
        options.speed,
        limit(options.timeout),
      )
      .await
      .into_napi()
  }

  /// Playwright: `androidDevice.swipe(selector, direction, percent,
  /// options?): Promise<void>`.
  #[napi(
    ts_args_type = "selector: import('./types').AndroidSelector, direction: 'down' | 'up' | 'left' | 'right', percent: number, options?: AndroidSpeedOptions"
  )]
  pub async fn swipe(
    &self,
    selector: AndroidSelector,
    direction: String,
    percent: f64,
    options: Option<AndroidSpeedOptions>,
  ) -> Result<()> {
    let options = options.unwrap_or_default();
    self
      .inner
      .swipe(
        &selector.0,
        self::direction(&direction)?,
        percent,
        options.speed,
        limit(options.timeout),
      )
      .await
      .into_napi()
  }

  /// Playwright: `androidDevice.info(selector): Promise<AndroidElementInfo>`.
  #[napi(ts_args_type = "selector: import('./types').AndroidSelector")]
  pub async fn info(&self, selector: AndroidSelector) -> Result<AndroidElementInfo> {
    let info = self.inner.info(&selector.0).await.into_napi()?;
    Ok(AndroidElementInfo {
      clazz: info.clazz,
      desc: info.desc,
      res: info.res,
      pkg: info.pkg,
      text: info.text,
      bounds: AndroidBounds {
        x: info.bounds.x,
        y: info.bounds.y,
        width: info.bounds.width,
        height: info.bounds.height,
      },
      checkable: info.checkable,
      checked: info.checked,
      clickable: info.clickable,
      enabled: info.enabled,
      focusable: info.focusable,
      focused: info.focused,
      long_clickable: info.long_clickable,
      scrollable: info.scrollable,
      selected: info.selected,
    })
  }

  /// Playwright: `androidDevice.launchBrowser(options?: BrowserContextOptions
  /// & { pkg?: string, args?: string[] }): Promise<BrowserContext>`. The
  /// browser `pkg`, `com.android.chrome` when absent, started afresh, and
  /// its context: the one the device has.
  #[napi(ts_args_type = "options?: Parameters<Browser['newContext']>[0] & { pkg?: string, args?: string[] }")]
  pub async fn launch_browser(
    &self,
    options: Option<AndroidLaunchBrowserOptions>,
  ) -> Result<crate::context::BrowserContext> {
    let (pkg, args, mut context) = match options {
      Some(options) => (options.pkg, options.args, options.context.into_core()),
      None => (None, Vec::new(), ferridriver::options::BrowserContextOptions::default()),
    };
    // A device has one context, so its proxy is the browser's.
    let proxy = context.proxy.take();
    let context = self
      .inner
      .launch_browser(ferridriver::android::AndroidLaunchBrowserOptions {
        pkg,
        args,
        proxy,
        context,
      })
      .await
      .into_napi()?;
    Ok(crate::context::BrowserContext::wrap(context))
  }

  /// Playwright: `androidDevice.close(): Promise<void>`.
  #[napi]
  pub async fn close(&self) {
    self.inner.close().await;
  }
}

/// Playwright `AndroidInput`, reached as `androidDevice.input`: raw input
/// that addresses the screen and not a view.
#[napi]
pub struct AndroidInput {
  inner: ferridriver::android::AndroidInput,
}

#[napi]
impl AndroidInput {
  /// Playwright: `androidInput.type(text: string): Promise<void>`.
  #[napi(js_name = "type")]
  pub async fn type_text(&self, text: String) -> Result<()> {
    self.inner.type_text(&text).await.into_napi()
  }

  /// Playwright: `androidInput.press(key: AndroidKey): Promise<void>`.
  #[napi(ts_args_type = "key: import('./types').AndroidKey")]
  pub async fn press(&self, key: String) -> Result<()> {
    self.inner.press(&key).await.into_napi()
  }

  /// Playwright: `androidInput.tap(point: { x, y }): Promise<void>`.
  #[napi]
  pub async fn tap(&self, point: crate::types::Point) -> Result<()> {
    self.inner.tap(point.into()).await.into_napi()
  }

  /// Playwright: `androidInput.swipe(from, segments, steps): Promise<void>`.
  #[napi]
  pub async fn swipe(&self, from: crate::types::Point, segments: Vec<crate::types::Point>, steps: u32) -> Result<()> {
    let segments: Vec<ferridriver::options::Point> = segments.into_iter().map(Into::into).collect();
    self.inner.swipe(from.into(), &segments, steps).await.into_napi()
  }

  /// Playwright: `androidInput.drag(from, to, steps): Promise<void>`.
  #[napi]
  pub async fn drag(&self, from: crate::types::Point, to: crate::types::Point, steps: u32) -> Result<()> {
    self.inner.drag(from.into(), to.into(), steps).await.into_napi()
  }
}

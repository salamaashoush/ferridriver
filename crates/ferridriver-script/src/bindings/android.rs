//! `_android` — QuickJS binding for [`ferridriver::android::Android`].
//!
//! Mirrors Playwright's `client/android.ts`: `_android.devices(options?)`
//! and, on an `AndroidDevice`, `serial()`, `model()`, `shell(command)`,
//! `screenshot(options?)`, `push(file, path, options?)`,
//! `installApk(file, options?)`, `launchBrowser(options?)` and `close()`,
//! the selector actions (`wait`, `fill`, `press`, `tap`, `longTap`,
//! `drag`, `fling`, `pinchClose`, `pinchOpen`, `scroll`, `swipe`, `info`)
//! and `input`, `open(command)` with its `AndroidSocket`, `webViews()`,
//! `webView(selector, options?)` with `AndroidWebView`, the `webview` and
//! `close` events, and `waitForEvent`.
//!
//! A device is authority the realm has to hold like any other. The ADB
//! server is a host and a port, judged by the realm's network grant; a
//! file named by path is read under its read grant, and a screenshot
//! written to a path under its write grant.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ferridriver::android::{
  Android, AndroidDevice, AndroidDeviceEvent, AndroidDevicesOptions, AndroidDirection, AndroidFile, AndroidInput,
  AndroidLaunchBrowserOptions, AndroidSelector, AndroidSocket, AndroidText, AndroidWebView, AndroidWebViewSelector,
};
use ferridriver::options::Point;
use rquickjs::class::Trace;
use rquickjs::function::Opt;
use rquickjs::promise::Promised;
use rquickjs::{Class, Ctx, Function, JsLifetime, Persistent, Value};

use crate::bindings::convert::{FerriResultCtxExt, ms_f64_to_u64, serde_from_js, serde_to_js, type_error};

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

/// Playwright's `string | RegExp` condition of a selector: a string is
/// the whole value, a `RegExp` its `source`.
fn text<'js>(ctx: &Ctx<'js>, selector: &rquickjs::Object<'js>, name: &str) -> rquickjs::Result<Option<AndroidText>> {
  let value: Value<'js> = selector.get(name)?;
  if value.is_undefined() || value.is_null() {
    return Ok(None);
  }
  if let Some(exact) = value.as_string() {
    return Ok(Some(AndroidText::Exact(exact.to_string()?)));
  }
  let source: Option<String> = value.as_object().map(|pattern| pattern.get("source")).transpose()?;
  source
    .map(AndroidText::Pattern)
    .map(Some)
    .ok_or_else(|| type_error(ctx, format!("selector.{name}: expected a string or a RegExp")))
}

/// Playwright: `AndroidSelector`.
fn selector<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> rquickjs::Result<AndroidSelector> {
  let bag = value
    .as_object()
    .ok_or_else(|| type_error(ctx, "selector: expected an object"))?;
  let nested = |name: &str| -> rquickjs::Result<Option<(Box<AndroidSelector>, Option<u32>)>> {
    let holder: Value<'js> = bag.get(name)?;
    let Some(holder) = holder.as_object() else {
      return Ok(None);
    };
    let inner: Value<'js> = holder.get("selector")?;
    Ok(Some((Box::new(selector(ctx, &inner)?), holder.get("maxDepth")?)))
  };
  Ok(AndroidSelector {
    checkable: bag.get("checkable")?,
    checked: bag.get("checked")?,
    clazz: text(ctx, bag, "clazz")?,
    clickable: bag.get("clickable")?,
    depth: bag.get("depth")?,
    desc: text(ctx, bag, "desc")?,
    enabled: bag.get("enabled")?,
    focusable: bag.get("focusable")?,
    focused: bag.get("focused")?,
    has_child: nested("hasChild")?.map(|(child, _)| child),
    has_descendant: nested("hasDescendant")?,
    long_clickable: bag.get("longClickable")?,
    pkg: text(ctx, bag, "pkg")?,
    res: text(ctx, bag, "res")?,
    scrollable: bag.get("scrollable")?,
    selected: bag.get("selected")?,
    text: text(ctx, bag, "text")?,
  })
}

/// The options every selector action takes some of: `timeout`, and for
/// the gestures `speed`, for `tap` `duration`, for `wait` `state`.
#[derive(serde::Deserialize, Default)]
struct ActionOptions {
  timeout: Option<f64>,
  speed: Option<f64>,
  duration: Option<f64>,
  state: Option<String>,
}

impl ActionOptions {
  fn timeout(&self) -> Option<u64> {
    self.timeout.map(ms_f64_to_u64)
  }
}

fn point<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<Point> {
  serde_from_js(ctx, value)
}
#[rquickjs::methods]
impl AndroidJs {
  /// Playwright: `android.setDefaultTimeout(timeout: number): void`.
  #[qjs(rename = "setDefaultTimeout")]
  pub fn set_default_timeout(&self, timeout: f64) {
    self.inner.set_default_timeout(ms_f64_to_u64(timeout));
  }

  /// Playwright: `android.connect(endpoint, options?): Promise<AndroidDevice>`.
  /// Refused: the endpoint is a Playwright server.
  #[qjs(rename = "connect")]
  pub fn connect<'js>(
    &self,
    ctx: Ctx<'js>,
    endpoint: String,
    _options: Opt<Value<'js>>,
  ) -> Promised<impl std::future::Future<Output = rquickjs::Result<()>> + 'js> {
    // The error is made when the promise is settled: one thrown now
    // would be gone from the context by then.
    Promised(async move { Android::connect(&endpoint).map(|_| ()).into_js_with(&ctx) })
  }

  /// Playwright: `android.launchServer(options?): Promise<BrowserServer>`.
  /// Refused, for the same reason.
  #[qjs(rename = "launchServer")]
  pub fn launch_server<'js>(
    &self,
    ctx: Ctx<'js>,
    _options: Opt<Value<'js>>,
  ) -> Promised<impl std::future::Future<Output = rquickjs::Result<()>> + 'js> {
    Promised(async move { Android::launch_server().into_js_with(&ctx) })
  }

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

/// The listeners a script put on one object. They are called on the
/// interpreter's own thread, by a pump that reads the object's events.
#[derive(Default)]
struct Listeners {
  entries: RefCell<Vec<(String, Persistent<Function<'static>>, bool)>>,
}

impl Listeners {
  fn add<'js>(&self, ctx: &Ctx<'js>, event: String, listener: Function<'js>, once: bool) {
    self
      .entries
      .borrow_mut()
      .push((event, Persistent::save(ctx, listener), once));
  }

  fn remove<'js>(&self, ctx: &Ctx<'js>, event: &str, listener: &Function<'js>) -> rquickjs::Result<()> {
    let mut kept = Vec::new();
    for (name, saved, once) in self.entries.take() {
      let same = name == event && saved.clone().restore(ctx)?.as_value() == listener.as_value();
      if !same {
        kept.push((name, saved, once));
      }
    }
    // Anything registered while the list was out is kept as well.
    kept.append(&mut self.entries.borrow_mut());
    *self.entries.borrow_mut() = kept;
    Ok(())
  }

  /// Call every listener of `event` with `argument`, in the order they
  /// were added. One that throws does not stop the rest.
  async fn emit<'js>(&self, ctx: &Ctx<'js>, event: &str, argument: Value<'js>) {
    let due: Vec<Persistent<Function<'static>>> = {
      let mut entries = self.entries.borrow_mut();
      let due = entries
        .iter()
        .filter(|(name, _, _)| name == event)
        .map(|(_, saved, _)| saved.clone())
        .collect();
      entries.retain(|(name, _, once)| !(name == event && *once));
      due
    };
    for saved in due {
      let Ok(listener) = saved.restore(ctx) else { continue };
      let returned: rquickjs::Result<Value<'js>> = listener.call((argument.clone(),));
      if let Ok(value) = returned
        && let Some(promise) = value.as_promise()
      {
        let _ = promise.clone().into_future::<Value<'js>>().await;
      }
    }
  }
}

/// One JS object per web view of a device, as Playwright's
/// `AndroidDevice._webViews` gives.
type ViewObjects = Rc<RefCell<Vec<(AndroidWebView, Persistent<Class<'static, AndroidWebViewJs>>)>>>;

fn view_object<'js>(
  ctx: &Ctx<'js>,
  views: &ViewObjects,
  view: AndroidWebView,
) -> rquickjs::Result<Class<'js, AndroidWebViewJs>> {
  let known = views
    .borrow()
    .iter()
    .find(|(known, _)| *known == view)
    .map(|(_, object)| object.clone());
  if let Some(object) = known {
    return object.restore(ctx);
  }
  let object = Class::instance(
    ctx.clone(),
    AndroidWebViewJs {
      inner: view.clone(),
      listeners: Rc::default(),
      pumping: Cell::new(false),
    },
  )?;
  let mut views = views.borrow_mut();
  views.retain(|(known, _)| !known.is_closed());
  views.push((view, Persistent::save(ctx, object.clone())));
  Ok(object)
}

/// JS truthiness of what a predicate returned.
fn truthy<'js>(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<bool> {
  let boolean: Function<'js> = ctx.globals().get("Boolean")?;
  boolean.call((value,))
}

#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "AndroidDevice")]
pub struct AndroidDeviceJs {
  #[qjs(skip_trace)]
  inner: AndroidDevice,
  #[qjs(skip_trace)]
  listeners: Rc<Listeners>,
  #[qjs(skip_trace)]
  views: ViewObjects,
  #[qjs(skip_trace)]
  pumping: Cell<bool>,
}

impl AndroidDeviceJs {
  #[must_use]
  pub fn new(inner: AndroidDevice) -> Self {
    Self {
      inner,
      listeners: Rc::default(),
      views: Rc::default(),
      pumping: Cell::new(false),
    }
  }

  /// Start delivering the device's events to its listeners, once.
  fn pump(&self, ctx: &Ctx<'_>) {
    if self.pumping.replace(true) {
      return;
    }
    let mut events = self.inner.subscribe();
    let device = self.inner.clone();
    let listeners = Rc::clone(&self.listeners);
    let views = Rc::clone(&self.views);
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
      loop {
        match events.recv().await {
          Ok(AndroidDeviceEvent::WebView(view)) => {
            if let Ok(object) = view_object(&ctx, &views, view) {
              listeners.emit(&ctx, "webview", object.into_value()).await;
            }
          },
          Ok(AndroidDeviceEvent::Close) => {
            let this = crate::bindings::identity::android_device_object(&ctx, device.clone())
              .map_or_else(|_| Value::new_undefined(ctx.clone()), Class::into_value);
            listeners.emit(&ctx, "close", this).await;
            return;
          },
          Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
          Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
      }
    });
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

  /// Playwright: `androidDevice.on(event: 'webview' | 'close', listener)`.
  #[qjs(rename = "on")]
  pub fn on<'js>(&self, ctx: Ctx<'js>, event: String, listener: Function<'js>) {
    self.listeners.add(&ctx, event, listener, false);
    self.pump(&ctx);
  }

  /// Playwright: `androidDevice.once(event, listener)`.
  #[qjs(rename = "once")]
  pub fn once<'js>(&self, ctx: Ctx<'js>, event: String, listener: Function<'js>) {
    self.listeners.add(&ctx, event, listener, true);
    self.pump(&ctx);
  }

  /// Playwright: `androidDevice.off(event, listener)`.
  #[qjs(rename = "off")]
  pub fn off<'js>(&self, ctx: Ctx<'js>, event: String, listener: Function<'js>) -> rquickjs::Result<()> {
    self.listeners.remove(&ctx, &event, &listener)
  }

  /// Playwright: `androidDevice.waitForEvent(event, optionsOrPredicate?)`.
  /// `webview` resolves to the `AndroidWebView`, `close` to the device.
  #[qjs(rename = "waitForEvent")]
  pub async fn wait_for_event<'js>(
    &self,
    ctx: Ctx<'js>,
    event: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Value<'js>> {
    if event != "webview" && event != "close" {
      return Err(type_error(&ctx, format!("waitForEvent: unknown event \"{event}\"")));
    }
    let (predicate, timeout): (Option<Function<'js>>, Option<f64>) = match options.0 {
      Some(value) if value.is_function() => (value.into_function(), None),
      Some(value) if value.is_object() => {
        let bag = value.as_object().cloned();
        let predicate: Option<Function<'js>> = bag.as_ref().map(|bag| bag.get("predicate")).transpose()?.flatten();
        let timeout: Option<f64> = bag.as_ref().map(|bag| bag.get("timeout")).transpose()?.flatten();
        (predicate, timeout)
      },
      _ => (None, None),
    };
    let timeout_ms = timeout.map_or_else(|| self.inner.default_timeout(), ms_f64_to_u64);
    // Subscribed before anything is awaited, so no event in between is missed.
    let mut events = self.inner.subscribe();
    let accepted = |candidate: &Value<'js>| -> rquickjs::Result<bool> {
      match &predicate {
        Some(predicate) => truthy(&ctx, predicate.call((candidate.clone(),))?),
        None => Ok(true),
      }
    };
    let closed = || {
      crate::bindings::convert::ferri_throw(
        &ctx,
        &ferridriver::FerriError::target_closed(Some("Device is closed".into())),
      )
    };
    // A closed device shows no more web views; say so now, not at the timeout.
    if event == "webview" && self.inner.is_closed() {
      return Err(closed());
    }
    let happened = async {
      loop {
        match events.recv().await {
          Ok(AndroidDeviceEvent::WebView(view)) if event == "webview" => {
            let object = view_object(&ctx, &self.views, view)?.into_value();
            if accepted(&object)? {
              return Ok(object);
            }
          },
          Ok(AndroidDeviceEvent::WebView(_)) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
          Ok(AndroidDeviceEvent::Close) if event == "close" => {
            let this = crate::bindings::identity::android_device_object(&ctx, self.inner.clone())?.into_value();
            if accepted(&this)? {
              return Ok(this);
            }
          },
          Ok(AndroidDeviceEvent::Close) | Err(tokio::sync::broadcast::error::RecvError::Closed) => {
            return Err(closed());
          },
        }
      }
    };
    if timeout_ms == 0 {
      return happened.await;
    }
    match tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), happened).await {
      Ok(outcome) => outcome,
      Err(_) => Err(crate::bindings::convert::ferri_throw(
        &ctx,
        &ferridriver::FerriError::timeout(format!("waiting for event \"{event}\""), timeout_ms),
      )),
    }
  }

  /// Playwright: `androidDevice.webViews(): AndroidWebView[]`.
  #[qjs(rename = "webViews")]
  pub fn web_views<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Vec<Class<'js, AndroidWebViewJs>>> {
    self
      .inner
      .web_views()
      .into_iter()
      .map(|view| view_object(&ctx, &self.views, view))
      .collect()
  }

  /// Playwright: `androidDevice.webView(selector: { pkg?, socketName? },
  /// options?: { timeout? }): Promise<AndroidWebView>`.
  #[qjs(rename = "webView")]
  pub async fn web_view<'js>(
    &self,
    ctx: Ctx<'js>,
    target: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<Class<'js, AndroidWebViewJs>> {
    #[derive(serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    struct Target {
      pkg: Option<String>,
      socket_name: Option<String>,
    }
    let target: Target = serde_from_js(&ctx, target)?;
    let options: ActionOptions = bag(&ctx, options)?;
    let view = self
      .inner
      .web_view(
        &AndroidWebViewSelector {
          pkg: target.pkg,
          socket_name: target.socket_name,
        },
        options.timeout(),
      )
      .await
      .into_js_with(&ctx)?;
    view_object(&ctx, &self.views, view)
  }

  /// Playwright: `androidDevice.open(command: string): Promise<AndroidSocket>`.
  #[qjs(rename = "open")]
  pub async fn open<'js>(&self, ctx: Ctx<'js>, command: String) -> rquickjs::Result<Class<'js, AndroidSocketJs>> {
    let socket = self.inner.open(&command).await.into_js_with(&ctx)?;
    Class::instance(
      ctx,
      AndroidSocketJs {
        inner: Rc::new(socket),
        listeners: Rc::default(),
        pumping: Cell::new(false),
      },
    )
  }

  /// Playwright: `androidDevice.setDefaultTimeout(timeout: number): void`.
  #[qjs(rename = "setDefaultTimeout")]
  pub fn set_default_timeout(&self, timeout: f64) {
    self.inner.set_default_timeout(ms_f64_to_u64(timeout));
  }

  /// Playwright: `androidDevice.input: AndroidInput`.
  #[qjs(get, rename = "input")]
  pub fn input<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, AndroidInputJs>> {
    Class::instance(
      ctx,
      AndroidInputJs {
        inner: self.inner.input(),
      },
    )
  }

  /// Playwright: `androidDevice.wait(selector, options?: { state?: 'gone',
  /// timeout? }): Promise<void>`.
  #[qjs(rename = "wait")]
  pub async fn wait<'js>(&self, ctx: Ctx<'js>, target: Value<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<()> {
    let target = selector(&ctx, &target)?;
    let options: ActionOptions = bag(&ctx, options)?;
    let gone = match options.state.as_deref() {
      None => false,
      Some("gone") => true,
      Some(other) => return Err(type_error(&ctx, format!("state: expected 'gone', got '{other}'"))),
    };
    self
      .inner
      .wait(&target, gone, options.timeout())
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `androidDevice.fill(selector, text, options?): Promise<void>`.
  #[qjs(rename = "fill")]
  pub async fn fill<'js>(
    &self,
    ctx: Ctx<'js>,
    target: Value<'js>,
    text: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let target = selector(&ctx, &target)?;
    let options: ActionOptions = bag(&ctx, options)?;
    self
      .inner
      .fill(&target, &text, options.timeout())
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `androidDevice.press(selector, key, options?): Promise<void>`.
  #[qjs(rename = "press")]
  pub async fn press<'js>(
    &self,
    ctx: Ctx<'js>,
    target: Value<'js>,
    key: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let target = selector(&ctx, &target)?;
    let options: ActionOptions = bag(&ctx, options)?;
    self
      .inner
      .press(&target, &key, options.timeout())
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `androidDevice.tap(selector, options?: { duration?,
  /// timeout? }): Promise<void>`.
  #[qjs(rename = "tap")]
  pub async fn tap<'js>(&self, ctx: Ctx<'js>, target: Value<'js>, options: Opt<Value<'js>>) -> rquickjs::Result<()> {
    let target = selector(&ctx, &target)?;
    let options: ActionOptions = bag(&ctx, options)?;
    self
      .inner
      .tap(&target, options.duration, options.timeout())
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `androidDevice.longTap(selector, options?): Promise<void>`.
  #[qjs(rename = "longTap")]
  pub async fn long_tap<'js>(
    &self,
    ctx: Ctx<'js>,
    target: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let target = selector(&ctx, &target)?;
    let options: ActionOptions = bag(&ctx, options)?;
    self.inner.long_tap(&target, options.timeout()).await.into_js_with(&ctx)
  }

  /// Playwright: `androidDevice.drag(selector, dest, options?: { speed?,
  /// timeout? }): Promise<void>`.
  #[qjs(rename = "drag")]
  pub async fn drag<'js>(
    &self,
    ctx: Ctx<'js>,
    target: Value<'js>,
    dest: Value<'js>,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let target = selector(&ctx, &target)?;
    let dest = point(&ctx, dest)?;
    let options: ActionOptions = bag(&ctx, options)?;
    self
      .inner
      .drag(&target, dest, options.speed, options.timeout())
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `androidDevice.fling(selector, direction, options?): Promise<void>`.
  #[qjs(rename = "fling")]
  pub async fn fling<'js>(
    &self,
    ctx: Ctx<'js>,
    target: Value<'js>,
    direction: String,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let target = selector(&ctx, &target)?;
    let direction = AndroidDirection::parse(&direction).into_js_with(&ctx)?;
    let options: ActionOptions = bag(&ctx, options)?;
    self
      .inner
      .fling(&target, direction, options.speed, options.timeout())
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `androidDevice.pinchClose(selector, percent, options?): Promise<void>`.
  #[qjs(rename = "pinchClose")]
  pub async fn pinch_close<'js>(
    &self,
    ctx: Ctx<'js>,
    target: Value<'js>,
    percent: f64,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let target = selector(&ctx, &target)?;
    let options: ActionOptions = bag(&ctx, options)?;
    self
      .inner
      .pinch_close(&target, percent, options.speed, options.timeout())
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `androidDevice.pinchOpen(selector, percent, options?): Promise<void>`.
  #[qjs(rename = "pinchOpen")]
  pub async fn pinch_open<'js>(
    &self,
    ctx: Ctx<'js>,
    target: Value<'js>,
    percent: f64,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let target = selector(&ctx, &target)?;
    let options: ActionOptions = bag(&ctx, options)?;
    self
      .inner
      .pinch_open(&target, percent, options.speed, options.timeout())
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `androidDevice.scroll(selector, direction, percent,
  /// options?): Promise<void>`.
  #[qjs(rename = "scroll")]
  pub async fn scroll<'js>(
    &self,
    ctx: Ctx<'js>,
    target: Value<'js>,
    direction: String,
    percent: f64,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let target = selector(&ctx, &target)?;
    let direction = AndroidDirection::parse(&direction).into_js_with(&ctx)?;
    let options: ActionOptions = bag(&ctx, options)?;
    self
      .inner
      .scroll(&target, direction, percent, options.speed, options.timeout())
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `androidDevice.swipe(selector, direction, percent,
  /// options?): Promise<void>`.
  #[qjs(rename = "swipe")]
  pub async fn swipe<'js>(
    &self,
    ctx: Ctx<'js>,
    target: Value<'js>,
    direction: String,
    percent: f64,
    options: Opt<Value<'js>>,
  ) -> rquickjs::Result<()> {
    let target = selector(&ctx, &target)?;
    let direction = AndroidDirection::parse(&direction).into_js_with(&ctx)?;
    let options: ActionOptions = bag(&ctx, options)?;
    self
      .inner
      .swipe(&target, direction, percent, options.speed, options.timeout())
      .await
      .into_js_with(&ctx)
  }

  /// Playwright: `androidDevice.info(selector): Promise<AndroidElementInfo>`.
  #[qjs(rename = "info")]
  pub async fn info<'js>(&self, ctx: Ctx<'js>, target: Value<'js>) -> rquickjs::Result<Value<'js>> {
    let target = selector(&ctx, &target)?;
    let info = self.inner.info(&target).await.into_js_with(&ctx)?;
    serde_to_js(&ctx, &info)
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
      Some(value) if !value.is_undefined() && !value.is_null() => (serde_from_js::<Launch>(&ctx, value.clone())?, {
        let options = serde_from_js::<crate::bindings::browser::JsBrowserContextOptions>(&ctx, value)?.into_core();
        crate::bindings::convert::check_context_paths(&ctx, &options)?;
        options
      }),
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

/// Playwright: `AndroidWebView`.
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "AndroidWebView")]
pub struct AndroidWebViewJs {
  #[qjs(skip_trace)]
  inner: AndroidWebView,
  #[qjs(skip_trace)]
  listeners: Rc<Listeners>,
  #[qjs(skip_trace)]
  pumping: Cell<bool>,
}

impl AndroidWebViewJs {
  fn listen<'js>(&self, ctx: &Ctx<'js>, event: String, listener: Function<'js>, once: bool) {
    self.listeners.add(ctx, event, listener, once);
    if self.pumping.replace(true) {
      return;
    }
    let view = self.inner.clone();
    let listeners = Rc::clone(&self.listeners);
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
      view.closed().await;
      listeners.emit(&ctx, "close", Value::new_undefined(ctx.clone())).await;
    });
  }
}

#[rquickjs::methods]
impl AndroidWebViewJs {
  /// Playwright: `androidWebView.pid(): number`.
  #[qjs(rename = "pid")]
  pub fn pid(&self) -> i64 {
    self.inner.pid()
  }

  /// Playwright: `androidWebView.pkg(): string`.
  #[qjs(rename = "pkg")]
  pub fn pkg(&self) -> String {
    self.inner.pkg().to_owned()
  }

  /// Playwright: `androidWebView.page(): Promise<Page>`.
  #[qjs(rename = "page")]
  pub async fn page<'js>(&self, ctx: Ctx<'js>) -> rquickjs::Result<Class<'js, crate::bindings::page::PageJs>> {
    let page = self.inner.page().await.into_js_with(&ctx)?;
    crate::bindings::identity::page_object(&ctx, page)
  }

  /// Playwright: `androidWebView.on('close', listener)`.
  #[qjs(rename = "on")]
  pub fn on<'js>(&self, ctx: Ctx<'js>, event: String, listener: Function<'js>) {
    self.listen(&ctx, event, listener, false);
  }

  /// Playwright: `androidWebView.once('close', listener)`.
  #[qjs(rename = "once")]
  pub fn once<'js>(&self, ctx: Ctx<'js>, event: String, listener: Function<'js>) {
    self.listen(&ctx, event, listener, true);
  }

  /// Playwright: `androidWebView.off('close', listener)`.
  #[qjs(rename = "off")]
  pub fn off<'js>(&self, ctx: Ctx<'js>, event: String, listener: Function<'js>) -> rquickjs::Result<()> {
    self.listeners.remove(&ctx, &event, &listener)
  }
}

/// Playwright: `AndroidSocket`, a connection `androidDevice.open` made.
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "AndroidSocket")]
pub struct AndroidSocketJs {
  #[qjs(skip_trace)]
  inner: Rc<AndroidSocket>,
  #[qjs(skip_trace)]
  listeners: Rc<Listeners>,
  #[qjs(skip_trace)]
  pumping: Cell<bool>,
}

impl AndroidSocketJs {
  /// What the device sent is held until the first listener asks, then
  /// delivered in order: `data` a chunk at a time, `close` at the end.
  fn listen<'js>(&self, ctx: &Ctx<'js>, event: String, listener: Function<'js>, once: bool) {
    self.listeners.add(ctx, event, listener, once);
    if self.pumping.replace(true) {
      return;
    }
    let socket = Rc::clone(&self.inner);
    let listeners = Rc::clone(&self.listeners);
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
      while let Some(chunk) = socket.next_data().await {
        if let Ok(data) = rquickjs::IntoJs::into_js(chunk, &ctx) {
          listeners.emit(&ctx, "data", data).await;
        }
      }
      listeners.emit(&ctx, "close", Value::new_undefined(ctx.clone())).await;
    });
  }
}

#[rquickjs::methods]
impl AndroidSocketJs {
  /// Playwright: `androidSocket.write(data: Buffer): Promise<void>`.
  #[qjs(rename = "write")]
  pub async fn write<'js>(&self, ctx: Ctx<'js>, data: Value<'js>) -> rquickjs::Result<()> {
    let data = ferrijs::std::node::bytes::value_to_bytes(&ctx, &data, None)?;
    self.inner.write(&data).await.into_js_with(&ctx)
  }

  /// Playwright: `androidSocket.close(): Promise<void>`.
  #[qjs(rename = "close")]
  pub async fn close(&self) {
    self.inner.close().await;
  }

  /// Playwright: `androidSocket.on(event: 'data' | 'close', listener)`.
  #[qjs(rename = "on")]
  pub fn on<'js>(&self, ctx: Ctx<'js>, event: String, listener: Function<'js>) {
    self.listen(&ctx, event, listener, false);
  }

  /// Playwright: `androidSocket.once(event, listener)`.
  #[qjs(rename = "once")]
  pub fn once<'js>(&self, ctx: Ctx<'js>, event: String, listener: Function<'js>) {
    self.listen(&ctx, event, listener, true);
  }

  /// Playwright: `androidSocket.off(event, listener)`.
  #[qjs(rename = "off")]
  pub fn off<'js>(&self, ctx: Ctx<'js>, event: String, listener: Function<'js>) -> rquickjs::Result<()> {
    self.listeners.remove(&ctx, &event, &listener)
  }
}

/// Playwright: `AndroidInput`, reached as `androidDevice.input`.
#[derive(JsLifetime, Trace)]
#[rquickjs::class(rename = "AndroidInput")]
pub struct AndroidInputJs {
  #[qjs(skip_trace)]
  inner: AndroidInput,
}

#[rquickjs::methods]
impl AndroidInputJs {
  /// Playwright: `androidInput.type(text: string): Promise<void>`.
  #[qjs(rename = "type")]
  pub async fn type_text(&self, ctx: Ctx<'_>, text: String) -> rquickjs::Result<()> {
    self.inner.type_text(&text).await.into_js_with(&ctx)
  }

  /// Playwright: `androidInput.press(key: AndroidKey): Promise<void>`.
  #[qjs(rename = "press")]
  pub async fn press(&self, ctx: Ctx<'_>, key: String) -> rquickjs::Result<()> {
    self.inner.press(&key).await.into_js_with(&ctx)
  }

  /// Playwright: `androidInput.tap(point: { x, y }): Promise<void>`.
  #[qjs(rename = "tap")]
  pub async fn tap<'js>(&self, ctx: Ctx<'js>, at: Value<'js>) -> rquickjs::Result<()> {
    let at = point(&ctx, at)?;
    self.inner.tap(at).await.into_js_with(&ctx)
  }

  /// Playwright: `androidInput.swipe(from, segments, steps): Promise<void>`.
  #[qjs(rename = "swipe")]
  pub async fn swipe<'js>(
    &self,
    ctx: Ctx<'js>,
    from: Value<'js>,
    segments: Value<'js>,
    steps: u32,
  ) -> rquickjs::Result<()> {
    let from = point(&ctx, from)?;
    let segments: Vec<Point> = serde_from_js(&ctx, segments)?;
    self.inner.swipe(from, &segments, steps).await.into_js_with(&ctx)
  }

  /// Playwright: `androidInput.drag(from, to, steps): Promise<void>`.
  #[qjs(rename = "drag")]
  pub async fn drag<'js>(&self, ctx: Ctx<'js>, from: Value<'js>, to: Value<'js>, steps: u32) -> rquickjs::Result<()> {
    let from = point(&ctx, from)?;
    let to = point(&ctx, to)?;
    self.inner.drag(from, to, steps).await.into_js_with(&ctx)
  }
}

/// Install the `_android` global, Playwright's `playwright._android`.
pub fn install_android(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
  Class::<AndroidJs>::define(&ctx.globals())?;
  Class::<AndroidDeviceJs>::define(&ctx.globals())?;
  Class::<AndroidInputJs>::define(&ctx.globals())?;
  Class::<AndroidWebViewJs>::define(&ctx.globals())?;
  Class::<AndroidSocketJs>::define(&ctx.globals())?;
  ctx.globals().set(
    "_android",
    Class::instance(ctx.clone(), AndroidJs { inner: Android::new() })?,
  )?;
  crate::bindings::runtime::mirror_global(ctx, "_android")
}

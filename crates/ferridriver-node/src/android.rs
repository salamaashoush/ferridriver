//! NAPI binding for [`ferridriver::android`].
//!
//! Mirrors Playwright's `client/android.ts`: the module's `_android`
//! object with `devices(options?)`, and on an `AndroidDevice` `serial()`,
//! `model()`, `shell(command)`, `screenshot(options?)`,
//! `push(file, path, options?)`, `installApk(file, options?)`,
//! `launchBrowser(options?)` and `close()`, the selector actions (`wait`,
//! `fill`, `press`, `tap`, `longTap`, `drag`, `fling`, `pinchClose`,
//! `pinchOpen`, `scroll`, `swipe`, `info`) and `input`, `open(command)`
//! with its `AndroidSocket`, `webViews()`, `webView(selector, options?)`
//! with `AndroidWebView`, the `webview` and `close` events, and
//! `waitForEvent`.

use napi::Result;
use napi::bindgen_prelude::{
  AsyncBlock, AsyncBlockBuilder, Buffer, Either, FromNapiValue, Object, TypeName, ValidateNapiValue,
};
use napi_derive::napi;

use crate::error::IntoNapi;

mod namespace;

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

type Tsfn<A> = napi::threadsafe_function::ThreadsafeFunction<A, (), A, napi::Status, false, true, 0>;

struct Listener<A: napi::bindgen_prelude::JsValuesTupleIntoVec + 'static> {
  id: u64,
  event: String,
  once: bool,
  call: Tsfn<A>,
}

/// The listeners JS put on one object, called from the task that reads
/// the object's events. A listener is matched for `off` by the function
/// itself, which only the JS thread can compare.
/// A listener's id and event beside the function JS passed, kept to
/// match it again in `off`.
type Functions<A> = Vec<(u64, String, napi::bindgen_prelude::FunctionRef<A, ()>)>;

struct Emitter<A: napi::bindgen_prelude::JsValuesTupleIntoVec + 'static> {
  listeners: std::sync::Arc<std::sync::Mutex<Vec<Listener<A>>>>,
  functions: std::sync::Mutex<Functions<A>>,
  next: std::sync::atomic::AtomicU64,
  pumping: std::sync::atomic::AtomicBool,
}

impl<A> Emitter<A>
where
  A: napi::bindgen_prelude::JsValuesTupleIntoVec
    + napi::bindgen_prelude::ToNapiValue
    + napi::bindgen_prelude::TypeName
    + Clone
    + Send
    + 'static,
{
  fn new() -> Self {
    Self {
      listeners: std::sync::Arc::default(),
      functions: std::sync::Mutex::new(Vec::new()),
      next: std::sync::atomic::AtomicU64::new(0),
      pumping: std::sync::atomic::AtomicBool::new(false),
    }
  }

  /// Register `listener`, and say whether this is the first one: the
  /// moment to start reading events.
  fn add(&self, event: String, listener: &napi::bindgen_prelude::Function<'static, A, ()>, once: bool) -> Result<bool> {
    let id = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let function = listener.create_ref()?;
    let call: Tsfn<A> = listener
      .build_threadsafe_function()
      .callee_handled::<false>()
      .weak::<true>()
      .max_queue_size::<0>()
      .build()?;
    self
      .functions
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push((id, event.clone(), function));
    self
      .listeners
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .push(Listener { id, event, once, call });
    Ok(!self.pumping.swap(true, std::sync::atomic::Ordering::SeqCst))
  }

  fn remove(
    &self,
    env: napi::Env,
    event: &str,
    listener: &napi::bindgen_prelude::Function<'static, A, ()>,
  ) -> Result<()> {
    let incoming = listener.create_ref()?;
    let mut functions = self.functions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut removed = Vec::new();
    let mut index = 0;
    while index < functions.len() {
      let same = functions[index].1 == event
        && env.strict_equals(incoming.borrow_back(&env)?, functions[index].2.borrow_back(&env)?)?;
      if same {
        removed.push(functions.remove(index).0);
      } else {
        index += 1;
      }
    }
    self
      .listeners
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .retain(|listener| !removed.contains(&listener.id));
    Ok(())
  }

  /// Call every listener of `event`, in the order they were added.
  fn emit(listeners: &std::sync::Mutex<Vec<Listener<A>>>, event: &str, argument: &A) {
    let mut listeners = listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for listener in listeners.iter().filter(|listener| listener.event == event) {
      listener.call.call(
        argument.clone(),
        napi::threadsafe_function::ThreadsafeFunctionCallMode::NonBlocking,
      );
    }
    listeners.retain(|listener| !(listener.event == event && listener.once));
  }
}

/// What a device's listener is called with: the web view that appeared,
/// or the device that closed.
#[derive(Clone)]
pub enum AndroidDeviceEventArg {
  WebView(crate::identity::AndroidWebViewObject),
  Device(crate::identity::AndroidDeviceObject),
}

impl TypeName for AndroidDeviceEventArg {
  fn type_name() -> &'static str {
    "AndroidWebView | AndroidDevice"
  }

  fn value_type() -> napi::ValueType {
    napi::ValueType::Object
  }
}

impl napi::bindgen_prelude::ToNapiValue for AndroidDeviceEventArg {
  unsafe fn to_napi_value(env: napi::sys::napi_env, value: Self) -> Result<napi::sys::napi_value> {
    match value {
      Self::WebView(view) => unsafe { napi::bindgen_prelude::ToNapiValue::to_napi_value(env, view) },
      Self::Device(device) => unsafe { napi::bindgen_prelude::ToNapiValue::to_napi_value(env, device) },
    }
  }
}

/// What a socket's listener is called with: the bytes that arrived, or
/// nothing when it closed.
#[derive(Clone)]
pub enum AndroidSocketEventArg {
  Data(Vec<u8>),
  Close,
}

impl TypeName for AndroidSocketEventArg {
  fn type_name() -> &'static str {
    "Buffer | undefined"
  }

  fn value_type() -> napi::ValueType {
    napi::ValueType::Unknown
  }
}

impl napi::bindgen_prelude::ToNapiValue for AndroidSocketEventArg {
  unsafe fn to_napi_value(env: napi::sys::napi_env, value: Self) -> Result<napi::sys::napi_value> {
    match value {
      Self::Data(bytes) => unsafe { napi::bindgen_prelude::ToNapiValue::to_napi_value(env, Buffer::from(bytes)) },
      Self::Close => unsafe { napi::bindgen_prelude::ToNapiValue::to_napi_value(env, ()) },
    }
  }
}

/// Playwright: `androidDevice.webView(selector)`.
#[napi(object, object_to_js = false)]
#[derive(Debug, Clone, Default)]
pub struct AndroidWebViewSelector {
  /// The package the web view runs in.
  pub pkg: Option<String>,
  /// The name of its DevTools socket.
  pub socket_name: Option<String>,
}

fn file(source: Either<String, Buffer>) -> ferridriver::android::AndroidFile {
  match source {
    Either::A(path) => ferridriver::android::AndroidFile::Path(path.into()),
    Either::B(bytes) => ferridriver::android::AndroidFile::Bytes(bytes.to_vec()),
  }
}

/// Playwright `AndroidDevice`: a device behind an ADB server.
#[napi]
pub struct AndroidDevice {
  inner: ferridriver::android::AndroidDevice,
  events: Emitter<AndroidDeviceEventArg>,
}

impl AndroidDevice {
  pub(crate) fn from_core(inner: ferridriver::android::AndroidDevice) -> Self {
    Self {
      inner,
      events: Emitter::new(),
    }
  }

  fn listen(
    &self,
    event: String,
    listener: &napi::bindgen_prelude::Function<'static, AndroidDeviceEventArg, ()>,
    once: bool,
  ) -> Result<()> {
    if !self.events.add(event, listener, once)? {
      return Ok(());
    }
    let mut events = self.inner.subscribe();
    let device = self.inner.clone();
    let listeners = std::sync::Arc::clone(&self.events.listeners);
    napi::bindgen_prelude::spawn(async move {
      loop {
        match events.recv().await {
          Ok(ferridriver::android::AndroidDeviceEvent::WebView(view)) => Emitter::emit(
            &listeners,
            "webview",
            &AndroidDeviceEventArg::WebView(crate::identity::AndroidWebViewObject(view)),
          ),
          Ok(ferridriver::android::AndroidDeviceEvent::Close) => {
            Emitter::emit(
              &listeners,
              "close",
              &AndroidDeviceEventArg::Device(crate::identity::AndroidDeviceObject(device.clone())),
            );
            return;
          },
          Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {},
          Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
      }
    });
    Ok(())
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

  /// Playwright: `androidDevice.on(event: 'webview' | 'close', listener)`.
  #[napi(ts_args_type = "event: 'webview' | 'close', listener: (arg: AndroidWebView | AndroidDevice) => void")]
  pub fn on(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'static, AndroidDeviceEventArg, ()>,
  ) -> Result<()> {
    self.listen(event, &listener, false)
  }

  /// Playwright: `androidDevice.once(event, listener)`.
  #[napi(ts_args_type = "event: 'webview' | 'close', listener: (arg: AndroidWebView | AndroidDevice) => void")]
  pub fn once(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'static, AndroidDeviceEventArg, ()>,
  ) -> Result<()> {
    self.listen(event, &listener, true)
  }

  /// Playwright: `androidDevice.off(event, listener)`.
  #[napi(ts_args_type = "event: 'webview' | 'close', listener: (arg: AndroidWebView | AndroidDevice) => void")]
  // napi-rs injects Env only as `&Env` (no by-value FromNapiValue).
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn off(
    &self,
    env: &napi::Env,
    event: String,
    listener: napi::bindgen_prelude::Function<'static, AndroidDeviceEventArg, ()>,
  ) -> Result<()> {
    self.events.remove(*env, &event, &listener)
  }

  /// Playwright: `androidDevice.waitForEvent(event, optionsOrPredicate?)`.
  /// `webview` resolves to the `AndroidWebView`, `close` to the device.
  #[napi(
    ts_args_type = "event: 'webview' | 'close', optionsOrPredicate?: ((arg: AndroidWebView | AndroidDevice) => boolean | Promise<boolean>) | { predicate?: (arg: AndroidWebView | AndroidDevice) => boolean | Promise<boolean>; timeout?: number }",
    ts_return_type = "Promise<AndroidWebView | AndroidDevice>"
  )]
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn wait_for_event(
    &self,
    env: &napi::Env,
    event: String,
    options_or_predicate: Option<
      Either<napi::bindgen_prelude::Function<'_, AndroidDeviceEventArg, crate::page::PredReturn>, Object<'_>>,
    >,
  ) -> Result<AsyncBlock<AndroidDeviceEventArg>> {
    let build = |function: napi::bindgen_prelude::Function<'_, AndroidDeviceEventArg, crate::page::PredReturn>| {
      function
        .build_threadsafe_function::<AndroidDeviceEventArg>()
        .callee_handled::<false>()
        .weak::<false>()
        .max_queue_size::<0>()
        .build()
    };
    let mut timeout = None;
    let mut predicate = None;
    match options_or_predicate {
      None => {},
      Some(Either::A(function)) => predicate = Some(build(function)?),
      Some(Either::B(bag)) => {
        timeout = bag.get::<f64>("timeout")?;
        if let Some(function) = bag.get("predicate")? {
          predicate = Some(build(function)?);
        }
      },
    }
    let timeout_ms = timeout.map_or_else(|| self.inner.default_timeout(), crate::types::f64_to_u64);
    let device = self.inner.clone();
    let closed = || napi::Error::from_reason("Target page, context or browser has been closed: Device is closed");
    // Subscribed before the promise exists, so no event in between is missed.
    let mut events = self.inner.subscribe();
    AsyncBlockBuilder::new(async move {
      // Refusals reject the promise the call returns, as every other does.
      if event != "webview" && event != "close" {
        return Err(napi::Error::from_reason(format!(
          "waitForEvent: unknown event \"{event}\""
        )));
      }
      if event == "webview" && device.is_closed() {
        return Err(closed());
      }
      let happened = async {
        loop {
          let candidate = match events.recv().await {
            Ok(ferridriver::android::AndroidDeviceEvent::WebView(view)) if event == "webview" => {
              AndroidDeviceEventArg::WebView(crate::identity::AndroidWebViewObject(view))
            },
            Ok(ferridriver::android::AndroidDeviceEvent::WebView(_))
            | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Ok(ferridriver::android::AndroidDeviceEvent::Close) if event == "close" => {
              AndroidDeviceEventArg::Device(crate::identity::AndroidDeviceObject(device.clone()))
            },
            Ok(ferridriver::android::AndroidDeviceEvent::Close)
            | Err(tokio::sync::broadcast::error::RecvError::Closed) => return Err(closed()),
          };
          let Some(predicate) = &predicate else {
            return Ok(candidate);
          };
          let verdict = predicate
            .call_async(candidate.clone())
            .await
            .map_err(|error| napi::Error::from_reason(error.to_string()))?;
          if crate::page::resolve_pred(verdict).await {
            return Ok(candidate);
          }
        }
      };
      if timeout_ms == 0 {
        return happened.await;
      }
      tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), happened)
        .await
        .map_err(|_| {
          napi::Error::from_reason(format!(
            "Timeout {timeout_ms}ms exceeded while waiting for event \"{event}\""
          ))
        })?
    })
    .build(env)
  }

  /// Playwright: `androidDevice.webViews(): AndroidWebView[]`.
  #[napi(ts_return_type = "Array<AndroidWebView>")]
  pub fn web_views(&self) -> Vec<crate::identity::AndroidWebViewObject> {
    self
      .inner
      .web_views()
      .into_iter()
      .map(crate::identity::AndroidWebViewObject)
      .collect()
  }

  /// Playwright: `androidDevice.webView(selector: { pkg?, socketName? },
  /// options?: { timeout? }): Promise<AndroidWebView>`.
  #[napi(ts_return_type = "Promise<AndroidWebView>")]
  pub async fn web_view(
    &self,
    selector: AndroidWebViewSelector,
    options: Option<AndroidTimeoutOptions>,
  ) -> Result<crate::identity::AndroidWebViewObject> {
    self
      .inner
      .web_view(
        &ferridriver::android::AndroidWebViewSelector {
          pkg: selector.pkg,
          socket_name: selector.socket_name,
        },
        limit(options.and_then(|options| options.timeout)),
      )
      .await
      .map(crate::identity::AndroidWebViewObject)
      .into_napi()
  }

  /// Playwright: `androidDevice.open(command: string): Promise<AndroidSocket>`.
  #[napi]
  pub async fn open(&self, command: String) -> Result<AndroidSocket> {
    let socket = self.inner.open(&command).await.into_napi()?;
    Ok(AndroidSocket {
      inner: std::sync::Arc::new(socket),
      events: Emitter::new(),
    })
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
      Some(options) => (options.pkg, options.args, options.context.into_core()?),
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

/// Playwright `AndroidWebView`: a web view an application on the device
/// shows.
#[napi]
pub struct AndroidWebView {
  inner: ferridriver::android::AndroidWebView,
  events: Emitter<()>,
}

impl AndroidWebView {
  pub(crate) fn from_core(inner: ferridriver::android::AndroidWebView) -> Self {
    Self {
      inner,
      events: Emitter::new(),
    }
  }

  fn listen(
    &self,
    event: String,
    listener: &napi::bindgen_prelude::Function<'static, (), ()>,
    once: bool,
  ) -> Result<()> {
    if !self.events.add(event, listener, once)? {
      return Ok(());
    }
    let view = self.inner.clone();
    let listeners = std::sync::Arc::clone(&self.events.listeners);
    napi::bindgen_prelude::spawn(async move {
      view.closed().await;
      Emitter::emit(&listeners, "close", &());
    });
    Ok(())
  }
}

#[napi]
impl AndroidWebView {
  /// Playwright: `androidWebView.pid(): number`.
  #[napi]
  pub fn pid(&self) -> i64 {
    self.inner.pid()
  }

  /// Playwright: `androidWebView.pkg(): string`.
  #[napi]
  pub fn pkg(&self) -> String {
    self.inner.pkg().to_owned()
  }

  /// Playwright: `androidWebView.page(): Promise<Page>`.
  #[napi(ts_return_type = "Promise<Page>")]
  pub async fn page(&self) -> Result<crate::identity::PageObject> {
    self.inner.page().await.map(crate::identity::PageObject).into_napi()
  }

  /// Playwright: `androidWebView.on('close', listener)`.
  #[napi(ts_args_type = "event: 'close', listener: () => void")]
  pub fn on(&self, event: String, listener: napi::bindgen_prelude::Function<'static, (), ()>) -> Result<()> {
    self.listen(event, &listener, false)
  }

  /// Playwright: `androidWebView.once('close', listener)`.
  #[napi(ts_args_type = "event: 'close', listener: () => void")]
  pub fn once(&self, event: String, listener: napi::bindgen_prelude::Function<'static, (), ()>) -> Result<()> {
    self.listen(event, &listener, true)
  }

  /// Playwright: `androidWebView.off('close', listener)`.
  #[napi(ts_args_type = "event: 'close', listener: () => void")]
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn off(
    &self,
    env: &napi::Env,
    event: String,
    listener: napi::bindgen_prelude::Function<'static, (), ()>,
  ) -> Result<()> {
    self.events.remove(*env, &event, &listener)
  }
}

/// Playwright `AndroidSocket`: a connection `androidDevice.open` made.
#[napi]
pub struct AndroidSocket {
  inner: std::sync::Arc<ferridriver::android::AndroidSocket>,
  events: Emitter<AndroidSocketEventArg>,
}

impl AndroidSocket {
  /// What the device sent is held until the first listener asks, then
  /// delivered in order: `data` a chunk at a time, `close` at the end.
  fn listen(
    &self,
    event: String,
    listener: &napi::bindgen_prelude::Function<'static, AndroidSocketEventArg, ()>,
    once: bool,
  ) -> Result<()> {
    if !self.events.add(event, listener, once)? {
      return Ok(());
    }
    let socket = std::sync::Arc::clone(&self.inner);
    let listeners = std::sync::Arc::clone(&self.events.listeners);
    napi::bindgen_prelude::spawn(async move {
      while let Some(chunk) = socket.next_data().await {
        Emitter::emit(&listeners, "data", &AndroidSocketEventArg::Data(chunk));
      }
      Emitter::emit(&listeners, "close", &AndroidSocketEventArg::Close);
    });
    Ok(())
  }
}

#[napi]
impl AndroidSocket {
  /// Playwright: `androidSocket.write(data: Buffer): Promise<void>`.
  #[napi]
  pub async fn write(&self, data: Buffer) -> Result<()> {
    self.inner.write(&data).await.into_napi()
  }

  /// Playwright: `androidSocket.close(): Promise<void>`.
  #[napi]
  pub async fn close(&self) {
    self.inner.close().await;
  }

  /// Playwright: `androidSocket.on(event: 'data' | 'close', listener)`.
  #[napi(ts_args_type = "event: 'data' | 'close', listener: (data?: Buffer) => void")]
  pub fn on(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'static, AndroidSocketEventArg, ()>,
  ) -> Result<()> {
    self.listen(event, &listener, false)
  }

  /// Playwright: `androidSocket.once(event, listener)`.
  #[napi(ts_args_type = "event: 'data' | 'close', listener: (data?: Buffer) => void")]
  pub fn once(
    &self,
    event: String,
    listener: napi::bindgen_prelude::Function<'static, AndroidSocketEventArg, ()>,
  ) -> Result<()> {
    self.listen(event, &listener, true)
  }

  /// Playwright: `androidSocket.off(event, listener)`.
  #[napi(ts_args_type = "event: 'data' | 'close', listener: (data?: Buffer) => void")]
  #[allow(clippy::trivially_copy_pass_by_ref)]
  pub fn off(
    &self,
    env: &napi::Env,
    event: String,
    listener: napi::bindgen_prelude::Function<'static, AndroidSocketEventArg, ()>,
  ) -> Result<()> {
    self.events.remove(*env, &event, &listener)
  }
}

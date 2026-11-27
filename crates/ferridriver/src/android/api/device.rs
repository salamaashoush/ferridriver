//! `AndroidDevice` shell, input, screenshot, browser launch, and `WebView` discovery methods.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::super::driver::{AndroidDirection, AndroidElementInfo, AndroidSelector, Driver};
use super::handles::WebViewInner;
use super::{
  AndroidDeviceEvent, AndroidFile, AndroidInput, AndroidLaunchBrowserOptions, AndroidSocket, AndroidWebView,
  AndroidWebViewSelector, COMMAND_LINE_FILE, DeviceInner, LAUNCH_TIMEOUT_MS, LAUNCHES, WEB_VIEW_POLL,
};
use crate::error::{FerriError, Result};
use crate::options::Point;

/// Playwright: `AndroidDevice`.
#[derive(Clone)]
pub struct AndroidDevice {
  pub(super) inner: Arc<DeviceInner>,
}

impl std::fmt::Debug for AndroidDevice {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("AndroidDevice")
      .field("serial", &self.inner.serial)
      .field("model", &self.inner.model)
      .finish_non_exhaustive()
  }
}

/// Two handles are equal when they are the same device object, which is
/// what `android.devices()` hands out for a device until it is closed.
impl PartialEq for AndroidDevice {
  fn eq(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.inner, &other.inner)
  }
}

impl AndroidDevice {
  /// Whether [`Self::close`] has been called on this device.
  #[must_use]
  pub fn is_closed(&self) -> bool {
    self.inner.closed.load(Ordering::SeqCst)
  }

  pub(super) fn ensure_open(&self) -> Result<()> {
    if self.is_closed() {
      return Err(FerriError::target_closed(Some("Device is closed".into())));
    }
    Ok(())
  }

  /// Playwright: `androidDevice.serial(): string`.
  #[must_use]
  pub fn serial(&self) -> &str {
    &self.inner.serial
  }

  /// Playwright: `androidDevice.model(): string`, the device's
  /// `ro.product.model`.
  #[must_use]
  pub fn model(&self) -> &str {
    &self.inner.model
  }

  /// Playwright: `androidDevice.shell(command: string): Promise<Buffer>`.
  /// Everything the command wrote, byte for byte.
  ///
  /// # Errors
  ///
  /// Returns an error if the device is closed or the ADB server refuses
  /// the command.
  pub async fn shell(&self, command: &str) -> Result<Vec<u8>> {
    self.ensure_open()?;
    self.inner.server.shell(&self.inner.serial, command).await
  }

  /// Playwright: `androidDevice.screenshot(options?: { path? }): Promise<Buffer>`.
  /// The screen as a PNG, also written to `path` when one is given.
  ///
  /// # Errors
  ///
  /// Returns an error if the device is closed, the capture fails, or
  /// `path` cannot be written.
  pub async fn screenshot(&self, path: Option<&std::path::Path>) -> Result<Vec<u8>> {
    let image = self.shell("screencap -p").await?;
    if let Some(path) = path {
      tokio::fs::write(path, &image)
        .await
        .map_err(|error| FerriError::invalid_argument("path", format!("{}: {error}", path.display())))?;
    }
    Ok(image)
  }

  /// Playwright: `androidDevice.push(file: string | Buffer, path: string,
  /// options?: { mode?: number }): Promise<void>`. `mode` is `0o644`
  /// when absent.
  ///
  /// # Errors
  ///
  /// Returns an error if the device is closed, `file` cannot be read, or
  /// the device refuses the write.
  pub async fn push(&self, file: AndroidFile, path: &str, mode: Option<u32>) -> Result<()> {
    self.ensure_open()?;
    let content = file.load().await?;
    self
      .inner
      .server
      .push(&self.inner.serial, &content, path, mode.unwrap_or(0o644))
      .await
  }

  /// Playwright: `androidDevice.installApk(file: string | Buffer,
  /// options?: { args?: string[] }): Promise<void>`. `args` go to
  /// `cmd package install` and are `-r -t -S` when absent.
  ///
  /// # Errors
  ///
  /// Returns an error if the device is closed, `file` cannot be read, or
  /// the package manager does not report success.
  pub async fn install_apk(&self, file: AndroidFile, args: Option<Vec<String>>) -> Result<()> {
    self.ensure_open()?;
    let apk = file.load().await?;
    self
      .inner
      .server
      .install(&self.inner.serial, &apk, &args.unwrap_or_default())
      .await
  }

  /// Playwright: `androidDevice.open(command: string): Promise<AndroidSocket>`.
  /// A connection to an ADB service on the device, `shell:logcat` or
  /// `localabstract:<name>` for example.
  ///
  /// # Errors
  ///
  /// Returns an error if the device is closed or the service is refused.
  pub async fn open(&self, command: &str) -> Result<AndroidSocket> {
    self.ensure_open()?;
    let stream = self.inner.server.open(&self.inner.serial, command).await?;
    Ok(AndroidSocket::new(stream))
  }

  /// Playwright: `androidDevice.setDefaultTimeout(timeout: number)`. How
  /// long a selector action waits for its view when given no timeout of
  /// its own. It reaches the driver as it is, and the driver reads zero
  /// as "look once", not as "wait for ever".
  pub fn set_default_timeout(&self, timeout_ms: u64) {
    self.inner.default_timeout.store(timeout_ms, Ordering::Relaxed);
  }

  /// The limit [`Self::set_default_timeout`] set, thirty seconds until
  /// it is called.
  #[must_use]
  pub fn default_timeout(&self) -> u64 {
    self.inner.default_timeout.load(Ordering::Relaxed)
  }

  fn timeout(&self, timeout_ms: Option<u64>) -> u64 {
    timeout_ms.unwrap_or_else(|| self.inner.default_timeout.load(Ordering::Relaxed))
  }

  /// The driver on the device, installed and started on first use.
  pub(super) async fn driver(&self) -> Result<Arc<Driver>> {
    self.ensure_open()?;
    let mut driver = self.inner.driver.lock().await;
    if let Some(driver) = driver.as_ref() {
      return Ok(Arc::clone(driver));
    }
    let apks = crate::install::BrowserInstaller::new().find_installed_android_driver();
    let started = Arc::new(
      Driver::start(
        &self.inner.server,
        &self.inner.serial,
        self.inner.omit_driver_install,
        apks.as_deref(),
        self.timeout(None),
      )
      .await?,
    );
    *driver = Some(Arc::clone(&started));
    Ok(started)
  }

  /// One selector call to the driver.
  async fn act(
    &self,
    method: &str,
    selector: &AndroidSelector,
    mut params: serde_json::Value,
    timeout_ms: Option<u64>,
  ) -> Result<serde_json::Value> {
    if let Some(bag) = params.as_object_mut() {
      bag.insert("selector".to_owned(), selector.to_wire());
    }
    let driver = self.driver().await?;
    driver.send(method, params, self.timeout(timeout_ms)).await
  }

  /// Playwright: `androidDevice.wait(selector, options?: { state?: 'gone',
  /// timeout? })`. Until a view matches.
  ///
  /// `gone` is sent as Playwright sends it, and Playwright's driver
  /// (revision 1001) does not read it: it waits for a match either way.
  ///
  /// # Errors
  ///
  /// Returns an error if the device is closed, the driver cannot start,
  /// or the wait times out.
  pub async fn wait(&self, selector: &AndroidSelector, gone: bool, timeout_ms: Option<u64>) -> Result<()> {
    let params = if gone {
      serde_json::json!({ "state": "gone" })
    } else {
      serde_json::json!({})
    };
    self.act("wait", selector, params, timeout_ms).await.map(|_| ())
  }

  /// Playwright: `androidDevice.fill(selector, text, options?)`. Taps
  /// the view, then sets its text.
  ///
  /// # Errors
  ///
  /// As [`Self::wait`], and if the view takes no text.
  pub async fn fill(&self, selector: &AndroidSelector, text: &str, timeout_ms: Option<u64>) -> Result<()> {
    self.act("click", selector, serde_json::json!({}), timeout_ms).await?;
    self
      .act("fill", selector, serde_json::json!({ "text": text }), timeout_ms)
      .await
      .map(|_| ())
  }

  /// Playwright: `androidDevice.press(selector, key, options?)`. Taps
  /// the view, then presses `key`.
  ///
  /// # Errors
  ///
  /// As [`Self::wait`], and for a key that is not an `AndroidKey`.
  pub async fn press(&self, selector: &AndroidSelector, key: &str, timeout_ms: Option<u64>) -> Result<()> {
    self.tap(selector, None, timeout_ms).await?;
    self.input().press(key).await
  }

  /// Playwright: `androidDevice.tap(selector, options?: { duration?,
  /// timeout? })`. `duration` is how long the tap is held, in
  /// milliseconds.
  ///
  /// # Errors
  ///
  /// As [`Self::wait`].
  pub async fn tap(&self, selector: &AndroidSelector, duration_ms: Option<f64>, timeout_ms: Option<u64>) -> Result<()> {
    let params = match duration_ms {
      Some(duration) => serde_json::json!({ "duration": duration }),
      None => serde_json::json!({}),
    };
    self.act("click", selector, params, timeout_ms).await.map(|_| ())
  }

  /// Playwright: `androidDevice.longTap(selector, options?)`.
  ///
  /// # Errors
  ///
  /// As [`Self::wait`].
  pub async fn long_tap(&self, selector: &AndroidSelector, timeout_ms: Option<u64>) -> Result<()> {
    self
      .act("longClick", selector, serde_json::json!({}), timeout_ms)
      .await
      .map(|_| ())
  }

  /// Playwright: `androidDevice.drag(selector, dest, options?: { speed?,
  /// timeout? })`. `speed` is in pixels per second.
  ///
  /// # Errors
  ///
  /// As [`Self::wait`].
  pub async fn drag(
    &self,
    selector: &AndroidSelector,
    dest: Point,
    speed: Option<f64>,
    timeout_ms: Option<u64>,
  ) -> Result<()> {
    let mut params = serde_json::json!({ "dest": { "x": dest.x, "y": dest.y } });
    Self::with_speed(&mut params, speed);
    self.act("drag", selector, params, timeout_ms).await.map(|_| ())
  }

  /// Playwright: `androidDevice.fling(selector, direction, options?)`.
  ///
  /// # Errors
  ///
  /// As [`Self::wait`].
  pub async fn fling(
    &self,
    selector: &AndroidSelector,
    direction: AndroidDirection,
    speed: Option<f64>,
    timeout_ms: Option<u64>,
  ) -> Result<()> {
    let mut params = serde_json::json!({ "direction": direction.as_str() });
    Self::with_speed(&mut params, speed);
    self.act("fling", selector, params, timeout_ms).await.map(|_| ())
  }

  /// Playwright: `androidDevice.pinchClose(selector, percent, options?)`.
  /// `percent` is the size of the pinch as a fraction of the view.
  ///
  /// # Errors
  ///
  /// As [`Self::wait`].
  pub async fn pinch_close(
    &self,
    selector: &AndroidSelector,
    percent: f64,
    speed: Option<f64>,
    timeout_ms: Option<u64>,
  ) -> Result<()> {
    let mut params = serde_json::json!({ "percent": percent });
    Self::with_speed(&mut params, speed);
    self.act("pinchClose", selector, params, timeout_ms).await.map(|_| ())
  }

  /// Playwright: `androidDevice.pinchOpen(selector, percent, options?)`.
  ///
  /// # Errors
  ///
  /// As [`Self::wait`].
  pub async fn pinch_open(
    &self,
    selector: &AndroidSelector,
    percent: f64,
    speed: Option<f64>,
    timeout_ms: Option<u64>,
  ) -> Result<()> {
    let mut params = serde_json::json!({ "percent": percent });
    Self::with_speed(&mut params, speed);
    self.act("pinchOpen", selector, params, timeout_ms).await.map(|_| ())
  }

  /// Playwright: `androidDevice.scroll(selector, direction, percent,
  /// options?)`. `percent` is the distance as a fraction of the view.
  ///
  /// # Errors
  ///
  /// As [`Self::wait`].
  pub async fn scroll(
    &self,
    selector: &AndroidSelector,
    direction: AndroidDirection,
    percent: f64,
    speed: Option<f64>,
    timeout_ms: Option<u64>,
  ) -> Result<()> {
    let mut params = serde_json::json!({ "direction": direction.as_str(), "percent": percent });
    Self::with_speed(&mut params, speed);
    self.act("scroll", selector, params, timeout_ms).await.map(|_| ())
  }

  /// Playwright: `androidDevice.swipe(selector, direction, percent,
  /// options?)`.
  ///
  /// # Errors
  ///
  /// As [`Self::wait`].
  pub async fn swipe(
    &self,
    selector: &AndroidSelector,
    direction: AndroidDirection,
    percent: f64,
    speed: Option<f64>,
    timeout_ms: Option<u64>,
  ) -> Result<()> {
    let mut params = serde_json::json!({ "direction": direction.as_str(), "percent": percent });
    Self::with_speed(&mut params, speed);
    self.act("swipe", selector, params, timeout_ms).await.map(|_| ())
  }

  fn with_speed(params: &mut serde_json::Value, speed: Option<f64>) {
    if let (Some(bag), Some(speed)) = (params.as_object_mut(), speed) {
      bag.insert("speed".to_owned(), speed.into());
    }
  }

  /// Playwright: `androidDevice.info(selector): Promise<AndroidElementInfo>`.
  ///
  /// # Errors
  ///
  /// Returns an error if the device is closed, the driver cannot start,
  /// or no view matches.
  pub async fn info(&self, selector: &AndroidSelector) -> Result<AndroidElementInfo> {
    // Upstream gives this call no limit of its own.
    let info = self.act("info", selector, serde_json::json!({}), Some(0)).await?;
    serde_json::from_value(info).map_err(|error| FerriError::protocol("Android driver", format!("info: {error}")))
  }

  /// Playwright: `androidDevice.input`, raw input that addresses the
  /// screen and not a view.
  #[must_use]
  pub fn input(&self) -> AndroidInput {
    AndroidInput { device: self.clone() }
  }

  /// The device's `webview` and `close` events, from now on.
  #[must_use]
  pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<AndroidDeviceEvent> {
    self.inner.events.subscribe()
  }

  /// Playwright: `androidDevice.webViews(): AndroidWebView[]`. The web
  /// views the device had when it was last asked, twice a second.
  #[must_use]
  pub fn web_views(&self) -> Vec<AndroidWebView> {
    self
      .inner
      .web_views
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
  }

  /// Playwright: `androidDevice.webView(selector: { pkg?, socketName? },
  /// options?): Promise<AndroidWebView>`. The web view that matches, once
  /// there is one.
  ///
  /// # Errors
  ///
  /// Returns an error if the device closes first, or none appears within
  /// the timeout (the device's default when `None`, no limit when zero).
  pub async fn web_view(&self, selector: &AndroidWebViewSelector, timeout_ms: Option<u64>) -> Result<AndroidWebView> {
    self.ensure_open()?;
    let matches = |view: &AndroidWebView| match (&selector.pkg, &selector.socket_name) {
      (Some(pkg), _) => view.pkg() == pkg,
      (None, Some(socket_name)) => view.socket_name() == socket_name,
      (None, None) => false,
    };
    // Subscribed before the list is read, so a view that appears in between is not missed.
    let mut events = self.subscribe();
    if let Some(view) = self.web_views().into_iter().find(|view| matches(view)) {
      return Ok(view);
    }
    let timeout_ms = self.timeout(timeout_ms);
    let appeared = async {
      loop {
        match events.recv().await {
          Ok(AndroidDeviceEvent::WebView(view)) if matches(&view) => return Ok(view),
          Ok(AndroidDeviceEvent::Close) | Err(tokio::sync::broadcast::error::RecvError::Closed) => {
            return Err(FerriError::target_closed(Some("Device is closed".into())));
          },
          // A lagged receiver may have missed the view: look at the list again.
          Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
            if let Some(view) = self.web_views().into_iter().find(|view| matches(view)) {
              return Ok(view);
            }
          },
          Ok(AndroidDeviceEvent::WebView(_)) => {},
        }
      }
    };
    if timeout_ms == 0 {
      return appeared.await;
    }
    tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), appeared)
      .await
      .map_err(|_| FerriError::timeout("waiting for event \"webview\"", timeout_ms))?
  }

  /// Ask the device which web views it has and tell listeners what
  /// changed: `server/android/android.ts::_refreshWebViews`.
  pub(super) async fn refresh_web_views(&self) -> Result<()> {
    let listing = self
      .inner
      .server
      .shell(&self.inner.serial, "cat /proc/net/unix | grep webview_devtools_remote")
      .await?;
    let listed = web_view_sockets(&String::from_utf8_lossy(&listing));
    let known: Vec<String> = self
      .web_views()
      .iter()
      .map(|view| view.socket_name().to_owned())
      .collect();
    for (socket_name, pid) in &listed {
      if known.contains(socket_name) {
        continue;
      }
      let pkg = match pid {
        Some(pid) => {
          let processes = self
            .inner
            .server
            .shell(&self.inner.serial, &format!("ps -A | grep {pid}"))
            .await?;
          process_package(&String::from_utf8_lossy(&processes), *pid)
        },
        None => String::new(),
      };
      if self.is_closed() {
        return Ok(());
      }
      let view = AndroidWebView {
        inner: Arc::new(WebViewInner {
          device: Arc::downgrade(&self.inner),
          pid: pid.map_or(-1, i64::from),
          pkg,
          socket_name: socket_name.clone(),
          page: tokio::sync::OnceCell::new(),
          closed: tokio::sync::watch::channel(false).0,
        }),
      };
      self
        .inner
        .web_views
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(view.clone());
      let _ = self.inner.events.send(AndroidDeviceEvent::WebView(view));
    }
    let gone: Vec<AndroidWebView> = {
      let mut views = self
        .inner
        .web_views
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      let (kept, gone) = std::mem::take(&mut *views)
        .into_iter()
        .partition(|view| listed.iter().any(|(socket_name, _)| socket_name == view.socket_name()));
      *views = kept;
      gone
    };
    for view in gone {
      view.inner.closed.send_replace(true);
    }
    Ok(())
  }

  /// Keep the list of web views current until the device closes. A
  /// device that stops answering is closed, as upstream closes it.
  pub(super) fn watch_web_views(&self) {
    let device = Arc::downgrade(&self.inner);
    tokio::spawn(async move {
      loop {
        tokio::time::sleep(WEB_VIEW_POLL).await;
        let Some(inner) = device.upgrade() else { return };
        let device = AndroidDevice { inner };
        if device.is_closed() {
          return;
        }
        if device.refresh_web_views().await.is_err() {
          device.close().await;
          return;
        }
      }
    });
  }

  /// Playwright: `androidDevice.launchBrowser(options?): Promise<BrowserContext>`.
  /// Start the browser `pkg` afresh with the command line its options
  /// ask for and return its context, the one the device has.
  ///
  /// The package is made the device's debug app so that a release build
  /// reads the command-line file too, where Playwright asks for the
  /// browser's "command line on non-rooted devices" flag to be on.
  ///
  /// # Errors
  ///
  /// Returns an error if the device is closed, the package or an argument
  /// is not usable, the browser does not start, or its `DevTools` socket
  /// does not open within thirty seconds.
  pub async fn launch_browser(&self, options: AndroidLaunchBrowserOptions) -> Result<crate::context::ContextRef> {
    // Boxed here once, so a caller's own future does not carry the whole launch.
    Box::pin(self.launch(options)).await
  }

  async fn launch(&self, options: AndroidLaunchBrowserOptions) -> Result<crate::context::ContextRef> {
    self.ensure_open()?;
    let server = &self.inner.server;
    let serial = self.inner.serial.as_str();
    let pkg = options.pkg.unwrap_or_else(|| "com.android.chrome".into());
    let socket = format!(
      "ferridriver_{}_{}_devtools_remote",
      std::process::id(),
      LAUNCHES.fetch_add(1, Ordering::Relaxed)
    );
    let mut args = options
      .proxy
      .as_ref()
      .map(|proxy| proxy.launch_flags(crate::backend::BackendKind::CdpWs))
      .unwrap_or_default();
    args.extend(options.args);
    // Validates the package before it is spliced into a shell command.
    let line = super::super::command_line(&pkg, &args, &socket, false)?;

    server.shell(serial, &format!("am force-stop {pkg}")).await?;
    let installed = server.shell(serial, &format!("pm path {pkg}")).await?;
    if !installed.starts_with(b"package:") {
      return Err(FerriError::backend(format!(
        "Android browser package {pkg} is not installed on {serial}"
      )));
    }
    let api_level = server.shell(serial, "getprop ro.build.version.sdk").await?;
    if String::from_utf8_lossy(&api_level)
      .trim()
      .parse::<u32>()
      .is_ok_and(|level| level >= 33)
    {
      // Otherwise Chrome's notification explainer covers the page.
      server
        .shell(serial, &format!("pm grant {pkg} android.permission.POST_NOTIFICATIONS"))
        .await?;
    }
    server
      .shell(serial, &format!("am set-debug-app --persistent {pkg}"))
      .await?;
    server.push(serial, line.as_bytes(), COMMAND_LINE_FILE, 0o644).await?;
    let started = server
      .shell(
        serial,
        &format!("am start -a android.intent.action.VIEW -d about:blank {pkg}"),
      )
      .await?;
    let started = String::from_utf8_lossy(&started);
    if started.contains("Error") {
      return Err(FerriError::backend(format!(
        "could not start {pkg}: {}",
        started.trim()
      )));
    }

    let port = server.forward(serial, &format!("localabstract:{socket}")).await?;
    let connected = async {
      let endpoint = format!("http://127.0.0.1:{port}");
      Self::wait_for_devtools(&endpoint).await?;
      let mut context = options.context;
      if context.viewport == crate::options::ViewportOption::Default {
        // The device has a screen of its own; Playwright connects with `noDefaultViewport`.
        context.viewport = crate::options::ViewportOption::Null;
      }
      crate::BrowserType::connect_persistent_over_cdp(
        &endpoint,
        context,
        Some(super::super::remote_downloads(server, serial)),
      )
      .await
    }
    .await;
    let removed = server.shell(serial, &format!("rm -f {COMMAND_LINE_FILE}")).await;
    match connected {
      Ok(context) => {
        removed?;
        self.inner.browsers.lock().await.push((context.clone(), port));
        Ok(context)
      },
      Err(error) => {
        let _ = server.kill_forward(serial, port).await;
        Err(error)
      },
    }
  }

  /// Wait until the browser behind `endpoint` answers on its `DevTools`
  /// socket: the forward exists as soon as it is asked for, the socket
  /// behind it only once the browser has started.
  async fn wait_for_devtools(endpoint: &str) -> Result<()> {
    let client = reqwest::Client::builder()
      .timeout(std::time::Duration::from_secs(2))
      .build()
      .map_err(|error| FerriError::backend(error.to_string()))?;
    let url = format!("{endpoint}/json/version");
    let ready = async {
      loop {
        if let Ok(response) = client.get(&url).send().await
          && response.status().is_success()
        {
          return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
      }
    };
    tokio::time::timeout(std::time::Duration::from_millis(LAUNCH_TIMEOUT_MS), ready)
      .await
      .map_err(|_| FerriError::timeout("waiting for the Android browser's DevTools socket", LAUNCH_TIMEOUT_MS))
  }

  /// Playwright: `androidDevice.close(): Promise<void>`. The browsers it
  /// launched are disconnected, every later call on the device fails,
  /// and `android.devices()` hands out a new object for it.
  pub async fn close(&self) {
    if self.inner.closed.swap(true, Ordering::SeqCst) {
      return;
    }
    if let Some(registry) = self.inner.registry.upgrade() {
      registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|device| !Arc::ptr_eq(&device.inner, &self.inner));
    }
    // Dropping the driver closes its socket and the shell it runs in.
    self.inner.driver.lock().await.take();
    let views = std::mem::take(
      &mut *self
        .inner
        .web_views
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    for view in views {
      view.inner.closed.send_replace(true);
    }
    let _ = self.inner.events.send(AndroidDeviceEvent::Close);
    let browsers = std::mem::take(&mut *self.inner.browsers.lock().await);
    for (context, port) in browsers {
      if !context.is_closed()
        && let Err(error) = context.close().await
      {
        tracing::debug!(target: "ferridriver::android", %error, "closing a launched browser failed");
      }
      if let Err(error) = self.inner.server.kill_forward(&self.inner.serial, port).await {
        tracing::debug!(target: "ferridriver::android", %error, port, "removing a DevTools forward failed");
      }
    }
  }
}

/// The `DevTools` sockets of web views in a `/proc/net/unix` listing, each
/// with the process id its name ends in when it has one:
/// `webview_devtools_remote_32327`, `webview_devtools_remote_32327_zeus`
/// and `webview_devtools_remote_zeus` are all names upstream expects.
pub(super) fn web_view_sockets(listing: &str) -> Vec<(String, Option<u32>)> {
  const MARKER: &str = "webview_devtools_remote";
  let mut sockets: Vec<(String, Option<u32>)> = Vec::new();
  for line in listing.lines() {
    let Some((_, name)) = line.split_once('@') else {
      continue;
    };
    let name = name.trim();
    let Some((_, after)) = name.split_once(MARKER) else {
      continue;
    };
    let digits: String = after
      .trim_start_matches('_')
      .chars()
      .take_while(char::is_ascii_digit)
      .collect();
    if !sockets.iter().any(|(known, _)| known == name) {
      sockets.push((name.to_owned(), digits.parse().ok()));
    }
  }
  sockets
}

/// The package of process `pid` in a `ps -A` listing: the last column of
/// the line whose second column is the pid.
pub(super) fn process_package(listing: &str, pid: u32) -> String {
  listing
    .lines()
    .find_map(|line| {
      let mut columns = line.split_whitespace();
      let listed: u32 = columns.nth(1)?.parse().ok()?;
      (listed == pid).then(|| line.split_whitespace().last().unwrap_or_default().to_owned())
    })
    .unwrap_or_default()
}

//! Playwright's `_android`: the devices behind an ADB server, driven
//! through that server's own protocol with no `adb` binary in between.
//! `client/android.ts` for the shape, `server/android/android.ts` for what
//! each call does.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::driver::{AndroidDirection, AndroidElementInfo, AndroidSelector, Driver, key_code, key_codes};
use super::wire::AdbServer;
use crate::error::{FerriError, Result};
use crate::options::Point;

/// Playwright: `android.devices(options?: { host?, port?, omitDriverInstall? })`.
#[derive(Debug, Clone, Default)]
pub struct AndroidDevicesOptions {
  /// Where the ADB server listens. `127.0.0.1` when absent.
  pub host: Option<String>,
  /// The ADB server's port. `5037` when absent.
  pub port: Option<u16>,
  /// Leave the device's driver as it is instead of installing it before
  /// the first call that needs it.
  pub omit_driver_install: Option<bool>,
}

/// A file argument: Playwright's `string | Buffer`, a path on this
/// machine or the bytes themselves.
#[derive(Debug, Clone)]
pub enum AndroidFile {
  Path(PathBuf),
  Bytes(Vec<u8>),
}

impl AndroidFile {
  async fn load(self) -> Result<Vec<u8>> {
    match self {
      Self::Bytes(bytes) => Ok(bytes),
      Self::Path(path) => tokio::fs::read(&path)
        .await
        .map_err(|error| FerriError::invalid_argument("file", format!("{}: {error}", path.display()))),
    }
  }
}

/// Playwright: `androidDevice.launchBrowser(options?: BrowserContextOptions
/// & { pkg?, args?, proxy? })`.
#[derive(Debug, Clone, Default)]
pub struct AndroidLaunchBrowserOptions {
  /// The browser's application package. `com.android.chrome` when absent.
  pub pkg: Option<String>,
  /// Switches added to the browser's command line.
  pub args: Vec<String>,
  /// The proxy every request of the browser goes through.
  pub proxy: Option<crate::options::ProxyConfig>,
  /// The options of the context returned, the device's one context.
  pub context: crate::options::BrowserContextOptions,
}

/// Where Chromium on Android reads its command line from at start.
const COMMAND_LINE_FILE: &str = "/data/local/tmp/chrome-command-line";

/// Playwright's default for a call that waits, `DEFAULT_PLAYWRIGHT_TIMEOUT`.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// How long a launched browser has to open its `DevTools` socket.
const LAUNCH_TIMEOUT_MS: u64 = 30_000;

static LAUNCHES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

type Registry = Arc<std::sync::Mutex<Vec<AndroidDevice>>>;

/// Playwright: `Android`, reached there as `playwright._android`.
#[derive(Clone, Default)]
pub struct Android {
  /// The devices handed out and not yet closed. A device listed again is
  /// the same object, as in Playwright's `Android._devices`.
  devices: Registry,
}

impl Android {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Playwright: `android.devices(options?): Promise<AndroidDevice[]>`.
  /// The devices the ADB server reports as usable; one that is offline
  /// or unauthorized is not among them.
  ///
  /// # Errors
  ///
  /// Returns an error if no ADB server answers at the address, or a
  /// listed device does not answer for its model.
  pub async fn devices(&self, options: AndroidDevicesOptions) -> Result<Vec<AndroidDevice>> {
    let omit_driver_install = options.omit_driver_install.unwrap_or(false);
    let server = AdbServer {
      host: options.host.unwrap_or_else(|| AdbServer::default().host),
      port: options.port.unwrap_or_else(|| AdbServer::default().port),
    };
    let listed: Vec<String> = server
      .devices()
      .await?
      .into_iter()
      .filter(|entry| entry.state == "device")
      .map(|entry| entry.serial)
      .collect();

    let known = |serial: &str| {
      self
        .devices
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|device| device.inner.server == server && device.inner.serial == serial)
    };
    for serial in &listed {
      if known(serial) {
        continue;
      }
      let model = server.shell(serial, "getprop ro.product.model").await?;
      let device = AndroidDevice {
        inner: Arc::new(DeviceInner {
          server: server.clone(),
          serial: serial.clone(),
          model: String::from_utf8_lossy(&model).trim().to_owned(),
          closed: AtomicBool::new(false),
          registry: Arc::downgrade(&self.devices),
          browsers: tokio::sync::Mutex::new(Vec::new()),
          omit_driver_install,
          driver: tokio::sync::Mutex::new(None),
          default_timeout: std::sync::atomic::AtomicU64::new(DEFAULT_TIMEOUT_MS),
        }),
      };
      self
        .devices
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(device);
    }

    let mut devices = self.devices.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    devices.retain(|device| device.inner.server != server || listed.contains(&device.inner.serial));
    Ok(
      devices
        .iter()
        .filter(|device| device.inner.server == server)
        .cloned()
        .collect(),
    )
  }
}

struct DeviceInner {
  server: AdbServer,
  serial: String,
  model: String,
  closed: AtomicBool,
  registry: std::sync::Weak<std::sync::Mutex<Vec<AndroidDevice>>>,
  /// The browsers this device launched and the local port each one's
  /// `DevTools` socket is forwarded to, for [`AndroidDevice::close`].
  browsers: tokio::sync::Mutex<Vec<(crate::context::ContextRef, u16)>>,
  omit_driver_install: bool,
  /// The driver, installed and started by the first call that needs it.
  driver: tokio::sync::Mutex<Option<Arc<Driver>>>,
  default_timeout: std::sync::atomic::AtomicU64,
}

/// Playwright: `AndroidDevice`.
#[derive(Clone)]
pub struct AndroidDevice {
  inner: Arc<DeviceInner>,
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

  fn ensure_open(&self) -> Result<()> {
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

  fn timeout(&self, timeout_ms: Option<u64>) -> u64 {
    timeout_ms.unwrap_or_else(|| self.inner.default_timeout.load(Ordering::Relaxed))
  }

  /// The driver on the device, installed and started on first use.
  async fn driver(&self) -> Result<Arc<Driver>> {
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
    let line = super::command_line(&pkg, &args, &socket, false)?;

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
      crate::BrowserType::connect_persistent_over_cdp(&endpoint, context).await
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

/// Playwright: `AndroidInput`, reached as `androidDevice.input`.
#[derive(Clone)]
pub struct AndroidInput {
  device: AndroidDevice,
}

impl AndroidInput {
  async fn send(&self, method: &str, params: serde_json::Value) -> Result<()> {
    let driver = self.device.driver().await?;
    // Upstream gives the input calls no limit of their own.
    driver.send(method, params, 0).await.map(|_| ())
  }

  /// Playwright: `androidInput.type(text: string)`. A key press per
  /// character.
  ///
  /// # Errors
  ///
  /// Returns an error for a character no `AndroidKey` stands for, and
  /// if the device is closed or the driver cannot start.
  pub async fn type_text(&self, text: &str) -> Result<()> {
    for code in key_codes(text)? {
      self.send("inputPress", serde_json::json!({ "keyCode": code })).await?;
    }
    Ok(())
  }

  /// Playwright: `androidInput.press(key: AndroidKey)`.
  ///
  /// # Errors
  ///
  /// Returns an error for a key that is not an `AndroidKey`, and if the
  /// device is closed or the driver cannot start.
  pub async fn press(&self, key: &str) -> Result<()> {
    let code = key_code(key)?;
    self.send("inputPress", serde_json::json!({ "keyCode": code })).await
  }

  /// Playwright: `androidInput.tap(point: { x, y })`.
  ///
  /// # Errors
  ///
  /// Returns an error if the device is closed or the driver cannot start.
  pub async fn tap(&self, point: Point) -> Result<()> {
    self
      .send(
        "inputClick",
        serde_json::json!({ "point": { "x": point.x, "y": point.y } }),
      )
      .await
  }

  /// Playwright: `androidInput.swipe(from, segments, steps)`. From
  /// `from` through each of `segments`, `steps` moves per segment.
  ///
  /// # Errors
  ///
  /// Returns an error if the device is closed or the driver cannot start.
  pub async fn swipe(&self, from: Point, segments: &[Point], steps: u32) -> Result<()> {
    let path: Vec<serde_json::Value> = std::iter::once(&from)
      .chain(segments)
      .map(|point| serde_json::json!({ "x": point.x, "y": point.y }))
      .collect();
    self
      .send("inputSwipe", serde_json::json!({ "segments": path, "steps": steps }))
      .await
  }

  /// Playwright: `androidInput.drag(from, to, steps)`.
  ///
  /// # Errors
  ///
  /// Returns an error if the device is closed or the driver cannot start.
  pub async fn drag(&self, from: Point, to: Point, steps: u32) -> Result<()> {
    self
      .send(
        "inputDrag",
        serde_json::json!({
          "from": { "x": from.x, "y": from.y },
          "to": { "x": to.x, "y": to.y },
          "steps": steps,
        }),
      )
      .await
  }
}

/// Playwright: `AndroidSocket`, a connection `androidDevice.open` made.
pub struct AndroidSocket {
  writer: tokio::sync::Mutex<Option<tokio::net::tcp::OwnedWriteHalf>>,
  data: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>,
  reader: tokio::task::AbortHandle,
}

impl AndroidSocket {
  fn new(stream: tokio::net::TcpStream) -> Self {
    let (mut read, write) = stream.into_split();
    let (sender, data) = tokio::sync::mpsc::unbounded_channel();
    let reader = tokio::spawn(async move {
      let mut buffer = vec![0u8; 64 * 1024];
      loop {
        match read.read(&mut buffer).await {
          Ok(0) | Err(_) => break,
          Ok(length) => {
            if sender.send(buffer[..length].to_vec()).is_err() {
              break;
            }
          },
        }
      }
    })
    .abort_handle();
    Self {
      writer: tokio::sync::Mutex::new(Some(write)),
      data: tokio::sync::Mutex::new(data),
      reader,
    }
  }

  /// Playwright: `androidSocket.write(data: Buffer): Promise<void>`.
  ///
  /// # Errors
  ///
  /// Returns an error if the socket is closed.
  pub async fn write(&self, data: &[u8]) -> Result<()> {
    let mut writer = self.writer.lock().await;
    let Some(writer) = writer.as_mut() else {
      return Err(FerriError::target_closed(Some("Socket is closed".into())));
    };
    writer.write_all(data).await?;
    Ok(())
  }

  /// The next bytes the device sent: Playwright's `data` event, one
  /// chunk at a time. `None` once the connection has closed, which is
  /// its `close` event.
  pub async fn next_data(&self) -> Option<Vec<u8>> {
    self.data.lock().await.recv().await
  }

  /// Playwright: `androidSocket.close(): Promise<void>`.
  pub async fn close(&self) {
    self.writer.lock().await.take();
    self.reader.abort();
    self.data.lock().await.close();
  }
}

impl Drop for AndroidSocket {
  fn drop(&mut self) {
    self.reader.abort();
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::net::TcpListener;

  /// An ADB server with the devices `listing` names, whose every shell
  /// answers with the command it was given.
  async fn adb(listing: &'static str) -> AndroidDevicesOptions {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
      loop {
        let Ok((mut stream, _)) = listener.accept().await else {
          return;
        };
        tokio::spawn(async move {
          let mut digits = [0u8; 4];
          stream.read_exact(&mut digits).await.unwrap();
          let length = usize::from_str_radix(std::str::from_utf8(&digits).unwrap(), 16).unwrap();
          let mut message = vec![0u8; length];
          stream.read_exact(&mut message).await.unwrap();
          let message = String::from_utf8(message).unwrap();
          if message == "host:devices" {
            stream
              .write_all(format!("OKAY{:04x}{listing}", listing.len()).as_bytes())
              .await
              .unwrap();
            return;
          }
          assert!(message.starts_with("host:transport:"), "{message}");
          stream.write_all(b"OKAY").await.unwrap();
          let mut digits = [0u8; 4];
          stream.read_exact(&mut digits).await.unwrap();
          let length = usize::from_str_radix(std::str::from_utf8(&digits).unwrap(), 16).unwrap();
          let mut service = vec![0u8; length];
          stream.read_exact(&mut service).await.unwrap();
          let service = String::from_utf8(service).unwrap();
          stream.write_all(b"OKAY").await.unwrap();
          match service.as_str() {
            "shell:getprop ro.product.model" => stream.write_all(b"Pixel 7\n").await.unwrap(),
            "shell:echo-back" => {
              let mut heard = [0u8; 4];
              stream.read_exact(&mut heard).await.unwrap();
              stream.write_all(&heard).await.unwrap();
              stream.write_all(b"!").await.unwrap();
            },
            other => stream.write_all(other.as_bytes()).await.unwrap(),
          }
        });
      }
    });
    AndroidDevicesOptions {
      host: None,
      port: Some(port),
      omit_driver_install: None,
    }
  }

  #[tokio::test]
  async fn devices_are_the_usable_ones_and_keep_their_identity() {
    let options = adb("emulator-5554\tdevice\nR58M123ABC\tunauthorized\n").await;
    let android = Android::new();
    let devices = android.devices(options.clone()).await.unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].serial(), "emulator-5554");
    assert_eq!(devices[0].model(), "Pixel 7");

    let again = android.devices(options.clone()).await.unwrap();
    assert!(devices[0] == again[0], "a listed device is the same object");

    devices[0].close().await;
    let after = android.devices(options).await.unwrap();
    assert!(devices[0] != after[0], "a closed device is listed anew");
    assert!(devices[0].is_closed() && !after[0].is_closed());
  }

  #[tokio::test]
  async fn shell_and_screenshot_return_what_the_device_wrote() {
    let android = Android::new();
    let device = android
      .devices(adb("emulator-5554\tdevice\n").await)
      .await
      .unwrap()
      .remove(0);
    assert_eq!(device.shell("id -u").await.unwrap(), b"shell:id -u");

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("screen.png");
    let image = device.screenshot(Some(&path)).await.unwrap();
    assert_eq!(image, b"shell:screencap -p");
    assert_eq!(std::fs::read(&path).unwrap(), image);
  }

  #[tokio::test]
  async fn a_closed_device_refuses_every_call() {
    let android = Android::new();
    let device = android
      .devices(adb("emulator-5554\tdevice\n").await)
      .await
      .unwrap()
      .remove(0);
    device.close().await;
    for error in [
      device.shell("true").await.unwrap_err(),
      device.screenshot(None).await.unwrap_err(),
      device
        .push(AndroidFile::Bytes(vec![1]), "/data/local/tmp/x", None)
        .await
        .unwrap_err(),
      device.install_apk(AndroidFile::Bytes(vec![1]), None).await.unwrap_err(),
      device.open("shell:true").await.err().unwrap(),
    ] {
      assert!(error.to_string().contains("Device is closed"), "{error}");
    }
  }

  #[tokio::test]
  async fn a_file_that_cannot_be_read_names_itself() {
    let android = Android::new();
    let device = android
      .devices(adb("emulator-5554\tdevice\n").await)
      .await
      .unwrap()
      .remove(0);
    let missing = std::env::temp_dir().join("ferridriver-no-such-file-sashoush.apk");
    let error = device
      .install_apk(AndroidFile::Path(missing.clone()), None)
      .await
      .unwrap_err()
      .to_string();
    assert!(error.contains(&missing.display().to_string()), "{error}");
  }

  #[tokio::test]
  async fn a_socket_carries_bytes_both_ways_until_it_closes() {
    let android = Android::new();
    let device = android
      .devices(adb("emulator-5554\tdevice\n").await)
      .await
      .unwrap()
      .remove(0);
    let socket = device.open("shell:echo-back").await.unwrap();
    socket.write(b"ping").await.unwrap();
    let mut heard = Vec::new();
    while let Some(chunk) = socket.next_data().await {
      heard.extend(chunk);
    }
    assert_eq!(heard, b"ping!");
    socket.close().await;
    assert!(
      socket
        .write(b"late")
        .await
        .unwrap_err()
        .to_string()
        .contains("Socket is closed")
    );
  }
}

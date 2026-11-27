//! Playwright's `_android`: the devices behind an ADB server, driven
//! through that server's own protocol with no `adb` binary in between.
//! `client/android.ts` for the shape, `server/android/android.ts` for what
//! each call does.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::driver::Driver;
use super::wire::AdbServer;
use crate::error::{FerriError, Result};

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
  pub(super) async fn load(self) -> Result<Vec<u8>> {
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
pub(super) const COMMAND_LINE_FILE: &str = "/data/local/tmp/chrome-command-line";

/// Playwright's default for a call that waits, `DEFAULT_PLAYWRIGHT_TIMEOUT`.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// How long a launched browser has to open its `DevTools` socket.
pub(super) const LAUNCH_TIMEOUT_MS: u64 = 30_000;

pub(super) static LAUNCHES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How often a device is asked for the web views it has, as upstream.
pub(super) const WEB_VIEW_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// What a device tells its listeners. Playwright: the `webview` and
/// `close` events of `AndroidDevice`.
#[derive(Clone)]
pub enum AndroidDeviceEvent {
  /// A web view appeared on the device.
  WebView(AndroidWebView),
  /// The device was closed.
  Close,
}

/// Playwright: `androidDevice.webView(selector)`. The first that is set
/// decides.
#[derive(Debug, Clone, Default)]
pub struct AndroidWebViewSelector {
  /// The package the web view runs in.
  pub pkg: Option<String>,
  /// The name of its `DevTools` socket.
  pub socket_name: Option<String>,
}

type Registry = Arc<std::sync::Mutex<Vec<AndroidDevice>>>;

/// Playwright: `Android`, reached there as `playwright._android`.
#[derive(Clone)]
pub struct Android {
  /// The devices handed out and not yet closed. A device listed again is
  /// the same object, as in Playwright's `Android._devices`.
  devices: Registry,
  /// What a device made from here on starts with as its default timeout.
  default_timeout: Arc<std::sync::atomic::AtomicU64>,
}

impl Default for Android {
  fn default() -> Self {
    Self {
      devices: Registry::default(),
      default_timeout: Arc::new(std::sync::atomic::AtomicU64::new(DEFAULT_TIMEOUT_MS)),
    }
  }
}

/// Why `android.connect` and `android.launchServer` are refused.
const NO_PLAYWRIGHT_SERVER: &str = concat!(
  "the Android server Playwright launches and connects to speaks Playwright's own client/server protocol, ",
  "which ferridriver does not implement; reach a device on another machine through its ADB server with ",
  "android.devices({ host, port })"
);

impl Android {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  /// Playwright: `android.setDefaultTimeout(timeout: number)`. The
  /// default timeout of every device listed from now on; a device
  /// already handed out keeps its own.
  pub fn set_default_timeout(&self, timeout_ms: u64) {
    self.default_timeout.store(timeout_ms, Ordering::Relaxed);
  }

  /// Playwright: `android.connect(endpoint, options?): Promise<AndroidDevice>`.
  ///
  /// # Errors
  ///
  /// Always: the endpoint is a Playwright server, whose protocol is not
  /// spoken here.
  pub fn connect(_endpoint: &str) -> Result<AndroidDevice> {
    Err(FerriError::unsupported(NO_PLAYWRIGHT_SERVER))
  }

  /// Playwright: `android.launchServer(options?): Promise<BrowserServer>`.
  ///
  /// # Errors
  ///
  /// Always, for the reason [`Self::connect`] gives.
  pub fn launch_server() -> Result<()> {
    Err(FerriError::unsupported(NO_PLAYWRIGHT_SERVER))
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
          default_timeout: std::sync::atomic::AtomicU64::new(self.default_timeout.load(Ordering::Relaxed)),
          web_views: std::sync::Mutex::new(Vec::new()),
          events: tokio::sync::broadcast::channel(64).0,
        }),
      };
      // Upstream lists the web views once before it hands the device
      // out, then keeps asking.
      device.refresh_web_views().await?;
      device.watch_web_views();
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

pub(super) struct DeviceInner {
  pub(super) server: AdbServer,
  pub(super) serial: String,
  pub(super) model: String,
  pub(super) closed: AtomicBool,
  pub(super) registry: std::sync::Weak<std::sync::Mutex<Vec<AndroidDevice>>>,
  /// The browsers this device launched and the local port each one's
  /// `DevTools` socket is forwarded to, for [`AndroidDevice::close`].
  pub(super) browsers: tokio::sync::Mutex<Vec<(crate::context::ContextRef, u16)>>,
  pub(super) omit_driver_install: bool,
  /// The driver, installed and started by the first call that needs it.
  pub(super) driver: tokio::sync::Mutex<Option<Arc<Driver>>>,
  pub(super) default_timeout: std::sync::atomic::AtomicU64,
  pub(super) web_views: std::sync::Mutex<Vec<AndroidWebView>>,
  pub(super) events: tokio::sync::broadcast::Sender<AndroidDeviceEvent>,
}

mod device;
mod handles;
#[cfg(test)]
mod tests;

pub use device::AndroidDevice;
pub use handles::{AndroidInput, AndroidSocket, AndroidWebView};

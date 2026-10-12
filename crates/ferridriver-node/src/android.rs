//! NAPI binding for [`ferridriver::android`].
//!
//! Mirrors Playwright's `client/android.ts`: the module's `_android`
//! object with `devices(options?)`, and on an `AndroidDevice` `serial()`,
//! `model()`, `shell(command)`, `screenshot(options?)`,
//! `push(file, path, options?)`, `installApk(file, options?)` and
//! `close()`.

use napi::Result;
use napi::bindgen_prelude::{Buffer, Either, PromiseRaw};
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

  /// Playwright: `androidDevice.close(): Promise<void>`.
  #[napi(ts_return_type = "Promise<void>")]
  pub fn close<'env>(&self, env: &'env napi::Env) -> Result<PromiseRaw<'env, ()>> {
    self.inner.close();
    env.spawn_future(async { Ok(()) })
  }
}

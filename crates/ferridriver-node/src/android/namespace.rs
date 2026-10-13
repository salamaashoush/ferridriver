//! The module's `_android`: Playwright's `playwright._android` as a
//! napi namespace, so the generated loader names the export and a
//! Node ES module can import it by name.

// `#[napi]` exports these to JS but clippy's reachability check only
// follows Rust call graphs, so it flags the entry points as dead. Same
// module-level exemption every other NAPI entry-point module carries.
#![allow(dead_code)]

use napi::Result;
use napi::bindgen_prelude::{AsyncBlock, AsyncBlockBuilder, Env};
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

/// The one `Android` behind the module's `_android`, as Playwright's
/// `playwright._android` is one object.
static ANDROID: std::sync::LazyLock<ferridriver::android::Android> = std::sync::LazyLock::new(Default::default);

/// Playwright: `android.setDefaultTimeout(timeout: number): void`. The
/// default timeout of every device listed from now on.
#[napi(namespace = "_android", js_name = "setDefaultTimeout")]
pub fn android_set_default_timeout(timeout: f64) {
  ANDROID.set_default_timeout(crate::types::f64_to_u64(timeout));
}

/// Playwright: `android.connect(endpoint, options?): Promise<AndroidDevice>`.
/// Rejects: the endpoint is a Playwright server, whose protocol
/// ferridriver does not speak. Reach a remote device through its ADB
/// server with `devices({ host, port })`.
///
/// # Errors
///
/// When the promise cannot be created. The refusal itself rejects it.
#[napi(
  namespace = "_android",
  js_name = "connect",
  ts_args_type = "endpoint: string, options?: object",
  ts_return_type = "Promise<AndroidDevice>"
)]
pub fn android_connect(env: Env, endpoint: String) -> Result<AsyncBlock<()>> {
  AsyncBlockBuilder::new(async move {
    ferridriver::android::Android::connect(&endpoint)
      .map(|_| ())
      .into_napi()
  })
  .build(&env)
}

/// Playwright: `android.launchServer(options?): Promise<BrowserServer>`.
/// Rejects, for the reason `connect` gives.
///
/// # Errors
///
/// When the promise cannot be created. The refusal itself rejects it.
#[napi(
  namespace = "_android",
  js_name = "launchServer",
  ts_args_type = "options?: object",
  ts_return_type = "Promise<never>"
)]
pub fn android_launch_server(env: Env) -> Result<AsyncBlock<()>> {
  AsyncBlockBuilder::new(async move { ferridriver::android::Android::launch_server().into_napi() }).build(&env)
}

/// Playwright: `android.devices(options?: { host?, port?,
/// omitDriverInstall? }): Promise<AndroidDevice[]>`. The usable devices
/// of the ADB server at `host:port`, `127.0.0.1:5037` when absent.
///
/// # Errors
///
/// When the port is out of range or the ADB server cannot be reached.
#[napi(
  namespace = "_android",
  js_name = "devices",
  ts_return_type = "Promise<Array<AndroidDevice>>"
)]
pub async fn android_devices(
  options: Option<AndroidDevicesOptions>,
) -> Result<Vec<crate::identity::AndroidDeviceObject>> {
  let options = options.unwrap_or_default();
  let port = options
    .port
    .map(|port| u16::try_from(port).map_err(|_| napi::Error::from_reason(format!("port {port} is out of range"))))
    .transpose()?;
  let devices = ANDROID
    .devices(ferridriver::android::AndroidDevicesOptions {
      host: options.host,
      port,
      omit_driver_install: options.omit_driver_install,
    })
    .await
    .into_napi()?;
  Ok(devices.into_iter().map(crate::identity::AndroidDeviceObject).collect())
}

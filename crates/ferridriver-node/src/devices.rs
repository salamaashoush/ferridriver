//! `devices` — Playwright's device registry as a module-level value.
//!
//! Playwright exports `devices` as an OBJECT, not a factory
//! (`test.mjs`: `export const devices = playwright.devices`), so a
//! suite writes `{ ...devices['iPhone 15'] }`. napi-rs only derives a
//! module-level value from a Rust `const`, and a 207-entry map is not
//! const-evaluable — so the same registration the `#[napi]` const
//! expansion performs is written out here, with the value built when the
//! addon loads.
//!
//! The object is parsed from the vendored source rather than assembled
//! field by field, so it carries exactly the keys upstream ships and a
//! spread behaves as it does there. Its TypeScript declaration lives in
//! `types.d.ts`, the package's type entry, because the generated
//! `index.d.ts` only describes what napi-rs itself emitted.

/// Build the `devices` object for the loading addon.
///
/// # Safety
///
/// `env` must be the live `napi_env` napi-rs passes at module
/// registration.
unsafe fn devices_value(env: napi::sys::napi_env) -> napi::Result<napi::sys::napi_value> {
  let table: serde_json::Value = serde_json::from_str(ferridriver::devices::SOURCE)
    .map_err(|e| napi::Error::from_reason(format!("device registry is not valid JSON: {e}")))?;
  unsafe { napi::bindgen_prelude::ToNapiValue::to_napi_value(env, table) }
}

#[cfg(not(target_family = "wasm"))]
napi::ctor::declarative::ctor! {
  #[ctor(unsafe)]
  fn register_devices() {
    napi::bindgen_prelude::register_module_export(None, "devices\0", devices_value);
  }
}

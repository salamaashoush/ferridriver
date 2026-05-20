// See crates/ferridriver/build.rs for the rationale — emits
// `cfg(webkit_backend)` so the test framework's webkit-specific code
// paths compile on macOS and Linux but not on other targets.
fn main() {
  println!("cargo::rustc-check-cfg=cfg(webkit_backend)");
  let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
  if matches!(target_os.as_str(), "macos" | "linux") {
    println!("cargo::rustc-cfg=webkit_backend");
  }
}

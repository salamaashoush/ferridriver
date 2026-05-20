// Emits a single workspace-level `cfg(webkit_backend)` predicate that
// expands to `any(target_os = "macos", target_os = "linux")`. The same
// emission lives in every crate that gates dispatch by WebKit
// availability; keeping it inline (rather than behind a tiny helper
// crate or the `cfg_aliases` crate) makes each crate's build process
// self-contained and avoids an external build-dep just for four
// trivial lines.
fn main() {
  println!("cargo::rustc-check-cfg=cfg(webkit_backend)");
  let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
  if matches!(target_os.as_str(), "macos" | "linux") {
    println!("cargo::rustc-cfg=webkit_backend");
  }
}

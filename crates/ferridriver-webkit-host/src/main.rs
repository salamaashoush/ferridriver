//! `ferridriver-webkit-host` — cross-platform `WebKit` host subprocess.
//!
//! - **macOS**: the binary is built from `src/macos/host.m` (Obj-C,
//!   drives `WKWebView`). The Rust `main` below tail-calls into
//!   `fd_webkit_host_main` — an `extern "C"` symbol declared by the
//!   `.m` file and linked in by `build.rs`.
//! - **Linux**: the binary is the Rust [`linux`] module — webkit6
//!   driving `WebKitWebView` on a GTK4 main loop.
//!
//! Both platforms speak the same wire protocol (see
//! [`ferridriver_webkit_wire`]) and the same shared JS shims
//! (`crates/ferridriver-webkit-wire/shared_js/*.js`).

#[cfg(target_os = "macos")]
unsafe extern "C" {
  /// Defined in `src/macos/host.m`. Takes the IPC socket fd (passed by
  /// the parent as fd 3), runs the Obj-C event loop, never returns.
  fn fd_webkit_host_main(fd: std::ffi::c_int) -> !;
}

#[cfg(target_os = "macos")]
fn main() -> ! {
  // SAFETY: `fd_webkit_host_main` is a thin C ABI entry that the Obj-C
  // host defines as `__attribute__((noreturn))`. It takes ownership of
  // the IPC socket fd we pass and runs the NSRunLoop until the parent
  // sends `Op::Shutdown`, at which point it `_exit(0)`s.
  #[allow(unsafe_code)]
  unsafe {
    fd_webkit_host_main(3)
  }
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
  linux::run()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn main() -> std::process::ExitCode {
  eprintln!(
    "ferridriver-webkit-host is only built for macOS and Linux. On Windows/BSD the WebKit backend is currently unavailable; use cdp-pipe / cdp-raw / bidi instead."
  );
  std::process::ExitCode::from(64) // EX_USAGE
}

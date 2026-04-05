fn main() {
  // Only compile WebKit ObjC host binary when targeting macOS.
  // build.rs #[cfg] checks the HOST platform, not the target.
  // Use CARGO_CFG_TARGET_OS to check the actual cross-compilation target.
  let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
  if target_os != "macos" {
    return;
  }

  let Ok(out_dir) = std::env::var("OUT_DIR") else {
    panic!("OUT_DIR environment variable not set -- this build script must be run by Cargo");
  };

  // Compile host.m and host_main.c into object files via the cc crate.
  // We use cc for correct compiler detection, flag handling, and
  // cross-compilation support.

  // host.m -> host.o
  cc::Build::new()
    .file("src/backend/webkit/host.m")
    .flag("-fobjc-arc")
    .flag("-fmodules")
    .flag("-Wno-deprecated-declarations")
    .cargo_warnings(false)
    .compile("webkit_host_obj");

  // host_main.c -> host_main.o
  cc::Build::new()
    .file("src/backend/webkit/host_main.c")
    .cargo_warnings(false)
    .compile("webkit_host_main_obj");

  // Link the two object files into a standalone executable.
  // We use the cc crate's detected compiler for the final link step.
  let host_obj = format!("{out_dir}/libwebkit_host_obj.a");
  let main_obj = format!("{out_dir}/libwebkit_host_main_obj.a");
  let host_bin = format!("{out_dir}/fd_webkit_host");

  let tool = cc::Build::new().get_compiler();
  let Ok(status) = tool
    .to_command()
    .args([&host_obj, &main_obj])
    .arg("-o")
    .arg(&host_bin)
    .args(["-framework", "Cocoa"])
    .args(["-framework", "WebKit"])
    .args(["-framework", "CoreFoundation"])
    .status()
  else {
    panic!("Failed to run linker for webkit host binary");
  };

  assert!(status.success(), "Failed to link webkit host binary");

  // ── Copy fd_webkit_host to discoverable locations ─────────────────────
  //
  // 1. target/{profile}/fd_webkit_host  (sibling to CLI binary)
  // 2. ~/.cache/ferridriver/fd_webkit_host  (survives cargo clean)
  //
  // Both copies are best-effort: warnings on failure, not panics.

  // Derive target/{profile}/ from OUT_DIR.
  // OUT_DIR layout: <target>/<profile>/build/<crate>-<hash>/out
  // Walking up 3 parents from OUT_DIR gives <target>/<profile>/
  let out_path = std::path::Path::new(&out_dir);
  if let Some(profile_dir) = out_path
    .parent()
    .and_then(|p| p.parent())
    .and_then(|p| p.parent())
  {
    let dest = profile_dir.join("fd_webkit_host");
    if let Err(e) = std::fs::copy(&host_bin, &dest) {
      println!("cargo:warning=Could not copy fd_webkit_host to {}: {e}", dest.display());
    }
  }

  // Copy to ~/.cache/ferridriver/ (survives cargo clean, works for cargo install)
  if let Some(home) = std::env::var_os("HOME") {
    let cache_dir = std::path::Path::new(&home).join(".cache").join("ferridriver");
    match std::fs::create_dir_all(&cache_dir) {
      Ok(()) => {
        let dest = cache_dir.join("fd_webkit_host");
        if let Err(e) = std::fs::copy(&host_bin, &dest) {
          println!("cargo:warning=Could not copy fd_webkit_host to {}: {e}", dest.display());
        }
      },
      Err(e) => {
        println!("cargo:warning=Could not create cache dir {}: {e}", cache_dir.display());
      },
    }
  }

  // Don't link the static libs into the Rust library -- they're only
  // used to produce the standalone binary above. Clear the link flags
  // that cc::Build emits by default.
  // (The Rust library doesn't call fd_webkit_host_main anymore.)

  println!("cargo:rerun-if-changed=src/backend/webkit/host.m");
  println!("cargo:rerun-if-changed=src/backend/webkit/host_main.c");
}

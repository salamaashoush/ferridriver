fn main() {
  napi_build::setup();

  // On macOS, copy fd_webkit_host alongside the NAPI crate source
  // so npm packaging can include it.
  let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
  if target_os != "macos" {
    return;
  }

  let Ok(out_dir) = std::env::var("OUT_DIR") else {
    return;
  };

  // Derive target/{profile}/ from OUT_DIR (same layout as ferridriver/build.rs).
  let out_path = std::path::Path::new(&out_dir);
  let Some(profile_dir) = out_path.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) else {
    return;
  };

  // ferridriver's build.rs copies fd_webkit_host to target/{profile}/.
  // Copy it to the NAPI crate source directory for npm packaging.
  let host_in_profile = profile_dir.join("fd_webkit_host");
  let napi_crate_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
  if !napi_crate_dir.is_empty() && host_in_profile.exists() {
    let dest = std::path::Path::new(&napi_crate_dir).join("fd_webkit_host");
    if let Err(e) = std::fs::copy(&host_in_profile, &dest) {
      println!("cargo:warning=Could not copy fd_webkit_host to {}: {e}", dest.display());
    }
  }
}

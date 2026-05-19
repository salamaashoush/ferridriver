#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Default (no `set_bdd_script_caps` call — the macro/harness path with
//! no config): the BDD step VM is locked down — `process.env` is
//! empty. The step file asserts this at module load; if a cap leaked,
//! loading would throw. Browser-free.
//!
//! Own test binary so the process-global cap `OnceLock` stays unset.

use std::path::PathBuf;

use ferridriver_bdd::js::JsBddSession;

fn scratch(name: &str, src: &str) -> PathBuf {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_nanos())
    .unwrap_or(0);
  let dir = std::env::temp_dir().join(format!("ferri_bddcaps_off_{nanos}"));
  std::fs::create_dir_all(dir.join("steps")).expect("mkdir");
  std::fs::write(dir.join("steps").join(name), src).expect("write step");
  dir
}

#[tokio::test(flavor = "multi_thread")]
async fn bdd_step_vm_is_locked_down_by_default() {
  // No set_bdd_script_caps: even with a real env var present it must
  // NOT appear (locked-down default).
  unsafe {
    std::env::set_var("FERRIDRIVER_BDD_CAPTEST_OFF", "leak");
  }
  let dir = scratch(
    "cap.js",
    "if (Object.keys(process.env).length !== 0) \
       throw new Error('env leaked when caps unset: ' + JSON.stringify(process.env)); \
     Given('a no-op', function () {});",
  );

  let res = JsBddSession::from_globs(&["steps/**/*.js".to_string()], &dir).await;
  let _ = std::fs::remove_dir_all(&dir);
  assert!(
    res.is_ok(),
    "locked-down default should load (empty env); got: {:?}",
    res.err()
  );
}

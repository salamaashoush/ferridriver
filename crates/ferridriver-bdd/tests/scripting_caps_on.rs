#![allow(clippy::expect_used, clippy::unwrap_used)]
//! The `[scripting]` env allow-list is threaded into the BDD step VM
//! via `set_bdd_script_caps`, exactly like the MCP server. The step
//! file asserts at module load that the cap'd env var is visible — if
//! the wiring were missing, loading the bundle would throw and
//! `from_globs` would error. Browser-free (only bundles + evaluates
//! the step module; no scenario run).
//!
//! Own test binary so the process-global `OnceLock` cap is pristine.

use std::path::PathBuf;

use ferridriver_bdd::js::JsBddSession;

fn scratch(name: &str, src: &str) -> PathBuf {
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_nanos())
    .unwrap_or(0);
  let dir = std::env::temp_dir().join(format!("ferri_bddcaps_on_{nanos}"));
  std::fs::create_dir_all(dir.join("steps")).expect("mkdir");
  std::fs::write(dir.join("steps").join(name), src).expect("write step");
  dir
}

#[tokio::test(flavor = "multi_thread")]
async fn scripting_caps_reach_the_bdd_step_vm() {
  // Set the real env var first; `ScriptCaps::resolve` only captures
  // allow-listed names that are actually present.
  unsafe {
    std::env::set_var("FERRIDRIVER_BDD_CAPTEST", "yes");
  }
  ferridriver_bdd::js::set_bdd_script_caps(ferridriver_script::ScriptCaps::resolve(&[
    "FERRIDRIVER_BDD_CAPTEST".to_string()
  ]));

  let dir = scratch(
    "cap.js",
    "if (process.env.FERRIDRIVER_BDD_CAPTEST !== 'yes') \
       throw new Error('env cap NOT threaded: ' + JSON.stringify(process.env)); \
     Given('a no-op', function () {});",
  );

  let res = JsBddSession::from_globs(&["steps/**/*.js".to_string()], &dir).await;
  let _ = std::fs::remove_dir_all(&dir);
  assert!(
    res.is_ok(),
    "step bundle should load with caps threaded; got: {:?}",
    res.err()
  );
}

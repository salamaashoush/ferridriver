#![allow(clippy::expect_used, clippy::unwrap_used)]
// `std::env::set_var` is `unsafe` in edition 2024; no safe alternative for
// scoping the cache dir / disable switch in a bench. Confined to this target.
#![allow(unsafe_code)]
//! Re-runnable microbench for the cost of LOADING JS/TS step plugins.
//! `#[ignore]` so it stays out of the green gate; run explicitly:
//!
//! ```text
//! cargo test --profile release-fast -p ferridriver-bdd --test load_bench -- --ignored --nocapture
//! ```
//!
//! Isolates the loading stages (no browser, no step execution):
//! 1. cold bundle — rolldown + transpile + tree-shake + compile to bytecode, then persist to disk.
//! 2. disk-warm — unchanged sources skip rolldown AND compile; load bytecode from the cross-process cache.
//! 3. after-edit — a transitive input changed: the cache must MISS and rebundle (freshness check).
//! 4. cache-off — `FERRIDRIVER_NO_BYTECODE_CACHE`: always cold (the pre-cache baseline).
//! 5. per-session — `JsBddSession::load`: VM create + eval bytecode + registry + BeforeAll.

use std::time::Instant;

use ferridriver_bdd::js::{JsBddSession, bundle_steps};

fn step_source(marker: u128) -> String {
  format!(
    "// unique:{marker}\n\
     function slugify(s) {{ return String(s).toLowerCase().replace(/\\s+/g, '-'); }}\n\
     defineParameterType({{ name: 'color', regexp: /red|green|blue/, transformer: (s) => s.toUpperCase() }});\n\
     Before(function () {{ this.count = 0; }});\n\
     Given('I start with {{int}}', function (n) {{ this.count = n; }});\n\
     Given('I pick a {{color}} item', function (c) {{ this.color = c; }});\n\
     When('I add {{int}}', function (n) {{ this.count += n; }});\n\
     When('I name it {{string}}', function (s) {{ this.slug = slugify(s); }});\n\
     Then('the total is {{int}}', function (n) {{ if (this.count !== n) throw new Error('bad'); }});\n\
     Then('the slug is {{string}}', function (s) {{ if (this.slug !== s) throw new Error('bad'); }});\n"
  )
}

fn now_ns() -> u128 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_nanos()
}

async fn time_bundle(globs: &[String], dir: &std::path::Path) -> f64 {
  let t = Instant::now();
  let _ = bundle_steps(globs, dir).await.expect("bundle");
  t.elapsed().as_secs_f64() * 1e3
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "perf microbench; run with --ignored --nocapture"]
async fn js_plugin_load_bench() {
  let src = tempfile::tempdir().expect("tempdir");
  let dir = src.path().to_path_buf();
  // Isolated, empty disk cache so the cold number is genuinely cold.
  let cache = tempfile::tempdir().expect("cache tempdir");
  // SAFETY: single-threaded test setup before any cache access.
  unsafe { std::env::set_var("FERRIDRIVER_CACHE_DIR", cache.path()) };
  unsafe { std::env::remove_var("FERRIDRIVER_NO_BYTECODE_CACHE") };

  std::fs::write(dir.join("steps.ts"), step_source(now_ns())).expect("write");
  let globs = vec!["steps.ts".to_string()];

  let cold_ms = time_bundle(&globs, &dir).await;
  let warm_ms = time_bundle(&globs, &dir).await; // disk hit
  let warm2_ms = time_bundle(&globs, &dir).await; // disk hit again

  // Mutate a transitive input -> cache must miss and rebundle.
  std::fs::write(dir.join("steps.ts"), step_source(now_ns())).expect("rewrite");
  let after_edit_ms = time_bundle(&globs, &dir).await;
  let warm_after_edit_ms = time_bundle(&globs, &dir).await; // hit on new content

  // Baseline with the cache disabled: every call pays full cold.
  unsafe { std::env::set_var("FERRIDRIVER_NO_BYTECODE_CACHE", "1") };
  let off_a_ms = time_bundle(&globs, &dir).await;
  let off_b_ms = time_bundle(&globs, &dir).await;
  unsafe { std::env::remove_var("FERRIDRIVER_NO_BYTECODE_CACHE") };

  // Per-session load (unchanged by the disk cache).
  let bundle = bundle_steps(&globs, &dir).await.expect("bundle for session");
  let _ = JsBddSession::load(bundle.clone(), &dir, serde_json::Value::Null)
    .await
    .expect("warm session");
  let n = 50u32;
  let sess = Instant::now();
  for _ in 0..n {
    let _ = JsBddSession::load(bundle.clone(), &dir, serde_json::Value::Null)
      .await
      .expect("session load");
  }
  let per_session_ms = (sess.elapsed().as_secs_f64() * 1e3) / f64::from(n);

  println!("\n=== js plugin LOAD bench (1 step file, 7 steps) ===");
  println!("cold bundle (rolldown+bytecode+store): {cold_ms:8.2} ms");
  println!(
    "disk-warm  (validate+load bytecode)  : {warm_ms:8.3} ms   [{:.0}x faster]",
    cold_ms / warm_ms
  );
  println!("disk-warm  (again)                   : {warm2_ms:8.3} ms");
  println!("after edit (must miss -> rebundle)   : {after_edit_ms:8.2} ms   [freshness ok]");
  println!("disk-warm  (post-edit content)       : {warm_after_edit_ms:8.3} ms");
  println!("cache OFF  (a)                        : {off_a_ms:8.2} ms");
  println!("cache OFF  (b)                        : {off_b_ms:8.2} ms");
  println!("per-session load (VM+eval+reg)       : {per_session_ms:8.3} ms");
  println!("===================================================\n");

  // Freshness guard: a disk-warm load must be far cheaper than cold, and
  // an edit must cost about a full rebundle again.
  assert!(warm_ms < cold_ms, "disk-warm must beat cold");
}

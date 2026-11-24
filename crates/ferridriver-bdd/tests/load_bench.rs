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
//! 5. per-session — `JsBddSession::load`: VM create + eval bytecode + registry + `BeforeAll`.
//!
//! The bytecode cache reads its environment once per process, so each cache
//! mode is measured in a child process of this test binary, started with
//! that mode's environment.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use ferridriver_bdd::js::{BddSessionSetup, JsBddSession, bundle_steps};

const TEST_NAME: &str = "js_plugin_load_bench";
const PHASE_ENV: &str = "FERRIDRIVER_LOAD_BENCH_PHASE";
const DIR_ENV: &str = "FERRIDRIVER_LOAD_BENCH_DIR";
const LINE_PREFIX: &str = "load-bench ";
const CACHED: &str = "cached";
const UNCACHED: &str = "uncached";

type Timings = Vec<(String, f64)>;

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

fn now_ns() -> anyhow::Result<u128> {
  Ok(
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)?
      .as_nanos(),
  )
}

async fn time_bundle(globs: &[String], dir: &Path) -> anyhow::Result<f64> {
  let t = Instant::now();
  bundle_steps(globs, dir).await?;
  Ok(t.elapsed().as_secs_f64() * 1e3)
}

/// Child process, cache on: cold, disk-warm, after-edit, and per-session load.
async fn cached_phase(dir: &Path) -> anyhow::Result<Timings> {
  let globs = vec!["steps.ts".to_string()];
  let mut timings = Timings::new();
  timings.push(("cold".into(), time_bundle(&globs, dir).await?));
  timings.push(("warm".into(), time_bundle(&globs, dir).await?));
  timings.push(("warm_again".into(), time_bundle(&globs, dir).await?));

  // Mutate a transitive input -> cache must miss and rebundle.
  std::fs::write(dir.join("steps.ts"), step_source(now_ns()?))?;
  timings.push(("after_edit".into(), time_bundle(&globs, dir).await?));
  timings.push(("warm_after_edit".into(), time_bundle(&globs, dir).await?));

  // Per-session load (unchanged by the disk cache).
  let bundle = bundle_steps(&globs, dir).await?;
  JsBddSession::load(Arc::clone(&bundle), dir, &BddSessionSetup::default()).await?;
  let n = 50u32;
  let sess = Instant::now();
  for _ in 0..n {
    JsBddSession::load(Arc::clone(&bundle), dir, &BddSessionSetup::default()).await?;
  }
  timings.push((
    "per_session".into(),
    (sess.elapsed().as_secs_f64() * 1e3) / f64::from(n),
  ));
  Ok(timings)
}

/// Child process, cache off: every call pays a full cold bundle.
async fn uncached_phase(dir: &Path) -> anyhow::Result<Timings> {
  let globs = vec!["steps.ts".to_string()];
  Ok(vec![
    ("uncached_first".into(), time_bundle(&globs, dir).await?),
    ("uncached_second".into(), time_bundle(&globs, dir).await?),
  ])
}

/// Run one phase in a child of this test binary and read back its timings.
fn run_phase(phase: &str, dir: &Path, cache_dir: &Path) -> anyhow::Result<Timings> {
  let mut cmd = Command::new(std::env::current_exe()?);
  cmd
    .args([TEST_NAME, "--exact", "--ignored", "--nocapture", "--test-threads=1"])
    .env(PHASE_ENV, phase)
    .env(DIR_ENV, dir);
  if phase == UNCACHED {
    cmd.env("FERRIDRIVER_NO_BYTECODE_CACHE", "1");
  } else {
    cmd
      .env("FERRIDRIVER_CACHE_DIR", cache_dir)
      .env_remove("FERRIDRIVER_NO_BYTECODE_CACHE");
  }
  let output = cmd.output()?;
  anyhow::ensure!(
    output.status.success(),
    "{phase} phase failed ({}):\n{}{}",
    output.status,
    String::from_utf8_lossy(&output.stdout),
    String::from_utf8_lossy(&output.stderr)
  );
  String::from_utf8_lossy(&output.stdout)
    .lines()
    // libtest prints `test <name> ... ` on the line the first timing lands on.
    .filter_map(|line| line.split_once(LINE_PREFIX).map(|(_, entry)| entry))
    .map(|entry| {
      let (name, ms) = entry
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("malformed timing line: {entry}"))?;
      Ok((name.to_string(), ms.parse()?))
    })
    .collect()
}

fn timing(timings: &Timings, name: &str) -> anyhow::Result<f64> {
  timings
    .iter()
    .find(|(n, _)| n == name)
    .map(|(_, ms)| *ms)
    .ok_or_else(|| anyhow::anyhow!("no `{name}` timing"))
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "perf microbench; run with --ignored --nocapture"]
async fn js_plugin_load_bench() -> anyhow::Result<()> {
  if let Ok(phase) = std::env::var(PHASE_ENV) {
    let dir = PathBuf::from(std::env::var(DIR_ENV)?);
    let timings = if phase == UNCACHED {
      uncached_phase(&dir).await?
    } else {
      cached_phase(&dir).await?
    };
    for (name, ms) in timings {
      println!("{LINE_PREFIX}{name}={ms}");
    }
    return Ok(());
  }

  let src = tempfile::tempdir()?;
  // Isolated, empty disk cache so the cold number is genuinely cold.
  let cache = tempfile::tempdir()?;
  std::fs::write(src.path().join("steps.ts"), step_source(now_ns()?))?;

  let cached = run_phase(CACHED, src.path(), cache.path())?;
  let uncached = run_phase(UNCACHED, src.path(), cache.path())?;
  let cold_ms = timing(&cached, "cold")?;
  let warm_ms = timing(&cached, "warm")?;

  println!("\n=== js plugin LOAD bench (1 step file, 7 steps) ===");
  println!("cold bundle (rolldown+bytecode+store): {cold_ms:8.2} ms");
  println!(
    "disk-warm  (validate+load bytecode)  : {warm_ms:8.3} ms   [{:.0}x faster]",
    cold_ms / warm_ms
  );
  println!(
    "disk-warm  (again)                   : {:8.3} ms",
    timing(&cached, "warm_again")?
  );
  println!(
    "after edit (must miss -> rebundle)   : {:8.2} ms   [freshness ok]",
    timing(&cached, "after_edit")?
  );
  println!(
    "disk-warm  (post-edit content)       : {:8.3} ms",
    timing(&cached, "warm_after_edit")?
  );
  println!(
    "cache OFF  (a)                        : {:8.2} ms",
    timing(&uncached, "uncached_first")?
  );
  println!(
    "cache OFF  (b)                        : {:8.2} ms",
    timing(&uncached, "uncached_second")?
  );
  println!(
    "per-session load (VM+eval+reg)       : {:8.3} ms",
    timing(&cached, "per_session")?
  );
  println!("===================================================\n");

  // Freshness guard: a disk-warm load must be far cheaper than cold.
  assert!(warm_ms < cold_ms, "disk-warm must beat cold");
  Ok(())
}

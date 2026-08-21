#![allow(clippy::expect_used, clippy::unwrap_used)]
//! What a trace records for the calls a script makes.
//!
//! The viewer is only as useful as the action list behind it: a run whose
//! `setContent`, `evaluate`, mouse and keyboard input and context calls
//! leave no action shows a page changing for no visible reason. This
//! drives a real script through the built binary and reads the recorded
//! actions back with `trace show --json`.
//!
//! Requires a built `ferridriver` binary and a Chromium.
//!
//! `--instance default` is what provisions `page` / `context` / `browser`:
//! a bare `run` launches nothing, so a script that touches them would only
//! report that they are undefined. `--no-inherit` keeps a config in the
//! user's own directories out of the run, which would otherwise decide the
//! backend and headedness of a test that has an opinion about neither.

use std::process::Command;

fn bin() -> String {
  std::env::var("FERRIDRIVER_BIN").unwrap_or_else(|_| {
    let base = format!("{}/../../target", env!("CARGO_MANIFEST_DIR"));
    let debug = format!("{base}/debug/ferridriver");
    if std::path::Path::new(&debug).exists() {
      debug
    } else {
      format!("{base}/release/ferridriver")
    }
  })
}

const SCRIPT: &str = r#"
await context.tracing.start({ snapshots: true });
await page.setContent('<h1>hello</h1><button id="b">go</button>');
await page.title();
await page.content();
await page.evaluate('() => document.title');
await page.$('h1');
await page.locator('#b').click();
await page.mouse.move(5, 5);
await page.mouse.click(5, 5);
await page.keyboard.press('Tab');
await page.keyboard.type('abc');
await page.setViewportSize({ width: 900, height: 700 });
await page.waitForLoadState('load');
await page.screenshot();
await context.addCookies([{ name: 'a', value: 'b', url: 'https://example.com' }]);
await context.cookies();
await context.setOffline(false);
await context.route('**/never', route => route.continue());
await context.unroute('**/never');
await context.tracing.stop({ path: 'trace.zip' });
'done';
"#;

#[test]
fn a_script_records_one_action_per_call_it_makes() {
  let dir = tempfile::tempdir().expect("tempdir");
  std::fs::write(dir.path().join("script.ts"), SCRIPT).expect("write script");

  let run = Command::new(bin())
    .args(["--no-inherit", "run", "--instance", "default", "script.ts"])
    .current_dir(dir.path())
    .output()
    .expect("run script");
  assert!(
    run.status.success(),
    "script failed: {}",
    String::from_utf8_lossy(&run.stderr)
  );

  let shown = Command::new(bin())
    .args(["--no-inherit", "trace", "show", "trace.zip", "--json"])
    .current_dir(dir.path())
    .output()
    .expect("trace show");
  assert!(
    shown.status.success(),
    "trace show failed: {}",
    String::from_utf8_lossy(&shown.stderr)
  );
  let report: serde_json::Value = serde_json::from_slice(&shown.stdout).expect("json");
  let actions = report["contexts"][0]["actions"].as_array().expect("actions");
  let titles: Vec<&str> = actions.iter().filter_map(|action| action["title"].as_str()).collect();

  for expected in [
    "page.setContent",
    "page.title",
    "page.content",
    "page.evaluate",
    "page.$",
    "locator.click",
    "mouse.move",
    "mouse.click",
    "keyboard.press",
    "keyboard.type",
    "page.setViewportSize",
    "page.waitForLoadState",
    "page.screenshot",
    "browserContext.addCookies",
    "browserContext.cookies",
    "browserContext.setOffline",
    "browserContext.route",
    "browserContext.unroute",
  ] {
    assert!(titles.contains(&expected), "no {expected} in {titles:?}");
  }

  // One action per call: a public method reaching another (setContent
  // waits through waitForLoadState internally) must not double-count.
  let set_content = titles.iter().filter(|title| **title == "page.setContent").count();
  assert_eq!(set_content, 1, "{titles:?}");
  let waits = titles.iter().filter(|title| **title == "page.waitForLoadState").count();
  assert_eq!(
    waits, 1,
    "the internal wait inside setContent is not its own action: {titles:?}"
  );

  // Every action is attributed to the line of the script that made it.
  let script = dir.path().join("script.ts");
  let located = actions
    .iter()
    .filter(|action| action["title"].as_str().is_some_and(|title| title.starts_with("page.")))
    .filter(|action| {
      action["stack"][0]["file"]
        .as_str()
        .is_some_and(|file| file.ends_with("script.ts"))
    })
    .count();
  assert!(
    located >= 5,
    "page actions carry no call site from {}: {:#?}",
    script.display(),
    actions
      .iter()
      .map(|action| (action["title"].clone(), action["stack"].clone()))
      .collect::<Vec<_>>()
  );
}

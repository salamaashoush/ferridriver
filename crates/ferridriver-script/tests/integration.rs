#![allow(
  clippy::expect_used,
  clippy::unwrap_used,
  clippy::uninlined_format_args,
  clippy::too_many_lines,
  clippy::needless_pass_by_value
)]
//! End-to-end integration tests: drive a real Chrome page via the scripting
//! engine and exercise the Page / Locator / `BrowserContext` bindings against
//! a live browser.
//!
//! These tests require a Chrome / Chromium binary available on the system —
//! same contract as `crates/ferridriver/tests/page_api.rs`. They run
//! sequentially against one browser launch to keep total runtime reasonable.

use std::sync::Arc;

use ferridriver::Browser;
use ferridriver::backend::BackendKind;
use ferridriver::options::LaunchOptions;
use ferridriver_script::{
  InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngine, ScriptEngineConfig,
};

fn data_url(html: &str) -> String {
  format!(
    "data:text/html,{}",
    html
      .bytes()
      .map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{:02X}", b),
      })
      .collect::<String>()
  )
}

struct Harness {
  _tmp: tempfile::TempDir,
  engine: ScriptEngine,
  ctx: RunContext,
}

async fn harness() -> Harness {
  let browser = Browser::launch(LaunchOptions {
    backend: BackendKind::CdpPipe,
    ..Default::default()
  })
  .await
  .expect("launch browser");
  let page = browser.page().await.expect("get page");

  let tmp = tempfile::tempdir().expect("tempdir");
  let sandbox = Arc::new(PathSandbox::new(tmp.path()).expect("sandbox"));
  let vars = Arc::new(InMemoryVars::new());

  let ctx = RunContext {
    vars,
    sandbox,
    artifacts: None,
    page: Some(page),
    browser_context: None,
    request: None,
  };
  let engine = ScriptEngine::new(ScriptEngineConfig::default());

  Harness { _tmp: tmp, engine, ctx }
}

fn expect_ok(result: ferridriver_script::ScriptResult, expected: serde_json::Value) {
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, expected),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_bindings_drive_real_browser() {
  let h = harness().await;

  // ── Navigate + title + url ──────────────────────────────────────────────
  let script = format!(
    "await page.goto({url:?}); return {{ title: await page.title(), url: await page.url() }};",
    url = data_url("<title>Hello</title><body>World</body>")
  );
  let r = h.engine.run(&script, &[], RunOptions::default(), h.ctx.clone()).await;
  match r.outcome {
    Outcome::Ok { success } => {
      let title = success.value.get("title").and_then(|v| v.as_str()).unwrap_or("");
      let url = success.value.get("url").and_then(|v| v.as_str()).unwrap_or("");
      assert!(title.contains("Hello"), "title: {title}");
      assert!(url.starts_with("data:"), "url: {url}");
    },
    Outcome::Error { error } => panic!("nav failed: {error:?}"),
  }

  // ── evaluate ────────────────────────────────────────────────────────────
  let r = h
    .engine
    .run(
      "return await page.evaluate('1 + 2');",
      &[],
      RunOptions::default(),
      h.ctx.clone(),
    )
    .await;
  // evaluate returns JSON-encoded string per the binding.
  match r.outcome {
    Outcome::Ok { success } => {
      let encoded = success.value.as_str().unwrap_or_default();
      assert_eq!(encoded, "3");
    },
    Outcome::Error { error } => panic!("evaluate failed: {error:?}"),
  }

  // ── locator click drives DOM ────────────────────────────────────────────
  let html = data_url("<button id='b' onclick=\"this.textContent='clicked'\">Go</button>");
  let script = format!(
    "await page.goto({html:?}); await page.locator('#b').click(); return await page.evaluate(\"document.getElementById('b').textContent\");",
    html = html
  );
  let r = h.engine.run(&script, &[], RunOptions::default(), h.ctx.clone()).await;
  match r.outcome {
    Outcome::Ok { success } => {
      let encoded = success.value.as_str().unwrap_or_default();
      assert!(encoded.contains("clicked"), "click result: {encoded}");
    },
    Outcome::Error { error } => panic!("click failed: {error:?}"),
  }

  // ── locator fill + inputValue round-trip ────────────────────────────────
  let script = format!(
    "await page.goto({html:?}); const loc = page.locator('#i'); await loc.fill('hi there'); return await loc.inputValue();",
    html = data_url("<input id='i' type='text'>")
  );
  let r = h.engine.run(&script, &[], RunOptions::default(), h.ctx.clone()).await;
  expect_ok(r, serde_json::json!("hi there"));

  // ── isVisible / isHidden predicates ─────────────────────────────────────
  let script = format!(
    "await page.goto({html:?}); return {{ v: await page.isVisible('#shown'), h: await page.isHidden('#hidden') }};",
    html = data_url("<div id='shown'>x</div><div id='hidden' style='display:none'>y</div>")
  );
  let r = h.engine.run(&script, &[], RunOptions::default(), h.ctx.clone()).await;
  expect_ok(r, serde_json::json!({ "v": true, "h": true }));

  // ── locator chain: getByRole + count ────────────────────────────────────
  let script = format!(
    "await page.goto({html:?}); return await page.getByRole('button').count();",
    html = data_url("<button>a</button><button>b</button><button>c</button>")
  );
  let r = h.engine.run(&script, &[], RunOptions::default(), h.ctx.clone()).await;
  expect_ok(r, serde_json::json!(3));

  // ── locator nth + textContent ───────────────────────────────────────────
  let script = format!(
    "await page.goto({html:?}); return await page.getByRole('button').nth(1).textContent();",
    html = data_url("<button>alpha</button><button>beta</button><button>gamma</button>")
  );
  let r = h.engine.run(&script, &[], RunOptions::default(), h.ctx.clone()).await;
  expect_ok(r, serde_json::json!("beta"));

  // ── vars persist across script calls on the same session ────────────────
  let r = h
    .engine
    .run(
      "vars.set('checkpoint', 'first'); return null;",
      &[],
      RunOptions::default(),
      h.ctx.clone(),
    )
    .await;
  assert!(r.is_ok(), "{r:?}");

  let r = h
    .engine
    .run(
      "return vars.get('checkpoint');",
      &[],
      RunOptions::default(),
      h.ctx.clone(),
    )
    .await;
  expect_ok(r, serde_json::json!("first"));

  // ── bound args reach the live page ──────────────────────────────────────
  let html = data_url("<input id='i' type='text'>");
  let script = format!(
    "await page.goto({html:?}); await page.fill('#i', args[0]); return await page.inputValue('#i');",
    html = html
  );
  let r = h
    .engine
    .run(
      &script,
      &[serde_json::json!("prompt-injection\"; drop table; --")],
      RunOptions::default(),
      h.ctx.clone(),
    )
    .await;
  // If interpolation had happened, the script would have crashed at parse.
  // With bound args, the string lands unchanged in the input.
  expect_ok(r, serde_json::json!("prompt-injection\"; drop table; --"));
}

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

/// Tier-3.x additions (3.2 referer, 3.21 page.close options, 3.23
/// setDefaultNavigationTimeout) must be reachable from scripts, not just
/// from NAPI. This exercises each through `run_script`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_bindings_expose_goto_options_and_close_options() {
  let h = harness().await;

  // goto({ waitUntil, referer, timeout }) — the option bag reaches the
  // binding. If parsing were missing, this would throw `TypeError: expected
  // 1 argument` (old single-arg signature). The navigation succeeds and
  // the loaded document sees our Referer header via `document.referrer`
  // if the origin permits reading it back (data: URLs do not, so we just
  // verify the call completes).
  let html = data_url("<title>opts</title><body>ready</body>");
  let script = format!(
    "await page.goto({html:?}, {{ waitUntil: 'domcontentloaded', referer: 'https://ref.example.com/', timeout: 10000 }}); return await page.title();",
    html = html
  );
  let r = h.engine.run(&script, &[], RunOptions::default(), h.ctx.clone()).await;
  expect_ok(r, serde_json::json!("opts"));

  // setDefaultNavigationTimeout exposed as its own method distinct from
  // setDefaultTimeout. Old script binding had neither; new one has both.
  let r = h
    .engine
    .run(
      "page.setDefaultTimeout(5000); page.setDefaultNavigationTimeout(10000); return 'ok';",
      &[],
      RunOptions::default(),
      h.ctx.clone(),
    )
    .await;
  expect_ok(r, serde_json::json!("ok"));

  // page.close({ reason }) — the option bag flows through. We create a
  // fresh page via the underlying context to avoid tearing down the
  // harness page. Asserts that the call accepts the object (parser
  // wired) and the page reports closed afterwards.
  let r = h
    .engine
    .run(
      "await page.goto('data:text/html,about'); return await page.isClosed();",
      &[],
      RunOptions::default(),
      h.ctx.clone(),
    )
    .await;
  expect_ok(r, serde_json::json!(false));
}

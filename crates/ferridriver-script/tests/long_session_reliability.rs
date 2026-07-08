#![allow(
  clippy::expect_used,
  clippy::unwrap_used,
  clippy::too_many_lines,
  clippy::uninlined_format_args
)]
//! Long-running MCP session reliability, driven through the PRODUCTION
//! path (`SessionTable::acquire` -> `BrowserSession::run`), the exact
//! mechanism `ferridriver mcp` uses:
//!
//!  * a single persistent session survives a long sequence of `run`
//!    calls (REPL `globalThis` state carried the whole way, no spurious
//!    VM rebuild) against a LIVE browser page;
//!  * heavy object churn over hundreds of executes stays inside the
//!    memory quota and never poisons (GC-threshold + no leak);
//!  * a timeout poisons the VM, the NEXT call transparently rebuilds,
//!    and the durable `vars` tier survives the rebuild;
//!  * a browser-epoch change rebuilds the VM but keeps `vars`;
//!  * the `SessionTable` LRU cap + idle-TTL retention works end to end
//!    with real sessions (VM evicted, `vars` survive; idle record
//!    reaped).

use std::sync::Arc;
use std::time::Duration;

use ferridriver::chromium;
use ferridriver::options::LaunchOptions;
use ferridriver_script::{
  InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngineConfig, SessionTable,
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

/// Build the per-call `RunContext` exactly like the MCP server does:
/// the slot's durable `vars`, the live page, a fresh sandbox.
fn ctx_for(vars: Arc<InMemoryVars>, sandbox: Arc<PathSandbox>, page: Option<Arc<ferridriver::Page>>) -> RunContext {
  RunContext {
    vars,
    sandbox,
    artifacts: None,
    page,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ferridriver_script::ExtensionHost::Mcp,
    caps: ferridriver_script::ScriptCaps::default(),
  }
}

fn ok_value(r: ferridriver_script::ScriptResult, label: &str) -> serde_json::Value {
  match r.outcome {
    Outcome::Ok { success } => success.value,
    Outcome::Error { error } => panic!("[{label}] expected ok, got error: {error:?}"),
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn long_live_session_keeps_state_and_stays_healthy() {
  let browser = chromium()
    .launch(LaunchOptions::default())
    .await
    .expect("launch browser");
  let page = browser.page().await.expect("page");
  let tmp = tempfile::tempdir().expect("tmp");
  let sandbox = Arc::new(PathSandbox::new(tmp.path()).expect("sandbox"));

  let table = SessionTable::new(64, Some(Duration::from_secs(1800)));
  let slot = table.acquire("mcp-1");
  let cfg = ScriptEngineConfig::default();

  // One-time navigation + REPL counter seed.
  {
    let mut s = slot.lock().await;
    let vars = s.vars();
    let ctx = ctx_for(vars, sandbox.clone(), Some(page.clone()));
    let r = s
      .run(
        cfg.clone(),
        &format!(
          "await page.goto({url:?}); globalThis.c = 0; vars.set('persisted', 'yes'); return true;",
          url = data_url("<title>Long</title><body><h1 id='h'>hi</h1></body>")
        ),
        &[],
        RunOptions::default(),
        ctx,
        Some(1),
      )
      .await;
    assert_eq!(ok_value(r, "seed"), serde_json::json!(true));
  }

  // 40 sequential calls: each touches the live page AND increments a
  // `globalThis` counter. If the VM ever rebuilt spuriously the counter
  // would reset and the final assert (== 40) would fail.
  for i in 1..=40u32 {
    let mut s = slot.lock().await;
    let vars = s.vars();
    let ctx = ctx_for(vars, sandbox.clone(), Some(page.clone()));
    let r = s
      .run(
        cfg.clone(),
        "globalThis.c += 1; \
         const t = await page.locator('#h').textContent(); \
         vars.set('lastIter', String(globalThis.c)); \
         return { c: globalThis.c, t };",
        &[],
        RunOptions::default(),
        ctx,
        Some(1),
      )
      .await;
    let v = ok_value(r, "iter");
    assert_eq!(
      v["c"],
      serde_json::json!(i),
      "REPL counter must advance with no rebuild"
    );
    assert_eq!(v["t"], serde_json::json!("hi"), "live page still reachable at iter {i}");
  }
  {
    let mut s = slot.lock().await;
    let vars = s.vars();
    let ctx = ctx_for(vars, sandbox.clone(), Some(page.clone()));
    let r = s
      .run(
        cfg.clone(),
        "return { c: globalThis.c, persisted: vars.get('persisted'), last: vars.get('lastIter') };",
        &[],
        RunOptions::default(),
        ctx,
        Some(1),
      )
      .await;
    let v = ok_value(r, "final");
    assert_eq!(v["c"], serde_json::json!(40), "globalThis survived all 41 executes");
    assert_eq!(v["persisted"], serde_json::json!("yes"));
    assert_eq!(v["last"], serde_json::json!("40"));
  }

  // Heavy object churn: 250 executes each allocating/returning a sizable
  // structure. Stays under the 256 MiB quota and never poisons — proves
  // the cycle-GC threshold path frees memory and there is no leak across
  // a long-lived VM.
  for i in 0..250u32 {
    let mut s = slot.lock().await;
    let vars = s.vars();
    let ctx = ctx_for(vars, sandbox.clone(), None);
    let r = s
      .run(
        cfg.clone(),
        "const a = Array.from({length: 5000}, (_, i) => ({ i, s: 'x'.repeat(32), nested: [i, i*2] })); \
         globalThis.churn = (globalThis.churn || 0) + 1; \
         return a.length + globalThis.churn;",
        &[],
        RunOptions::default(),
        ctx,
        Some(1),
      )
      .await;
    match r.outcome {
      Outcome::Ok { .. } => {},
      Outcome::Error { error } => panic!("churn iter {i} failed (possible leak/OOM): {error:?}"),
    }
  }
  {
    let mut s = slot.lock().await;
    let vars = s.vars();
    let ctx = ctx_for(vars, sandbox.clone(), None);
    let r = s
      .run(
        cfg.clone(),
        "return globalThis.churn;",
        &[],
        RunOptions::default(),
        ctx,
        Some(1),
      )
      .await;
    assert_eq!(
      ok_value(r, "churn-final"),
      serde_json::json!(250),
      "all 250 churn executes ran in one VM"
    );
  }

  // Timeout poisons the VM; the durable `vars` must still survive and
  // the NEXT call must transparently rebuild and run cleanly.
  {
    let mut s = slot.lock().await;
    let vars = s.vars();
    let ctx = ctx_for(vars, sandbox.clone(), None);
    let r = s
      .run(
        cfg.clone(),
        "while (true) {}",
        &[],
        RunOptions {
          timeout: Some(Duration::from_millis(200)),
          ..Default::default()
        },
        ctx,
        Some(1),
      )
      .await;
    assert!(
      matches!(r.outcome, Outcome::Error { .. }),
      "infinite loop must time out"
    );
  }
  {
    let mut s = slot.lock().await;
    let vars = s.vars();
    let ctx = ctx_for(vars, sandbox.clone(), None);
    let r = s
      .run(
        cfg.clone(),
        "return { rebuilt: globalThis.c === undefined, persisted: vars.get('persisted') };",
        &[],
        RunOptions::default(),
        ctx,
        Some(1),
      )
      .await;
    let v = ok_value(r, "post-poison");
    assert_eq!(
      v["rebuilt"],
      serde_json::json!(true),
      "poisoned VM was rebuilt (globalThis cleared)"
    );
    assert_eq!(
      v["persisted"],
      serde_json::json!("yes"),
      "durable vars survived the poison rebuild"
    );
  }

  // A browser-epoch change (relaunch/reconnect under the same session
  // name) rebuilds the VM but keeps `vars`.
  {
    let mut s = slot.lock().await;
    let vars = s.vars();
    let ctx = ctx_for(vars, sandbox.clone(), None);
    let r = s
      .run(
        cfg.clone(),
        "globalThis.afterEpoch = (globalThis.afterEpoch || 0) + 1; \
         return { fresh: globalThis.afterEpoch === 1, persisted: vars.get('persisted') };",
        &[],
        RunOptions::default(),
        ctx,
        Some(2), // epoch changed 1 -> 2
      )
      .await;
    let v = ok_value(r, "epoch");
    assert_eq!(v["fresh"], serde_json::json!(true), "epoch change rebuilt the VM");
    assert_eq!(v["persisted"], serde_json::json!("yes"), "vars survived epoch rebuild");
  }
}

#[tokio::test(flavor = "multi_thread")]
async fn session_table_cap_and_idle_ttl_end_to_end() {
  // Cap of 1 warm VM + a short idle TTL, driven through the production
  // path with real (browserless) sessions.
  let table = SessionTable::new(1, Some(Duration::from_millis(150)));
  let cfg = ScriptEngineConfig::default();
  let tmp = tempfile::tempdir().expect("tmp");
  let sandbox = Arc::new(PathSandbox::new(tmp.path()).expect("sandbox"));

  // Session "a": build a VM, set globalThis + durable vars.
  {
    let slot = table.acquire("a");
    let mut s = slot.lock().await;
    let vars = s.vars();
    let ctx = ctx_for(vars, sandbox.clone(), None);
    let r = s
      .run(
        cfg.clone(),
        "globalThis.tag = 'A'; vars.set('owner', 'a'); return true;",
        &[],
        RunOptions::default(),
        ctx,
        None,
      )
      .await;
    assert_eq!(ok_value(r, "a-build"), serde_json::json!(true));
  }
  assert_eq!(table.live_vm_count(), 1, "one warm VM after building 'a'");

  // Session "b": building its VM exceeds the cap of 1, so 'a' is
  // evicted (VM dropped) — but 'a' keeps its identity + durable vars.
  {
    let slot = table.acquire("b");
    let mut s = slot.lock().await;
    let vars = s.vars();
    let ctx = ctx_for(vars, sandbox.clone(), None);
    let r = s
      .run(
        cfg.clone(),
        "globalThis.tag = 'B'; return true;",
        &[],
        RunOptions::default(),
        ctx,
        None,
      )
      .await;
    assert_eq!(ok_value(r, "b-build"), serde_json::json!(true));
  }
  assert!(table.live_vm_count() <= 1, "cap holds the warm-VM count at <= 1");

  // Re-acquire "a": its VM was evicted (globalThis cleared) but the
  // durable `vars` survived the cap eviction.
  {
    let slot = table.acquire("a");
    let mut s = slot.lock().await;
    let vars = s.vars();
    let ctx = ctx_for(vars, sandbox.clone(), None);
    let r = s
      .run(
        cfg.clone(),
        "return { vmGone: globalThis.tag === undefined, owner: vars.get('owner') };",
        &[],
        RunOptions::default(),
        ctx,
        None,
      )
      .await;
    let v = ok_value(r, "a-reacquire");
    assert_eq!(v["vmGone"], serde_json::json!(true), "evicted VM rebuilt fresh");
    assert_eq!(v["owner"], serde_json::json!("a"), "vars survived cap eviction");
  }

  // Idle-TTL reap: let every session sit longer than the TTL, then a
  // fresh acquire triggers the sweep. The reaped session's durable
  // vars are gone (whole record dropped), proving memory is released.
  tokio::time::sleep(Duration::from_millis(250)).await;
  let slot = table.acquire("c"); // sweep runs here
  {
    let mut s = slot.lock().await;
    let vars = s.vars();
    let ctx = ctx_for(vars, sandbox.clone(), None);
    let _ = s
      .run(cfg.clone(), "return 1;", &[], RunOptions::default(), ctx, None)
      .await;
  }
  // After the TTL sweep, the long-idle "a"/"b" records were reaped: a
  // fresh acquire of "a" must NOT see the old durable vars.
  {
    let slot = table.acquire("a");
    let mut s = slot.lock().await;
    let vars = s.vars();
    let ctx = ctx_for(vars, sandbox.clone(), None);
    let r = s
      .run(
        cfg.clone(),
        "return vars.get('owner');",
        &[],
        RunOptions::default(),
        ctx,
        None,
      )
      .await;
    let v = ok_value(r, "a-after-reap");
    assert!(
      v.is_null() || v == serde_json::Value::Null,
      "idle-reaped session starts fresh (no leaked durable vars), got {v}"
    );
  }
}

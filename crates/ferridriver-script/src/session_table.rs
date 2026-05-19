//! Persistent per-session script VMs, as one owned aggregate, with a
//! crisp two-tier lifetime:
//!
//! - **The VM** (`globalThis`, compiled plugin bytecode, timers) is the
//!   heavy, disposable tier. It is rebuilt on poison (timeout/OOM), on a
//!   browser-session swap (relaunch/reconnect under the same name), and
//!   dropped under the warm-VM cap when another session needs a slot.
//! - **`vars`** is the light, durable tier: a string store that lives
//!   for the *logical session's* whole lifetime. It survives every VM
//!   rebuild above — cap eviction drops only the VM, not the session
//!   record. The single thing `globalThis` cannot give you.
//!
//! A logical session ends (and its `vars` are released) only on: an
//! idle-TTL reap, an explicit [`SessionTable::remove`], or
//! [`SessionTable::clear`] (server shutdown). That is the whole `vars`
//! durability contract — no fuzzier than that.
//!
//! Browser-agnostic by construction: a `RunContext` carries whatever
//! browser handles a call has (or `None`) and the browser `epoch` is
//! passed in, so every policy here is unit-testable without a browser.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::Mutex as AsyncMutex;

use crate::engine::{RunContext, RunOptions, ScriptEngineConfig, Session};
use crate::error::ScriptError;
use crate::result::ScriptResult;
use crate::vars::InMemoryVars;

/// One logical session: the (disposable) persistent VM, the (durable)
/// session-scoped `vars`, and the browser generation the live VM was
/// built against. Access is serialized by the [`AsyncMutex`]
/// [`SessionTable`] hands out — that slot lock IS the per-session
/// execution guard, so the invariant is structural, not a comment.
pub struct BrowserSession {
  vm: Option<Session>,
  vars: Arc<InMemoryVars>,
  /// Persistent child processes (servers/watchers) started by this
  /// session. Durable tier alongside `vars`: survives VM rebuild;
  /// `Drop` (idle reap / close / shutdown) SIGKILLs every group.
  procs: Arc<crate::session_procs::SessionProcs>,
  last_used: Instant,
  /// Browser instance generation the live `vm` was built against.
  /// `None` until first build, or when no browser is bound.
  epoch: Option<u64>,
}

impl BrowserSession {
  fn new() -> Self {
    Self {
      vm: None,
      vars: Arc::new(InMemoryVars::new()),
      procs: Arc::new(crate::session_procs::SessionProcs::default()),
      last_used: Instant::now(),
      epoch: None,
    }
  }

  /// The session-scoped `vars` store. Outlives every VM rebuild and cap
  /// eviction for the session's whole lifetime; released only when the
  /// session record itself is dropped (idle-TTL reap / explicit close).
  /// The caller threads this into the `RunContext` for [`Self::run`].
  #[must_use]
  pub fn vars(&self) -> Arc<InMemoryVars> {
    self.vars.clone()
  }

  fn has_vm(&self) -> bool {
    self.vm.is_some()
  }

  /// Drop only the VM, keeping the durable `vars` and the session
  /// record. Used by the warm-VM cap: a capped-out session keeps its
  /// identity + `vars`, just loses its compiled VM until next call.
  fn drop_vm(&mut self) {
    self.vm = None;
  }

  /// Execute one script against the persistent VM.
  ///
  /// Rebuilds the VM when: it does not exist yet, a prior call poisoned
  /// it (timeout/OOM force-halt), or `epoch` no longer matches the
  /// browser session it was built against (relaunch/reconnect under the
  /// same session name — a *different* browser, so any JS handles the
  /// old `globalThis` cached are dead and must not be reachable). In
  /// every one of those cases `vars` is untouched.
  pub async fn run(
    &mut self,
    config: ScriptEngineConfig,
    source: &str,
    args: &[serde_json::Value],
    options: RunOptions,
    context: RunContext,
    epoch: Option<u64>,
  ) -> ScriptResult {
    if self.vm.is_some() && self.epoch != epoch {
      self.vm = None;
    }

    if self.vm.is_none() {
      match Session::create(config, &context).await {
        Ok(vm) => {
          self.vm = Some(vm);
          self.epoch = epoch;
        },
        Err(e) => {
          self.last_used = Instant::now();
          return ScriptResult::err(e, 0, Vec::new());
        },
      }
    }

    let run = match self.vm.as_ref() {
      Some(vm) => {
        // Re-install the durable process registry on every (re)built VM
        // so a tool's `commands` start/status/stop reaches the SAME
        // registry that outlives the VM.
        vm.install_session_procs(self.procs.clone()).await;
        vm.execute(source, args, options, &context).await
      },
      None => {
        return ScriptResult::err(
          ScriptError::internal("session vm unexpectedly absent".to_string()),
          0,
          Vec::new(),
        );
      },
    };

    // A poisoning fault (timeout interrupt / OOM) left the interpreter
    // halted at an arbitrary point — discard so the NEXT call rebuilds.
    // A plain JS throw is not poisoning and keeps the warm VM.
    if run.poisoned {
      self.vm = None;
    }
    self.last_used = Instant::now();
    run.result
  }
}

/// The set of live sessions plus the retention policy. Cheap to share
/// (`Arc` it); every method takes `&self`.
pub struct SessionTable {
  map: Mutex<HashMap<String, Arc<AsyncMutex<BrowserSession>>>>,
  /// Upper bound on concurrently-warm VMs (not session records).
  max_vms: usize,
  idle_ttl: Option<Duration>,
}

impl SessionTable {
  #[must_use]
  pub fn new(max_vms: usize, idle_ttl: Option<Duration>) -> Self {
    Self {
      map: Mutex::new(HashMap::new()),
      max_vms: max_vms.max(1),
      idle_ttl,
    }
  }

  /// Get (or create) the slot for `name`. Before returning it this:
  ///
  /// 1. Reaps idle sessions whole (past `idle_ttl`) — the only implicit
  ///    end of a logical session; its `vars` go with it.
  /// 2. If this acquire will build a VM and the warm-VM cap is already
  ///    met, drops the *VM* of the least-recently-used other session
  ///    (its session record + `vars` stay; it rebuilds on next use).
  ///
  /// A slot currently locked (execution in flight) is never reaped nor
  /// VM-evicted — the cap is soft; correctness over the bound. The
  /// returned slot's [`AsyncMutex`] is the per-session execution guard:
  /// `lock().await` it, build a `RunContext` with its `vars()`, then
  /// call [`BrowserSession::run`].
  pub fn acquire(&self, name: &str) -> Arc<AsyncMutex<BrowserSession>> {
    let mut map = self.map.lock().unwrap_or_else(PoisonError::into_inner);

    if let Some(ttl) = self.idle_ttl {
      let now = Instant::now();
      map.retain(|_, slot| match slot.try_lock() {
        Ok(s) => now.duration_since(s.last_used) < ttl,
        Err(_) => true, // in flight — keep
      });
    }

    // A build happens unless an entry already holds a live VM for this
    // name (a locked entry owns its own VM lifecycle — don't second-guess).
    let will_build = match map.get(name).map(|s| s.try_lock()) {
      Some(Ok(s)) => !s.has_vm(),
      Some(Err(_)) => false,
      None => true,
    };

    if will_build {
      let mut live: Vec<(String, Instant)> = map
        .iter()
        .filter(|(k, _)| k.as_str() != name)
        .filter_map(|(k, s)| {
          s.try_lock()
            .ok()
            .and_then(|g| g.has_vm().then(|| (k.clone(), g.last_used)))
        })
        .collect();
      if live.len() >= self.max_vms {
        live.sort_by_key(|(_, t)| *t);
        if let Some((victim, _)) = live.first()
          && let Some(slot) = map.get(victim)
          && let Ok(mut g) = slot.try_lock()
        {
          g.drop_vm();
        }
      }
    }

    map
      .entry(name.to_string())
      .or_insert_with(|| Arc::new(AsyncMutex::new(BrowserSession::new())))
      .clone()
  }

  /// End a logical session (explicit close / browser shutdown): drops
  /// the slot and its durable `vars`. A later `acquire` starts fresh.
  pub fn remove(&self, name: &str) {
    self.map.lock().unwrap_or_else(PoisonError::into_inner).remove(name);
  }

  /// End every session (server shutdown).
  pub fn clear(&self) {
    self.map.lock().unwrap_or_else(PoisonError::into_inner).clear();
  }

  /// Number of live session records (durable tier), warm or not.
  #[must_use]
  pub fn len(&self) -> usize {
    self.map.lock().unwrap_or_else(PoisonError::into_inner).len()
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  /// Number of sessions currently holding a warm VM (heavy tier).
  /// Bounded by `max_vms` modulo in-flight soft-cap slack.
  #[must_use]
  pub fn live_vm_count(&self) -> usize {
    self
      .map
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .values()
      .filter(|s| s.try_lock().map_or(true, |g| g.has_vm()))
      .count()
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use super::*;
  use crate::fs::PathSandbox;

  fn ctx_with(vars: Arc<InMemoryVars>) -> (tempfile::TempDir, RunContext) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ctx = RunContext {
      vars,
      sandbox: Arc::new(PathSandbox::new(tmp.path()).expect("sandbox")),
      artifacts: None,
      page: None,
      browser_context: None,
      request: None,
      browser: None,
      plugins: Vec::new(),
      trusted_modules: false,
      host: crate::engine::ExtensionHost::Script,
      caps: crate::engine::ScriptCaps::default(),
    };
    (tmp, ctx)
  }

  async fn run(slot: &Arc<AsyncMutex<BrowserSession>>, src: &str, epoch: Option<u64>) -> ScriptResult {
    let mut s = slot.lock().await;
    let vars = s.vars();
    let (_tmp, ctx) = ctx_with(vars);
    s.run(
      ScriptEngineConfig::default(),
      src,
      &[],
      RunOptions::default(),
      ctx,
      epoch,
    )
    .await
  }

  #[track_caller]
  fn assert_ok(actual: &ScriptResult, expected: serde_json::Value) {
    match &actual.outcome {
      crate::result::Outcome::Ok { success } => assert_eq!(success.value, expected, "unexpected script value"),
      crate::result::Outcome::Error { error } => panic!("expected ok {expected}, got error: {error:?}"),
    }
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn vars_persist_but_globalthis_dies_on_browser_swap() {
    let table = SessionTable::new(8, None);
    let slot = table.acquire("s");

    let r = run(&slot, "globalThis.k = 7; vars.set('v', 'keep'); return 'a';", Some(1)).await;
    assert!(r.is_ok(), "{r:?}");
    let r = run(&slot, "return globalThis.k ?? 'gone';", Some(1)).await;
    assert_ok(&r, serde_json::json!(7));

    // Browser relaunched (epoch change): VM rebuilt, globalThis gone,
    // durable vars survive.
    let r = run(&slot, "return globalThis.k ?? 'gone';", Some(2)).await;
    assert_ok(&r, serde_json::json!("gone"));
    let r = run(&slot, "return vars.get('v') ?? 'missing';", Some(2)).await;
    assert_ok(&r, serde_json::json!("keep"));
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn cap_evicts_vm_but_vars_survive_the_eviction() {
    let table = SessionTable::new(1, None);
    let a = table.acquire("a");
    let r = run(&a, "globalThis.g = 1; vars.set('tok', 'abc'); return 1;", None).await;
    assert!(r.is_ok(), "{r:?}");

    // Acquiring + running "b" needs a VM; cap is 1, so "a"'s VM is
    // evicted — but its session record + vars stay.
    let b = table.acquire("b");
    let _ = run(&b, "return 1;", None).await;

    assert_eq!(table.len(), 2, "both session records live (vars tier)");
    {
      let m = table.map.lock().unwrap();
      let ga = m.get("a").unwrap().try_lock().unwrap();
      assert!(!ga.has_vm(), "a's VM was evicted under the cap");
    }

    // "a" rebuilds on next use: globalThis gone, durable vars intact.
    let r = run(&a, "return globalThis.g ?? 'rebuilt';", None).await;
    assert_ok(&r, serde_json::json!("rebuilt"));
    let r = run(&a, "return vars.get('tok') ?? 'lost';", None).await;
    assert_ok(&r, serde_json::json!("abc"));
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn in_flight_vm_is_never_cap_evicted() {
    let table = SessionTable::new(1, None);
    let a = table.acquire("a");
    run(&a, "return 1;", None).await;

    // Hold "a" locked (in flight) while "b" forces cap pressure: "a"'s
    // VM must NOT be dropped (locked => skipped), soft cap slack.
    let a_guard = a.lock().await;
    let b = table.acquire("b");
    run(&b, "return 1;", None).await;
    assert!(a_guard.has_vm(), "in-flight VM kept despite cap pressure");
    drop(a_guard);
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn idle_ttl_reaps_whole_session_including_vars() {
    let table = SessionTable::new(64, Some(Duration::from_millis(60)));
    let a = table.acquire("a");
    run(&a, "vars.set('x','1'); return 1;", None).await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    let _b = table.acquire("b"); // triggers reap sweep
    let present = {
      let m = table.map.lock().unwrap();
      (m.contains_key("a"), m.contains_key("b"))
    };
    assert_eq!(present, (false, true), "idle session reaped whole; fresh kept");
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn poison_on_timeout_rebuilds_next_call() {
    let table = SessionTable::new(8, None);
    let slot = table.acquire("s");
    {
      let mut s = slot.lock().await;
      let vars = s.vars();
      let (_tmp, ctx) = ctx_with(vars);
      let opts = RunOptions {
        timeout: Some(Duration::from_millis(50)),
        ..RunOptions::default()
      };
      let r = s
        .run(
          ScriptEngineConfig::default(),
          "globalThis.before = 1; while (true) {}",
          &[],
          opts,
          ctx,
          None,
        )
        .await;
      assert!(r.is_err(), "infinite loop must time out");
      assert!(!s.has_vm(), "timeout must poison (discard) the VM");
    }
    let r = run(&slot, "return globalThis.before ?? 'fresh';", None).await;
    assert_ok(&r, serde_json::json!("fresh"));
  }
}

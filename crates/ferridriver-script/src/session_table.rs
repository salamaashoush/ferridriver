//! Persistent per-session script VMs, as one owned aggregate, with a
//! crisp two-tier lifetime:
//!
//! - **The VM** (`globalThis`, compiled extension bytecode, timers) is the
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

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::Mutex as AsyncMutex;

use crate::bundle::CompiledBundle;
use crate::engine::{RunContext, RunOptions, ScriptEngineConfig, Session, SessionRun};
use crate::error::ScriptError;
use crate::result::ScriptResult;
use crate::vars::InMemoryVars;

/// One logical session: the (disposable) persistent VM, the (durable)
/// session-scoped `vars`, and the browser generation the live VM was
/// built against. Access is serialized by the [`AsyncMutex`]
/// [`SessionTable`] hands out — that slot lock IS the per-session
/// execution guard, so the invariant is structural, not a comment.
pub struct BrowserSession {
  resources: Arc<crate::SessionResources>,
  vm: Option<Session>,
  vars: Arc<InMemoryVars>,
  /// The session's HTTP client (`request` binding + `fetch` core).
  /// Durable tier alongside `vars`: one reqwest client + cookie jar for
  /// the logical session's lifetime, so connections/TLS sessions are
  /// reused across calls and cookies persist like `vars` do — instead
  /// of a fresh client (empty jar, cold pool) per tool call.
  request: Arc<ferridriver::http_client::HttpClient>,
  last_used: Instant,
  /// Browser instance generation the live `vm` was built against.
  /// `None` until first build, or when no browser is bound.
  epoch: Option<u64>,
}

impl BrowserSession {
  fn new(resources: Arc<crate::SessionResources>) -> Self {
    Self {
      resources,
      vm: None,
      vars: Arc::new(InMemoryVars::new()),
      request: Arc::new(ferridriver::http_client::HttpClient::new(
        ferridriver::http_client::HttpClientOptions::default(),
      )),
      last_used: Instant::now(),
      epoch: None,
    }
  }

  pub async fn close(&mut self) -> Result<(), ScriptError> {
    self.resources.begin_close();
    self.vm = None;
    self.resources.close().await
  }

  /// The session-scoped `vars` store. Outlives every VM rebuild and cap
  /// eviction for the session's whole lifetime; released only when the
  /// session record itself is dropped (idle-TTL reap / explicit close).
  /// The caller threads this into the `RunContext` for [`Self::run`].
  #[must_use]
  pub fn vars(&self) -> Arc<InMemoryVars> {
    self.vars.clone()
  }

  /// The session-scoped HTTP client. Same durability contract as
  /// [`Self::vars`]: survives VM rebuilds, released with the session
  /// record. The caller threads this into the `RunContext`.
  #[must_use]
  pub fn request(&self) -> Arc<ferridriver::http_client::HttpClient> {
    self.request.clone()
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
  /// Ensure the session VM exists for `epoch`, rebuilding it when the
  /// epoch changed (a browser reattach invalidates the prior VM).
  async fn ensure_vm(
    &mut self,
    config: ScriptEngineConfig,
    context: &RunContext,
    epoch: Option<u64>,
  ) -> Result<(), ScriptError> {
    self.resources.ensure_open()?;
    if self.vm.as_ref().is_some_and(Session::poisoned) || self.vm.is_some() && self.epoch != epoch {
      self.vm = None;
    }
    if self.vm.is_none() {
      self.vm = Some(self.resources.create(config, context).await?);
      self.epoch = epoch;
    }
    Ok(())
  }

  /// Apply the poison rule (discard the VM after a timeout/OOM force-halt
  /// so the next call rebuilds; a plain throw keeps the warm VM) and the
  /// last-used bookkeeping, returning the run's result.
  fn finish_run(&mut self, run: SessionRun) -> ScriptResult {
    if run.poisoned {
      self.vm = None;
    }
    self.last_used = Instant::now();
    run.result
  }

  pub async fn run(
    &mut self,
    config: ScriptEngineConfig,
    source: &str,
    args: &[serde_json::Value],
    options: RunOptions,
    context: RunContext,
    epoch: Option<u64>,
  ) -> ScriptResult {
    if let Err(e) = self.ensure_vm(config, &context, epoch).await {
      self.last_used = Instant::now();
      return ScriptResult::err(e, 0, Vec::new());
    }
    let Some(vm) = self.vm.as_ref() else {
      return ScriptResult::err(
        ScriptError::internal("session vm unexpectedly absent".to_string()),
        0,
        Vec::new(),
      );
    };
    let run = vm.execute(source, args, options, &context).await;
    self.finish_run(run)
  }

  /// Like [`Self::run`], but executes a precompiled bundled ES module — the
  /// TypeScript / `import` path. The run's result is the module's
  /// `default` export. Shares VM lifecycle + poison handling with `run`.
  pub async fn run_module(
    &mut self,
    config: ScriptEngineConfig,
    bundle: &CompiledBundle,
    args: &[serde_json::Value],
    options: RunOptions,
    context: RunContext,
    epoch: Option<u64>,
  ) -> ScriptResult {
    if let Err(e) = self.ensure_vm(config, &context, epoch).await {
      self.last_used = Instant::now();
      return ScriptResult::err(e, 0, Vec::new());
    }
    let Some(vm) = self.vm.as_ref() else {
      return ScriptResult::err(
        ScriptError::internal("session vm unexpectedly absent".to_string()),
        0,
        Vec::new(),
      );
    };
    let run = vm.execute_module(bundle, args, options, &context).await;
    self.finish_run(run)
  }

  /// Like [`Self::run`], but natively invokes a registered extension
  /// tool by manifest name (no synthesized script, no compile). Shares
  /// VM lifecycle + poison handling with `run`.
  pub async fn run_tool(
    &mut self,
    config: ScriptEngineConfig,
    name: &str,
    tool_args: serde_json::Value,
    options: RunOptions,
    context: RunContext,
    epoch: Option<u64>,
  ) -> ScriptResult {
    if let Err(e) = self.ensure_vm(config, &context, epoch).await {
      self.last_used = Instant::now();
      return ScriptResult::err(e, 0, Vec::new());
    }
    let Some(vm) = self.vm.as_ref() else {
      return ScriptResult::err(
        ScriptError::internal("session vm unexpectedly absent".to_string()),
        0,
        Vec::new(),
      );
    };
    let run = vm.execute_tool(name, tool_args, options, &context).await;
    self.finish_run(run)
  }
}

#[derive(Clone)]
struct SessionEntry {
  session: Arc<AsyncMutex<BrowserSession>>,
  resources: Arc<crate::SessionResources>,
}

impl SessionEntry {
  fn new() -> Self {
    let resources = Arc::new(crate::SessionResources::default());
    Self {
      session: Arc::new(AsyncMutex::new(BrowserSession::new(Arc::clone(&resources)))),
      resources,
    }
  }
}

#[derive(Default)]
struct Admission {
  closing: bool,
  terminal: bool,
  generation: Arc<()>,
}

/// The set of live sessions plus the retention policy. Cheap to share
/// (`Arc` it); every method takes `&self`.
pub struct SessionTable {
  map: DashMap<String, SessionEntry>,
  admission: std::sync::Mutex<Admission>,
  cleanup: AsyncMutex<()>,
  /// Upper bound on concurrently-warm VMs (not session records).
  max_vms: usize,
  idle_ttl: Option<Duration>,
}

impl SessionTable {
  #[must_use]
  pub fn new(max_vms: usize, idle_ttl: Option<Duration>) -> Self {
    Self {
      map: DashMap::new(),
      admission: std::sync::Mutex::new(Admission::default()),
      cleanup: AsyncMutex::new(()),
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
  pub async fn acquire(&self, name: &str) -> Result<Arc<AsyncMutex<BrowserSession>>, ScriptError> {
    let generation = {
      let admission = self.admission.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      if admission.closing {
        return Err(ScriptError::named("TargetClosedError", "script sessions are closing"));
      }
      Arc::clone(&admission.generation)
    };
    self.reap_idle().await?;
    let admission = self.admission.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if admission.closing || !Arc::ptr_eq(&generation, &admission.generation) {
      return Err(ScriptError::named(
        "TargetClosedError",
        "script sessions closed during acquisition",
      ));
    }

    // A build happens unless an entry already holds a live VM for this
    // name (a locked entry owns its own VM lifecycle — don't second-guess).
    let will_build = self
      .map
      .get(name)
      .is_none_or(|slot| match slot.value().session.try_lock() {
        Ok(s) => !s.has_vm(),
        Err(_) => false,
      });

    if will_build {
      let mut live: Vec<(String, Instant)> = self
        .map
        .iter()
        .filter(|entry| entry.key().as_str() != name)
        .filter_map(|entry| {
          entry
            .value()
            .session
            .try_lock()
            .ok()
            .and_then(|g| g.has_vm().then(|| (entry.key().clone(), g.last_used)))
        })
        .collect();
      if live.len() >= self.max_vms {
        live.sort_by_key(|(_, t)| *t);
        if let Some((victim, _)) = live.first()
          && let Some(slot) = self.map.get(victim)
          && let Ok(mut g) = slot.value().session.try_lock()
        {
          g.drop_vm();
        }
      }
    }

    let entry = self.map.entry(name.to_string()).or_insert_with(SessionEntry::new);
    entry.value().resources.ensure_open()?;
    Ok(Arc::clone(&entry.value().session))
  }

  async fn reap_idle(&self) -> Result<(), ScriptError> {
    let Some(ttl) = self.idle_ttl else { return Ok(()) };
    let now = Instant::now();
    let mut errors = Vec::new();
    for (name, entry) in self.snapshot(|_| true) {
      let Ok(mut session) = entry.session.try_lock() else {
        continue;
      };
      if entry.resources.ensure_open().is_err() || now.saturating_duration_since(session.last_used) < ttl {
        continue;
      }
      if let Err(error) = self.close_entry(&name, &entry, &mut session).await {
        errors.push(format!("{name}: {}", error.message));
      }
    }
    cleanup_result(errors)
  }

  fn snapshot(&self, pred: impl Fn(&str) -> bool) -> Vec<(String, SessionEntry)> {
    self
      .map
      .iter()
      .filter(|entry| pred(entry.key()))
      .map(|entry| (entry.key().clone(), entry.value().clone()))
      .collect()
  }

  async fn close_entry(
    &self,
    name: &str,
    entry: &SessionEntry,
    session: &mut BrowserSession,
  ) -> Result<(), ScriptError> {
    session.close().await?;
    self
      .map
      .remove_if(name, |_, current| Arc::ptr_eq(&current.session, &entry.session));
    Ok(())
  }

  pub async fn remove(&self, name: &str) -> Result<(), ScriptError> {
    let Some(entry) = self.map.get(name).map(|entry| entry.value().clone()) else {
      return Ok(());
    };
    entry.resources.begin_close();
    let mut session = entry.session.lock().await;
    self.close_entry(name, &entry, &mut session).await
  }

  /// Whether a session record for `name` currently exists. Lets a host keep
  /// its own per-session side tables (console routers, observers) bounded by
  /// the same lifetime this table enforces, instead of by every name a client
  /// has ever addressed.
  #[must_use]
  pub fn contains(&self, name: &str) -> bool {
    self.map.contains_key(name)
  }

  /// End every session whose name satisfies `pred`. Used when a whole
  /// browser instance closes and every session routed to it — under
  /// names only the caller can map — has to go with it.
  pub async fn remove_matching(&self, pred: impl Fn(&str) -> bool) -> Result<(), ScriptError> {
    let entries = self.snapshot(pred);
    for (_, entry) in &entries {
      entry.resources.begin_close();
    }
    self.close_entries(entries).await
  }

  pub async fn clear(&self) -> Result<(), ScriptError> {
    self.clear_after(std::future::ready(Ok(()))).await
  }

  pub async fn close(&self) -> Result<(), ScriptError> {
    self.close_after(std::future::ready(Ok(()))).await
  }

  pub async fn clear_after(
    &self,
    release: impl std::future::Future<Output = Result<(), ScriptError>>,
  ) -> Result<(), ScriptError> {
    self.close_all(false, release).await
  }

  pub async fn close_after(
    &self,
    release: impl std::future::Future<Output = Result<(), ScriptError>>,
  ) -> Result<(), ScriptError> {
    self.close_all(true, release).await
  }

  async fn close_all(
    &self,
    terminal: bool,
    release: impl std::future::Future<Output = Result<(), ScriptError>>,
  ) -> Result<(), ScriptError> {
    let (generation, entries) = {
      let mut admission = self.admission.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      admission.closing = true;
      admission.terminal |= terminal;
      admission.generation = Arc::new(());
      let entries = self.snapshot(|_| true);
      for (_, entry) in &entries {
        entry.resources.begin_close();
      }
      (Arc::clone(&admission.generation), entries)
    };
    let _cleanup = self.cleanup.lock().await;
    if let Err(error) = release.await {
      for (_, entry) in entries {
        entry.session.lock().await.drop_vm();
      }
      return Err(error);
    }
    let result = self.close_entries(entries).await;
    let mut admission = self.admission.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if result.is_ok() && !admission.terminal && Arc::ptr_eq(&generation, &admission.generation) {
      admission.closing = false;
    }
    result
  }

  async fn close_entries(&self, entries: Vec<(String, SessionEntry)>) -> Result<(), ScriptError> {
    let mut errors = Vec::new();
    // A provider in one context may serve a browser in another. Finish all
    // browser cleanup before stopping any of the selected contexts' processes.
    for (name, entry) in &entries {
      let mut session = entry.session.lock().await;
      session.drop_vm();
      if let Err(error) = entry.resources.close_browsers().await {
        errors.push(format!("{name}: {}", error.message));
      }
    }
    cleanup_result(errors)?;
    let mut errors = Vec::new();
    for (name, entry) in entries {
      let mut session = entry.session.lock().await;
      if let Err(error) = self.close_entry(&name, &entry, &mut session).await {
        errors.push(format!("{name}: {}", error.message));
      }
    }
    cleanup_result(errors)
  }

  /// Discard every live VM while keeping each session's durable tier
  /// (`vars`, persistent processes, HTTP client + cookie jar). The next
  /// call for a session rebuilds its VM from the host's CURRENT extension
  /// set, which is how a reload reaches sessions that are already open —
  /// [`Self::clear`] would reach them too, but by throwing away the state
  /// the durable tier exists to protect.
  ///
  /// Takes each slot's lock, so a session with a call in flight is waited
  /// for rather than yanked out from under it. Returns how many VMs were
  /// dropped.
  pub async fn drop_all_vms(&self) -> usize {
    let slots: Vec<Arc<AsyncMutex<BrowserSession>>> = self.map.iter().map(|e| Arc::clone(&e.value().session)).collect();
    let mut dropped = 0;
    for slot in slots {
      let mut guard = slot.lock().await;
      if guard.has_vm() {
        guard.drop_vm();
        dropped += 1;
      }
    }
    dropped
  }

  /// Number of live session records (durable tier), warm or not.
  #[must_use]
  pub fn len(&self) -> usize {
    self.map.len()
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
      .iter()
      .filter(|entry| entry.value().session.try_lock().map_or(true, |g| g.has_vm()))
      .count()
  }
}

fn cleanup_result(errors: Vec<String>) -> Result<(), ScriptError> {
  if errors.is_empty() {
    Ok(())
  } else {
    Err(ScriptError::internal(errors.join("; ")))
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};

  use super::*;

  fn ctx_with(vars: Arc<InMemoryVars>) -> (tempfile::TempDir, RunContext) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ctx = RunContext {
      vars,
      script_root: tmp.path().to_path_buf(),
      artifacts: None,
      page: None,
      browser_context: None,
      request: None,
      browser: None,
      extensions: Vec::new(),
      host: crate::engine::ExtensionHost::Script,
      caps: crate::engine::ScriptCaps::default(),
      session: None,
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

  async fn request(stream: &mut tokio::net::TcpStream) -> String {
    let mut reader = tokio::io::BufReader::new(stream);
    let mut first = String::new();
    assert!(reader.read_line(&mut first).await.unwrap() > 0);
    let mut length = 0;
    loop {
      let mut line = String::new();
      assert!(reader.read_line(&mut line).await.unwrap() > 0);
      if line == "\r\n" {
        break;
      }
      if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
        length = value.trim().parse().unwrap();
      }
    }
    reader.read_exact(&mut vec![0; length]).await.unwrap();
    first.trim().to_owned()
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn browser_ownership_survives_vm_eviction_and_poison_until_cleanup_finishes() {
    for poison in [false, true] {
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let endpoint = format!("http://{}", listener.local_addr().unwrap());
      let (deleting, deletion) = tokio::sync::oneshot::channel();
      let (release, released) = tokio::sync::oneshot::channel();
      let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for value in [
          serde_json::json!({"sessionId":"owned","capabilities":{"browserName":"safari","browserVersion":"contract"}}),
          serde_json::json!([]),
        ] {
          let (mut stream, _) = listener.accept().await.unwrap();
          requests.push(request(&mut stream).await);
          let body = serde_json::json!({"value":value}).to_string();
          stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
        let (mut stream, _) = listener.accept().await.unwrap();
        requests.push(request(&mut stream).await);
        deleting.send(()).unwrap();
        released.await.unwrap();
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"value\":null}").await.unwrap();
        requests
      });
      let table = SessionTable::new(1, None);
      let slot = table.acquire("sashoush").await.unwrap();
      assert_ok(
        &run(
          &slot,
          &format!(
            "globalThis.nested = await safari().connect({endpoint:?}, {{timeout:1000}}); return nested.version();"
          ),
          None,
        )
        .await,
        serde_json::json!("safari/contract"),
      );
      if poison {
        let mut session = slot.lock().await;
        let (_root, context) = ctx_with(session.vars());
        let result = session
          .run(
            ScriptEngineConfig::default(),
            "while (true) {}",
            &[],
            RunOptions {
              timeout: Some(Duration::from_millis(50)),
              ..Default::default()
            },
            context,
            None,
          )
          .await;
        assert!(result.is_err());
      } else {
        table.acquire("another-session").await.unwrap();
      }
      assert!(!slot.lock().await.has_vm());
      assert_ok(
        &run(&slot, "return typeof globalThis.nested;", None).await,
        serde_json::json!("undefined"),
      );
      let resources = Arc::clone(&slot.lock().await.resources);
      let mut closing = Box::pin(resources.close());
      assert!(
        futures::poll!(&mut closing).is_pending(),
        "VM replacement discarded the browser owner"
      );
      tokio::select! {
        result = &mut closing => panic!("cleanup returned before the provider acknowledged deletion: {result:?}"),
        result = deletion => result.unwrap(),
      }
      assert!(futures::poll!(&mut closing).is_pending());
      release.send(()).unwrap();
      closing.await.unwrap();
      assert_eq!(
        server.await.unwrap(),
        [
          "POST /session HTTP/1.1",
          "GET /session/owned/window/handles HTTP/1.1",
          "DELETE /session/owned HTTP/1.1",
        ]
      );
    }
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn vars_persist_but_globalthis_dies_on_browser_swap() {
    let table = SessionTable::new(8, None);
    let slot = table.acquire("s").await.unwrap();

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
    let a = table.acquire("a").await.unwrap();
    let r = run(&a, "globalThis.g = 1; vars.set('tok', 'abc'); return 1;", None).await;
    assert!(r.is_ok(), "{r:?}");

    // Acquiring + running "b" needs a VM; cap is 1, so "a"'s VM is
    // evicted — but its session record + vars stay.
    let b = table.acquire("b").await.unwrap();
    let _ = run(&b, "return 1;", None).await;

    assert_eq!(table.len(), 2, "both session records live (vars tier)");
    {
      let slot = table.map.get("a").unwrap();
      let ga = slot.value().session.try_lock().unwrap();
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
    let a = table.acquire("a").await.unwrap();
    run(&a, "return 1;", None).await;

    // Hold "a" locked (in flight) while "b" forces cap pressure: "a"'s
    // VM must NOT be dropped (locked => skipped), soft cap slack.
    let a_guard = a.lock().await;
    let b = table.acquire("b").await.unwrap();
    run(&b, "return 1;", None).await;
    assert!(a_guard.has_vm(), "in-flight VM kept despite cap pressure");
    drop(a_guard);
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn idle_ttl_reaps_whole_session_including_vars() {
    let table = SessionTable::new(64, Some(Duration::from_millis(60)));
    let a = table.acquire("a").await.unwrap();
    run(&a, "vars.set('x','1'); return 1;", None).await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    let _b = table.acquire("b").await.unwrap(); // triggers reap sweep
    let present = (table.map.contains_key("a"), table.map.contains_key("b"));
    assert_eq!(present, (false, true), "idle session reaped whole; fresh kept");
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn request_client_is_durable_across_vm_rebuilds() {
    let table = SessionTable::new(8, None);
    let slot = table.acquire("s").await.unwrap();
    let before = {
      let mut s = slot.lock().await;
      let vars = s.vars();
      let (_tmp, ctx) = ctx_with(vars);
      let _ = s
        .run(
          ScriptEngineConfig::default(),
          "return 1;",
          &[],
          RunOptions::default(),
          ctx,
          Some(1),
        )
        .await;
      s.request()
    };
    // Epoch change rebuilds the VM; the session's HTTP client (cookie
    // jar + connection pool) must be the same instance afterwards —
    // durable tier, like `vars`.
    let after = {
      let mut s = slot.lock().await;
      let vars = s.vars();
      let (_tmp, ctx) = ctx_with(vars);
      let _ = s
        .run(
          ScriptEngineConfig::default(),
          "return 1;",
          &[],
          RunOptions::default(),
          ctx,
          Some(2),
        )
        .await;
      s.request()
    };
    assert!(
      Arc::ptr_eq(&before, &after),
      "request client must survive the VM rebuild"
    );
  }

  #[tokio::test(flavor = "multi_thread")]
  async fn poison_on_timeout_rebuilds_next_call() {
    let table = SessionTable::new(8, None);
    let slot = table.acquire("s").await.unwrap();
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
  #[tokio::test]
  async fn delayed_remove_does_not_remove_a_replacement_session() {
    let table = SessionTable::new(1, None);
    let original = table.acquire("sashoush").await.unwrap();
    let held = original.lock().await;
    let mut first = Box::pin(table.remove("sashoush"));
    let mut second = Box::pin(table.remove("sashoush"));
    assert!(futures::poll!(&mut first).is_pending());
    assert!(futures::poll!(&mut second).is_pending());
    assert!(table.acquire("sashoush").await.is_err());
    drop(held);
    first.await.unwrap();
    let replacement = table.acquire("sashoush").await.unwrap();
    assert!(!Arc::ptr_eq(&original, &replacement));
    second.await.unwrap();
    assert!(Arc::ptr_eq(&table.acquire("sashoush").await.unwrap(), &replacement));
    assert!(run(&original, "return 1;", None).await.is_err());
    assert_ok(&run(&replacement, "return 42;", None).await, serde_json::json!(42));
    table.close().await.unwrap();
  }

  #[tokio::test]
  async fn cancelled_clear_keeps_admission_closed_until_retry_and_terminal_close_stays_closed() {
    let table = SessionTable::new(1, None);
    let original = table.acquire("sashoush").await.unwrap();
    let held = original.lock().await;
    let mut closing = Box::pin(table.clear());
    assert!(futures::poll!(&mut closing).is_pending());
    assert!(table.acquire("another").await.is_err());
    drop(closing);
    drop(held);
    assert!(table.acquire("another").await.is_err());
    table.clear().await.unwrap();
    let replacement = table.acquire("sashoush").await.unwrap();
    assert!(!Arc::ptr_eq(&original, &replacement));
    assert!(run(&original, "return 1;", None).await.is_err());
    table.close().await.unwrap();
    table.clear().await.unwrap();
    assert!(table.acquire("sashoush").await.is_err());
  }

  struct OwnedBrowserFixture {
    endpoint: String,
    server: tokio::task::JoinHandle<Vec<String>>,
    deleting: tokio::sync::oneshot::Receiver<()>,
    release: tokio::sync::oneshot::Sender<()>,
  }

  async fn owned_browser_fixture(fail_first_delete: bool) -> OwnedBrowserFixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (deleting, deletion) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
      let mut requests = Vec::new();
      for value in [
        serde_json::json!({"sessionId":"owned","capabilities":{"browserName":"safari","browserVersion":"contract"}}),
        serde_json::json!([]),
      ] {
        let (mut stream, _) = listener.accept().await.unwrap();
        requests.push(request(&mut stream).await);
        reply(&mut stream, "200 OK", value).await;
      }
      let (mut stream, _) = listener.accept().await.unwrap();
      requests.push(request(&mut stream).await);
      let _ = deleting.send(());
      released.await.unwrap();
      if fail_first_delete {
        reply(
          &mut stream,
          "500 Internal Server Error",
          serde_json::json!({"error":"unknown error","message":"sashoush cleanup unavailable"}),
        )
        .await;
        let (mut retry, _) = listener.accept().await.unwrap();
        requests.push(request(&mut retry).await);
        reply(&mut retry, "200 OK", serde_json::Value::Null).await;
      } else {
        reply(&mut stream, "200 OK", serde_json::Value::Null).await;
      }
      requests
    });
    OwnedBrowserFixture {
      endpoint,
      server,
      deleting: deletion,
      release,
    }
  }

  async fn reply(stream: &mut tokio::net::TcpStream, status: &str, value: serde_json::Value) {
    let body = serde_json::json!({"value":value}).to_string();
    stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
  }

  async fn connect_owned(table: &SessionTable, endpoint: &str) -> Arc<AsyncMutex<BrowserSession>> {
    let slot = table.acquire("sashoush").await.unwrap();
    assert_ok(
      &run(
        &slot,
        &format!(
          "globalThis.nested = await safari().connect({endpoint:?}, {{timeout:1000}}); return nested.version();"
        ),
        None,
      )
      .await,
      serde_json::json!("safari/contract"),
    );
    slot
  }

  async fn start_owned_process(slot: &Arc<AsyncMutex<BrowserSession>>) -> Arc<crate::SessionResources> {
    let resources = Arc::clone(&slot.lock().await.resources);
    resources
      .procs
      .start(
        "sashoush",
        &crate::command_spec::ResolvedCommand {
          exec: crate::command_spec::ResolvedExec::Shell("printf ready; exec sleep 30".into()),
          timeout_ms: None,
          env: Vec::new(),
          cwd: None,
          output: crate::command_spec::CommandOutput::Text,
          persistent: true,
        },
      )
      .unwrap();
    resources.procs.wait_for_output("sashoush", "ready").await.unwrap();
    resources
  }

  #[tokio::test]
  async fn failed_logical_close_retains_the_session_for_retry_without_recreating_its_browser() {
    let fixture = owned_browser_fixture(true).await;
    let table = SessionTable::new(1, None);
    let slot = connect_owned(&table, &fixture.endpoint).await;
    let resources = start_owned_process(&slot).await;
    fixture.release.send(()).unwrap();
    let error = table.remove("sashoush").await.unwrap_err();
    assert!(
      error.message.contains("sashoush cleanup unavailable"),
      "{}",
      error.message
    );
    assert!(table.contains("sashoush"));
    assert!(table.acquire("sashoush").await.is_err());
    assert!(run(&slot, "return 1;", None).await.is_err());
    assert_eq!(
      resources.procs.status("sashoush").unwrap()["running"],
      true,
      "a command may provide the browser endpoint needed for cleanup retry"
    );
    table.remove("sashoush").await.unwrap();
    assert!(!table.contains("sashoush"));
    assert!(resources.procs.status("sashoush").is_err());
    assert_eq!(
      fixture.server.await.unwrap(),
      [
        "POST /session HTTP/1.1",
        "GET /session/owned/window/handles HTTP/1.1",
        "DELETE /session/owned HTTP/1.1",
        "DELETE /session/owned HTTP/1.1"
      ]
    );
  }

  #[tokio::test]
  async fn logical_close_preserves_process_dependencies_until_browser_acknowledgement() {
    let fixture = owned_browser_fixture(false).await;
    let table = SessionTable::new(1, None);
    let slot = connect_owned(&table, &fixture.endpoint).await;
    let resources = start_owned_process(&slot).await;
    let mut closing = Box::pin(table.remove("sashoush"));
    tokio::select! {
      result = &mut closing => panic!("close returned before DELETE acknowledgement: {result:?}"),
      result = fixture.deleting => result.unwrap(),
    }
    assert_eq!(resources.procs.status("sashoush").unwrap()["running"], true);
    assert!(futures::poll!(&mut closing).is_pending());
    assert!(table.contains("sashoush"));
    fixture.release.send(()).unwrap();
    closing.await.unwrap();
    assert!(resources.procs.status("sashoush").is_err());
    assert!(!table.contains("sashoush"));
    assert_eq!(fixture.server.await.unwrap().len(), 3);
  }

  #[tokio::test]
  async fn acquisition_started_before_clear_cannot_create_a_session_after_clear() {
    let fixture = owned_browser_fixture(false).await;
    let table = SessionTable::new(1, Some(Duration::from_secs(1)));
    let slot = connect_owned(&table, &fixture.endpoint).await;
    slot.lock().await.last_used = Instant::now().checked_sub(Duration::from_secs(2)).unwrap();
    let mut acquiring = Box::pin(table.acquire("another"));
    tokio::select! {
      result = &mut acquiring => panic!("acquisition returned before TTL cleanup: {}", result.is_ok()),
      result = fixture.deleting => result.unwrap(),
    }
    let mut clearing = Box::pin(table.clear());
    assert!(futures::poll!(&mut clearing).is_pending());
    fixture.release.send(()).unwrap();
    assert!(acquiring.await.is_err());
    clearing.await.unwrap();
    assert!(!table.contains("another"));
    table.acquire("another").await.unwrap();
    table.close().await.unwrap();
    assert_eq!(fixture.server.await.unwrap().len(), 3);
  }

  #[tokio::test]
  async fn explicitly_closed_sessions_reject_further_script_execution() {
    let (_root, context) = ctx_with(Arc::new(InMemoryVars::new()));
    let session = Session::create(ScriptEngineConfig::default(), &context).await.unwrap();
    session.close().await.unwrap();
    let result = session
      .execute("return 42;", &[], RunOptions::default(), &context)
      .await
      .result;
    match result.outcome {
      crate::result::Outcome::Error { error } => assert_eq!(error.name.as_deref(), Some("TargetClosedError")),
      other @ crate::result::Outcome::Ok { .. } => panic!("closed session executed: {other:?}"),
    }
  }
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn coordinated_close_cancels_active_runs_and_keeps_admission_closed_through_release() {
    use crate::vars::VarsStore as _;
    for body in ["await new Promise(() => {});", "while (true) {}"] {
      let table = SessionTable::new(1, None);
      let slot = table.acquire("sashoush").await.unwrap();
      let vars = slot.lock().await.vars();
      let running_slot = Arc::clone(&slot);
      let running =
        tokio::spawn(async move { run(&running_slot, &format!("vars.set('started', 'yes'); {body}"), None).await });
      tokio::time::timeout(Duration::from_secs(3), async {
        while vars.get("started").as_deref() != Some("yes") {
          tokio::task::yield_now().await;
        }
      })
      .await
      .unwrap();
      let (entered, entering) = tokio::sync::oneshot::channel();
      let (release, released) = tokio::sync::oneshot::channel();
      let mut closing = Box::pin(table.clear_after(async {
        entered.send(()).unwrap();
        released.await.unwrap();
        Ok(())
      }));
      tokio::select! {
        result = &mut closing => panic!("close returned before bound-resource release: {result:?}"),
        result = entering => result.unwrap(),
      }
      assert!(table.acquire("another").await.is_err());
      let result = tokio::time::timeout(Duration::from_secs(3), running)
        .await
        .unwrap()
        .unwrap();
      match result.outcome {
        crate::result::Outcome::Error { error } => assert_eq!(error.name.as_deref(), Some("TargetClosedError")),
        other @ crate::result::Outcome::Ok { .. } => panic!("run survived session cancellation: {other:?}"),
      }
      assert!(futures::poll!(&mut closing).is_pending());
      release.send(()).unwrap();
      closing.await.unwrap();
      let replacement = table.acquire("sashoush").await.unwrap();
      assert!(!Arc::ptr_eq(&slot, &replacement));
      assert_ok(&run(&replacement, "return 42;", None).await, serde_json::json!(42));
      table.close().await.unwrap();
    }
  }

  #[tokio::test]
  async fn failed_bound_resource_release_preserves_dependencies_for_an_explicit_retry() {
    let table = SessionTable::new(1, None);
    let slot = table.acquire("sashoush").await.unwrap();
    let resources = start_owned_process(&slot).await;
    let error = table
      .clear_after(async { Err(ScriptError::internal("sashoush release failed")) })
      .await
      .unwrap_err();
    assert!(error.message.contains("sashoush release failed"));
    assert_eq!(resources.procs.status("sashoush").unwrap()["running"], true);
    assert!(table.contains("sashoush"));
    assert!(table.acquire("another").await.is_err());
    assert!(run(&slot, "return 1;", None).await.is_err());
    table.clear_after(async { Ok(()) }).await.unwrap();
    assert!(resources.procs.status("sashoush").is_err());
    table.acquire("another").await.unwrap();
    table.close().await.unwrap();
  }
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn direct_session_close_waits_for_active_runtime_cancellation() {
    let (_root, context) = ctx_with(Arc::new(InMemoryVars::new()));
    let vars = Arc::clone(&context.vars);
    let session = Arc::new(Session::create(ScriptEngineConfig::default(), &context).await.unwrap());
    let executing = Arc::clone(&session);
    let running = tokio::spawn(async move {
      executing
        .execute(
          "vars.set('started', 'yes'); while (true) {}",
          &[],
          RunOptions::default(),
          &context,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
      while vars.get("started").as_deref() != Some("yes") {
        tokio::task::yield_now().await;
      }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), session.close())
      .await
      .unwrap()
      .unwrap();
    assert!(
      session.poisoned(),
      "close returned before the active runtime was cancelled"
    );
    let result = running.await.unwrap().result;
    match result.outcome {
      crate::result::Outcome::Error { error } => assert_eq!(error.name.as_deref(), Some("TargetClosedError")),
      other @ crate::result::Outcome::Ok { .. } => panic!("run survived session close: {other:?}"),
    }
  }

  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn cancelled_call_rebuilds_the_vm_on_the_next_run_and_keeps_durable_resources() {
    use crate::vars::VarsStore as _;
    let table = SessionTable::new(1, None);
    let slot = table.acquire("sashoush").await.unwrap();
    let resources = start_owned_process(&slot).await;
    let pid = resources.procs.status("sashoush").unwrap()["pid"].clone();
    let vars = slot.lock().await.vars();
    let executing = Arc::clone(&slot);
    let running = tokio::spawn(async move {
      run(
        &executing,
        "globalThis.old = 1; vars.set('started', 'yes'); await new Promise(() => {});",
        None,
      )
      .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
      while vars.get("started").as_deref() != Some("yes") {
        tokio::task::yield_now().await;
      }
    })
    .await
    .unwrap();
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    assert_eq!(resources.procs.status("sashoush").unwrap()["pid"], pid);
    assert_eq!(resources.procs.status("sashoush").unwrap()["running"], true);
    assert_ok(
      &run(
        &slot,
        "return {old: typeof globalThis.old, kept: vars.get('started')};",
        None,
      )
      .await,
      serde_json::json!({"old":"undefined","kept":"yes"}),
    );
    table.close().await.unwrap();
    assert!(resources.procs.status("sashoush").is_err());
  }
}

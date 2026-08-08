//! Per-worker pool of pre-created browser contexts.
//!
//! A fresh context plus its first page costs ~42ms on Chromium, and
//! almost none of that is protocol work: `Target.createBrowserContext`
//! measures 0.6ms and `Target.createTarget` 1.7ms, while the flight of
//! session-setup commands that follows takes ~40ms because it can only
//! complete once the renderer process is up. Playwright pays the same
//! spawn, which is why per-test browser setup benchmarks at parity
//! between the two runners even though ferridriver's own dispatch is an
//! order of magnitude faster.
//!
//! Spawns overlap — 8 concurrent context+page creations measure 99ms
//! against 8 serial ones at 42ms each — so the cost is a latency that can
//! be hidden behind the running test rather than a floor. This pool
//! creates the next test's context while the current test is still
//! running and hands it over ready.
//!
//! Isolation is unchanged: every test still receives a context and page
//! that no other test has touched, created with that test's own options.
//! The only difference is when the creation happened. A pooled context is
//! [unlisted](ferridriver::Browser::new_context_unlisted) until it is
//! handed out, so a running test never sees the next test's container in
//! `browser.contexts()` or via `browser.on('context')`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::{Mutex, mpsc};

/// A context and its page, created ahead of the test that will use them.
struct Prepared {
  key: String,
  ctx: Arc<ferridriver::ContextRef>,
  page: Arc<ferridriver::Page>,
  /// Whether [`warm_layout`] has already run on this page.
  warm: bool,
}

/// Contexts are only interchangeable when they were created from
/// identical options — WebKit latches languages at target spawn and
/// document-time overrides (locale, timezone, `userAgent`) must be in the
/// bag before the first page's process starts, so a context built for one
/// `use` bag can never be handed to a test that asked for another. The
/// key is the serialized effective config; a miss just means the test
/// pays for its own context, exactly as it did before this pool existed.
pub(crate) type PoolKey = String;

/// One worker's pool. A worker runs a single test at a time, so the
/// receiving half stays single-consumer and the bookkeeping below is
/// exact rather than best-effort.
pub(crate) struct ContextPool {
  depth: usize,
  inbox: Mutex<Inbox>,
  /// Pre-creation stops for the rest of the worker's life once a refill
  /// fails: nothing was waiting on it, so there is no test to attribute
  /// the failure to, and retrying into a dead browser every test would
  /// bury the real error. The next `acquire` creates inline and surfaces
  /// it against the test that actually hit it.
  disabled: AtomicBool,
  hits: AtomicUsize,
  misses: AtomicUsize,
}

/// A pre-created context coming back to the pool, tagged with which
/// counter it was spawned against.
struct Delivery {
  /// True when this entry was already in the pool and merely went away to
  /// be warmed. Kept apart from creations because `spawn_refills` sizes
  /// the pool from creations alone — counting a promotion as one would
  /// make the pool believe it is already being topped up and leave it
  /// short exactly when it is busiest.
  promotion: bool,
  result: ferridriver::error::Result<Prepared>,
}

struct Inbox {
  ready: VecDeque<Prepared>,
  rx: mpsc::UnboundedReceiver<Delivery>,
  tx: mpsc::UnboundedSender<Delivery>,
  /// Creations spawned but not yet received. Every spawned task sends
  /// exactly one message, so this is `spawned - received`.
  in_flight: usize,
  /// Entries currently out of the queue being warmed.
  promoting: usize,
}

impl ContextPool {
  pub(crate) fn new(depth: usize) -> Arc<Self> {
    let (tx, rx) = mpsc::unbounded_channel();
    Arc::new(Self {
      depth,
      inbox: Mutex::new(Inbox {
        ready: VecDeque::new(),
        rx,
        tx,
        in_flight: 0,
        promoting: 0,
      }),
      disabled: AtomicBool::new(false),
      hits: AtomicUsize::new(0),
      misses: AtomicUsize::new(0),
    })
  }

  /// Take a context+page for `key`.
  ///
  /// Prefers a pre-created one, then one already being created, and only
  /// creates inline when neither exists. Waiting on an in-flight refill
  /// beats starting another creation: renderer spawns contend for CPU, so
  /// adding a third only makes all of them land later.
  pub(crate) async fn acquire(
    self: &Arc<Self>,
    browser: &Arc<ferridriver::Browser>,
    key: &str,
    opts: &ferridriver::options::BrowserContextOptions,
    backend: ferridriver::backend::BackendKind,
  ) -> ferridriver::error::Result<(Arc<ferridriver::ContextRef>, Arc<ferridriver::Page>)> {
    let prepared = loop {
      let mut inbox = self.inbox.lock().await;
      inbox.drain_arrivals();
      if let Some(prepared) = inbox.take(key) {
        self.hits.fetch_add(1, Ordering::Relaxed);
        break prepared;
      }
      self.spawn_refills(&mut inbox, browser, key, opts, backend);
      self.spawn_promotions(&mut inbox);
      if inbox.in_flight == 0 {
        self.misses.fetch_add(1, Ordering::Relaxed);
        drop(inbox);
        break Box::pin(create_prepared(browser, key.to_string(), opts.clone(), backend)).await?;
      }
      // Single-consumer: this worker is the only `acquire` in flight, so
      // awaiting here cannot strand another test's arrival.
      inbox.await_one().await;
    };

    browser.publish_context(&prepared.ctx);
    Ok((prepared.ctx, prepared.page))
  }

  /// Top the pool up to `depth`, counting what is already in flight.
  fn spawn_refills(
    self: &Arc<Self>,
    inbox: &mut Inbox,
    browser: &Arc<ferridriver::Browser>,
    key: &str,
    opts: &ferridriver::options::BrowserContextOptions,
    backend: ferridriver::backend::BackendKind,
  ) {
    if self.depth == 0 || self.disabled.load(Ordering::Relaxed) {
      return;
    }
    let want = self.depth.saturating_sub(inbox.ready.len() + inbox.in_flight);
    for _ in 0..want {
      inbox.in_flight += 1;
      let tx = inbox.tx.clone();
      let browser = Arc::clone(browser);
      let key = key.to_string();
      let opts = opts.clone();
      tokio::spawn(async move {
        let result = Box::pin(create_prepared(&browser, key, opts, backend)).await;
        let _ = tx.send(Delivery {
          promotion: false,
          result,
        });
      });
    }
  }

  /// Lay out the renderers of entries the pool can spare.
  ///
  /// Warming is worth a real Blink layout only for a test that renders, and
  /// it is never worth making a waiting test wait longer. Doing it here —
  /// after the entry is already pre-created, on a spare nobody is about to
  /// take, and always leaving one entry takeable — means a suite that
  /// renders gets warm pages while a suite that does not is never delayed
  /// by them.
  ///
  /// The entry leaves the queue for the duration, so no test can observe
  /// the throwaway markup mid-warm-up.
  fn spawn_promotions(self: &Arc<Self>, inbox: &mut Inbox) {
    if self.disabled.load(Ordering::Relaxed) {
      return;
    }
    // Leaves one entry in the queue, so a test arriving mid-promotion
    // still finds something to take.
    while inbox.ready.len() > 1 {
      let Some(i) = inbox.ready.iter().position(|p| !p.warm) else {
        break;
      };
      let Some(mut prepared) = inbox.ready.remove(i) else {
        break;
      };
      inbox.promoting += 1;
      let tx = inbox.tx.clone();
      tokio::spawn(async move {
        warm_layout(&prepared.page).await;
        prepared.warm = true;
        let _ = tx.send(Delivery {
          promotion: true,
          result: Ok(prepared),
        });
      });
    }
  }

  /// Close every context the pool holds or is still creating, so the
  /// browser does not shut down with pooled targets open under it.
  pub(crate) async fn drain(&self) {
    self.disabled.store(true, Ordering::Relaxed);
    let mut inbox = self.inbox.lock().await;
    tracing::debug!(
      target: "ferridriver::worker",
      hits = self.hits.load(Ordering::Relaxed),
      misses = self.misses.load(Ordering::Relaxed),
      "context pool",
    );
    while inbox.in_flight + inbox.promoting > 0 {
      inbox.await_one().await;
    }
    let held: Vec<Prepared> = inbox.ready.drain(..).collect();
    drop(inbox);
    for prepared in held {
      let _ = prepared.ctx.close().await;
    }
  }
}

impl Inbox {
  /// Move everything the spawned refills have delivered into `ready`.
  fn drain_arrivals(&mut self) {
    while let Ok(msg) = self.rx.try_recv() {
      self.accept(msg);
    }
  }

  /// Block until one more delivery lands. Only called with something
  /// outstanding, and every spawned task sends exactly one message, so
  /// this cannot wait forever.
  async fn await_one(&mut self) {
    if let Some(msg) = self.rx.recv().await {
      self.accept(msg);
    }
  }

  fn accept(&mut self, msg: Delivery) {
    if msg.promotion {
      self.promoting = self.promoting.saturating_sub(1);
    } else {
      self.in_flight = self.in_flight.saturating_sub(1);
    }
    match msg.result {
      Ok(prepared) => self.ready.push_back(prepared),
      Err(e) => tracing::debug!(target: "ferridriver::worker", "context prewarm failed: {e}"),
    }
  }

  /// Scan rather than pop: a per-test `use` bag can push a foreign key
  /// into a pool otherwise full of the suite default, and dropping the
  /// matching entries behind it would make every later test miss.
  fn take(&mut self, key: &str) -> Option<Prepared> {
    let i = self
      .ready
      .iter()
      .position(|p| p.key == key && p.warm)
      .or_else(|| self.ready.iter().position(|p| p.key == key))?;
    self.ready.remove(i)
  }
}

async fn create_prepared(
  browser: &Arc<ferridriver::Browser>,
  key: String,
  opts: ferridriver::options::BrowserContextOptions,
  backend: ferridriver::backend::BackendKind,
) -> ferridriver::error::Result<Prepared> {
  match open(browser, &key, opts.clone(), backend).await {
    Ok(prepared) => Ok(prepared),
    // Firefox occasionally hands back a BrowsingContext whose Window is
    // not wired up yet; the un-pooled path retries once for the same
    // reason (`is_retryable_bidi_page_error`).
    Err(e) if crate::worker::is_retryable_bidi_page_error(&e) => open(browser, &key, opts, backend).await,
    Err(e) => Err(e),
  }
}

async fn open(
  browser: &Arc<ferridriver::Browser>,
  key: &str,
  opts: ferridriver::options::BrowserContextOptions,
  backend: ferridriver::backend::BackendKind,
) -> ferridriver::error::Result<Prepared> {
  let ctx = Arc::new(browser.new_context_unlisted().options(opts).await?);
  match crate::worker::create_ready_page(&ctx, backend).await {
    Ok(page) => Ok(Prepared {
      key: key.to_string(),
      ctx,
      page,
      warm: false,
    }),
    Err(e) => {
      let _ = ctx.close().await;
      Err(e)
    },
  }
}

/// Lay out flowed text and a form control once, then undo it, reading
/// `offsetHeight` in between to force the layout synchronously rather than
/// leaving it for the next frame.
///
/// One expression, so this is a single round trip. Going through
/// `set_content` instead costs three round trips each way, which on a
/// supply-limited pool takes back more than the warm-up wins.
const LAYOUT_WARMUP_JS: &str = "(()=>{const b=document.body;if(!b)return 0;\
const h=b.innerHTML;b.innerHTML='<h1>a</h1><input>';\
const n=b.offsetHeight;b.innerHTML=h;return n})()";

/// Force the renderer's first layout before the test gets the page.
///
/// A renderer charges for its first text layout and its first form-control
/// layout, and it charges a lot: the same `setContent` measures 39.4ms on a
/// fresh renderer, 1.2ms the second time, and 1.9ms the first time if a
/// throwaway document laid out text and an `<input>` beforehand. That cost
/// is Blink's, so it lands on whichever driver renders first — paying it
/// here spends the pool's idle time instead of the test's.
///
/// The markup is removed again, which keeps the warmth (it belongs to the
/// renderer, not the document) while handing over a page that still reads
/// as untouched: `about:blank`, the body it started with, and
/// `history.length == 1` — nothing here navigates.
///
/// Best-effort — a page that cannot be warmed is still a usable page, and
/// failing the pre-creation over it would fail a test that has no idea
/// this happened.
async fn warm_layout(page: &Arc<ferridriver::Page>) {
  let _ = page.inner().evaluate(LAYOUT_WARMUP_JS).await;
}

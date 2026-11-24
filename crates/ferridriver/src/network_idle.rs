//! Playwright's `networkidle` lifecycle event
//! (`/tmp/playwright/packages/playwright-core/src/server/frames.ts`): a
//! frame goes network-idle once 500ms pass with none of its requests in
//! flight, and counts as idle when it and every frame under it have. It
//! fires once per document and stays fired, so
//! `waitForLoadState('networkidle')` after that returns at once rather
//! than waiting for the requests the page has started since.
//!
//! Fed from a page's own events by its state observer, so every backend
//! that reports requests keeps it.

use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use crate::events::PageEvent;
use crate::network::Request;

const QUIET: Duration = Duration::from_millis(500);

#[derive(Default)]
pub(crate) struct NetworkIdle {
  frames: Mutex<FxHashMap<Arc<str>, FrameIdle>>,
  timers: AtomicU64,
  changed: tokio::sync::Notify,
}

#[derive(Default)]
struct FrameIdle {
  parent: Option<Arc<str>>,
  /// Requests in flight that hold the frame busy: all but favicons and
  /// event streams (`frame._networkIdleRequests`).
  requests: Vec<Request>,
  /// The running quiet timer, `None` while requests are in flight.
  timer: Option<u64>,
  /// 500ms passed with nothing in flight (`_firedNetworkIdleSelf`).
  quiet: bool,
  /// The frame and its subtree went idle in this document.
  fired: bool,
}

impl NetworkIdle {
  /// Apply one page event. Runs in the page's state observer, before
  /// the event reaches any public listener.
  pub(crate) fn observe(self: &Arc<Self>, event: &PageEvent) {
    match event {
      PageEvent::Request(request) => self.request_started(request),
      PageEvent::RequestFinished(request) | PageEvent::RequestFailed(request) => self.request_ended(request),
      PageEvent::FrameAttached(info) => {
        let mut frames = self.lock();
        let frame = frames.entry(Arc::from(info.frame_id.as_str())).or_default();
        frame.parent = info.parent_frame_id.as_deref().map(Arc::from);
        if frame.requests.is_empty() && frame.timer.is_none() && !frame.quiet {
          self.start_timer(info.frame_id.as_str(), frame);
        }
      },
      PageEvent::FrameNavigated(info) => self.committed(&info.frame_id, info.parent_frame_id.as_deref()),
      PageEvent::FrameDetached { frame_id } => {
        let mut frames = self.lock();
        let mut gone = vec![Arc::<str>::from(frame_id.as_str())];
        while let Some(id) = gone.pop() {
          frames.remove(&id);
          gone.extend(
            frames
              .iter()
              .filter(|(_, frame)| frame.parent.as_deref() == Some(&*id))
              .map(|(child, _)| Arc::clone(child)),
          );
        }
        self.recalculate(&mut frames, None);
      },
      PageEvent::Load | PageEvent::DomContentLoaded => {
        let mut frames = self.lock();
        self.recalculate(&mut frames, None);
      },
      _ => {},
    }
  }

  /// Resolve once `frame_id`'s document has gone network-idle; at once
  /// when it already has. A frame not seen yet starts its quiet window now.
  pub(crate) async fn wait(self: &Arc<Self>, frame_id: &str) {
    loop {
      let changed = self.changed.notified();
      {
        let mut frames = self.lock();
        match frames.get_mut(frame_id) {
          Some(frame) if frame.fired => return,
          Some(_) => {},
          None => {
            let frame = frames.entry(Arc::from(frame_id)).or_default();
            self.start_timer(frame_id, frame);
          },
        }
      }
      changed.await;
    }
  }

  fn request_started(self: &Arc<Self>, request: &Request) {
    if is_favicon(request) || request.resource_type().eq_ignore_ascii_case("eventsource") {
      return;
    }
    let Some(frame_id) = request.frame_id() else {
      return;
    };
    let mut frames = self.lock();
    let frame = frames.entry(Arc::from(frame_id)).or_default();
    frame.requests.push(request.clone());
    if frame.requests.len() == 1 {
      frame.timer = None;
      frame.quiet = false;
    }
  }

  fn request_ended(self: &Arc<Self>, request: &Request) {
    let Some(frame_id) = request.frame_id() else {
      return;
    };
    let mut frames = self.lock();
    let Some(frame) = frames.get_mut(frame_id) else {
      return;
    };
    let before = frame.requests.len();
    frame.requests.retain(|held| !held.same_request(request));
    if before != frame.requests.len() && frame.requests.is_empty() {
      self.start_timer(frame_id, frame);
    }
  }

  /// A new document committed in the frame: what fired for the previous
  /// one no longer counts, and only the document's own request still
  /// holds it (`frame._onClearLifecycle`).
  fn committed(self: &Arc<Self>, frame_id: &str, parent: Option<&str>) {
    let mut frames = self.lock();
    let id: Arc<str> = Arc::from(frame_id);
    let frame = frames.entry(Arc::clone(&id)).or_default();
    frame.parent = parent.map(Arc::from);
    frame.fired = false;
    frame.quiet = false;
    frame.timer = None;
    frame.requests.retain(Request::is_navigation_request);
    if frame.requests.is_empty() {
      self.start_timer(frame_id, frame);
    }
    self.recalculate(&mut frames, Some(&id));
  }

  fn start_timer(self: &Arc<Self>, frame_id: &str, frame: &mut FrameIdle) {
    if frame.fired {
      return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
      return;
    };
    let timer = self.timers.fetch_add(1, Ordering::Relaxed);
    frame.timer = Some(timer);
    let this: Weak<Self> = Arc::downgrade(self);
    let frame_id: Arc<str> = Arc::from(frame_id);
    runtime.spawn(async move {
      tokio::time::sleep(QUIET).await;
      if let Some(this) = this.upgrade() {
        this.timer_fired(&frame_id, timer);
      }
    });
  }

  fn timer_fired(&self, frame_id: &str, timer: u64) {
    let mut frames = self.lock();
    let Some(frame) = frames.get_mut(frame_id) else {
      return;
    };
    if frame.timer != Some(timer) {
      return;
    }
    frame.timer = None;
    frame.quiet = true;
    self.recalculate(&mut frames, None);
  }

  /// Settle every frame's idleness from its subtree's
  /// (`frame._recalculateNetworkIdle`). Fired stays fired, except where
  /// a subtree that went busy again un-fires a frame other than the one
  /// whose commit asked for the recalculation.
  fn recalculate(&self, frames: &mut FxHashMap<Arc<str>, FrameIdle>, committed: Option<&Arc<str>>) {
    let roots: Vec<Arc<str>> = frames
      .iter()
      .filter(|(_, frame)| frame.parent.as_ref().is_none_or(|parent| !frames.contains_key(parent)))
      .map(|(id, _)| Arc::clone(id))
      .collect();
    let mut changed = false;
    for root in roots {
      settle(frames, &root, committed, &mut changed);
    }
    if changed {
      self.changed.notify_waiters();
    }
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, FxHashMap<Arc<str>, FrameIdle>> {
    self.frames.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// Whether `id` and its subtree are idle, firing and un-firing on the way.
fn settle(
  frames: &mut FxHashMap<Arc<str>, FrameIdle>,
  id: &Arc<str>,
  committed: Option<&Arc<str>>,
  changed: &mut bool,
) -> bool {
  let children: Vec<Arc<str>> = frames
    .iter()
    .filter(|(_, frame)| frame.parent.as_ref() == Some(id))
    .map(|(child, _)| Arc::clone(child))
    .collect();
  let mut idle = frames.get(id).is_some_and(|frame| frame.quiet);
  for child in &children {
    settle(frames, child, committed, changed);
    if !frames.get(child).is_some_and(|frame| frame.fired) {
      idle = false;
    }
  }
  let Some(frame) = frames.get_mut(id) else {
    return false;
  };
  if idle && !frame.fired {
    frame.fired = true;
    *changed = true;
  }
  if frame.fired && !idle && committed != Some(id) {
    frame.fired = false;
    *changed = true;
  }
  frame.fired
}

/// Playwright never lets a favicon hold a frame busy (`request._isFavicon`).
fn is_favicon(request: &Request) -> bool {
  let mut hop = Some(request.clone());
  while let Some(request) = hop {
    if request.url().ends_with("/favicon.ico") {
      return true;
    }
    hop = request.redirected_from();
  }
  false
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::backend::FrameInfo;

  fn request(id: &str, frame: &str, url: &str, resource_type: &str) -> Request {
    Request::new(crate::network::RequestInit {
      id: id.into(),
      url: url.into(),
      method: "GET".into(),
      resource_type: resource_type.into(),
      is_navigation_request: resource_type == "Document",
      post_data: None,
      headers: crate::network::Headers::default(),
      frame_id: Some(frame.into()),
      page_guid: None,
      redirected_from: None,
      timing: None,
      raw_headers_fn: None,
    })
  }

  fn main_frame(id: &str) -> PageEvent {
    PageEvent::FrameNavigated(FrameInfo {
      frame_id: id.into(),
      parent_frame_id: None,
      name: String::new(),
      url: "http://example.com/".into(),
    })
  }

  async fn fired(idle: &Arc<NetworkIdle>, frame: &str) -> bool {
    tokio::time::timeout(Duration::from_millis(1), idle.wait(frame))
      .await
      .is_ok()
  }

  #[tokio::test(start_paused = true)]
  async fn fires_after_quiet_and_stays_fired_while_new_requests_run() {
    let idle = Arc::new(NetworkIdle::default());
    idle.observe(&main_frame("main"));
    let data = request("1", "main", "http://example.com/data", "Fetch");
    idle.observe(&PageEvent::Request(data.clone()));
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!fired(&idle, "main").await, "busy while a request is in flight");

    idle.observe(&PageEvent::RequestFinished(data));
    tokio::time::sleep(Duration::from_millis(499)).await;
    assert!(!fired(&idle, "main").await, "not before 500ms of quiet");
    tokio::time::sleep(Duration::from_millis(2)).await;
    assert!(fired(&idle, "main").await);

    idle.observe(&PageEvent::Request(request(
      "2",
      "main",
      "http://example.com/more",
      "Fetch",
    )));
    assert!(fired(&idle, "main").await, "fired once per document");
  }

  #[tokio::test(start_paused = true)]
  async fn a_new_document_waits_again_and_a_busy_child_holds_its_parent() {
    let idle = Arc::new(NetworkIdle::default());
    idle.observe(&main_frame("main"));
    tokio::time::sleep(QUIET * 2).await;
    assert!(fired(&idle, "main").await);

    idle.observe(&main_frame("main"));
    assert!(!fired(&idle, "main").await, "a commit clears it");
    idle.observe(&PageEvent::FrameAttached(FrameInfo {
      frame_id: "child".into(),
      parent_frame_id: Some("main".into()),
      name: String::new(),
      url: String::new(),
    }));
    let image = request("3", "child", "http://example.com/a.png", "Image");
    idle.observe(&PageEvent::Request(image.clone()));
    tokio::time::sleep(QUIET * 2).await;
    assert!(!fired(&idle, "main").await, "the child is still loading");

    idle.observe(&PageEvent::RequestFailed(image));
    tokio::time::sleep(QUIET * 2).await;
    assert!(fired(&idle, "main").await);
  }

  #[tokio::test(start_paused = true)]
  async fn favicons_and_event_streams_never_hold_a_frame() {
    let idle = Arc::new(NetworkIdle::default());
    idle.observe(&main_frame("main"));
    idle.observe(&PageEvent::Request(request(
      "4",
      "main",
      "http://example.com/favicon.ico",
      "Image",
    )));
    idle.observe(&PageEvent::Request(request(
      "5",
      "main",
      "http://example.com/events",
      "EventSource",
    )));
    tokio::time::sleep(QUIET * 2).await;
    assert!(fired(&idle, "main").await);
  }
}

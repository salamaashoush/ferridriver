//! One screencast per page, shared by every consumer.
//!
//! A page streams at most one screencast: Chromium rejects a second
//! `Page.startScreencast` with "Screencast is already active", and a
//! `Page.stopScreencast` ends the stream for whoever else was reading it.
//! The trace film strip, the video recorder and the user-facing
//! `page.startScreencast` therefore subscribe to a per-page
//! [`ScreencastHub`] instead of driving the backend themselves. The first
//! subscriber starts the backend stream, later ones attach to it, a
//! subscriber that needs larger or better frames restarts it with the
//! union of every request, and the last one to leave stops it.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, PoisonError};
use std::time::Instant;

use tokio::sync::{mpsc, oneshot};

use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScreencastParams {
  pub quality: u8,
  pub max_width: u32,
  pub max_height: u32,
}

impl ScreencastParams {
  pub(crate) fn new(quality: u8, max_width: u32, max_height: u32) -> Self {
    Self {
      quality,
      max_width,
      max_height,
    }
  }

  fn covers(self, other: Self) -> bool {
    self.quality >= other.quality && self.max_width >= other.max_width && self.max_height >= other.max_height
  }

  fn union(self, other: Self) -> Self {
    Self {
      quality: self.quality.max(other.quality),
      max_width: self.max_width.max(other.max_width),
      max_height: self.max_height.max(other.max_height),
    }
  }
}

#[derive(Debug, Clone)]
pub(crate) struct ScreencastFrame {
  pub data: Vec<u8>,
  /// The backend's own frame timestamp, in seconds. Its clock differs per
  /// backend, so wall-time consumers use `received_at` instead.
  pub timestamp: f64,
  pub received_at: Instant,
}

/// A backend frame stream plus the signal that ends it. After the signal
/// the backend forwards the frames it already has, then closes the stream.
pub(crate) type RawScreencast = (mpsc::UnboundedReceiver<(Vec<u8>, f64)>, oneshot::Sender<()>);

pub(crate) trait ScreencastSource: Clone + Send + Sync + 'static {
  fn start_stream(&self, params: ScreencastParams) -> impl Future<Output = Result<RawScreencast>> + Send;
  fn stop_stream(&self) -> impl Future<Output = Result<()>> + Send;
  fn is_closed(&self) -> bool;
}

/// What a subscription needs of its page after subscribing: stopping the
/// stream when it is the last one out. Type-erased so a subscription does
/// not carry the backend page's type; a trace recorder holds subscriptions,
/// and with the page's type inside it every `Send` proof that reaches a
/// recorder walks the whole page graph again, past the solver's depth.
pub(crate) trait StreamStop: Send + Sync {
  fn stop_stream(&self) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
  fn is_closed(&self) -> bool;
}

impl<S: ScreencastSource> StreamStop for S {
  fn stop_stream(&self) -> std::pin::Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
    Box::pin(ScreencastSource::stop_stream(self))
  }

  fn is_closed(&self) -> bool {
    ScreencastSource::is_closed(self)
  }
}

#[derive(Debug, PartialEq, Eq)]
enum Plan {
  Attach,
  Start(ScreencastParams),
  Restart(ScreencastParams),
}

fn plan(running: Option<ScreencastParams>, wanted: ScreencastParams) -> Plan {
  match running {
    None => Plan::Start(wanted),
    Some(current) if current.covers(wanted) => Plan::Attach,
    Some(current) => Plan::Restart(current.union(wanted)),
  }
}

#[derive(Clone, Default)]
pub(crate) struct ScreencastHub {
  control: Arc<tokio::sync::Mutex<Control>>,
  fanout: Arc<StdMutex<Fanout>>,
}

#[derive(Default)]
struct Control {
  next_id: u64,
  running: Option<Running>,
  /// The subscription behind the user-facing `page.startScreencast`,
  /// which `page.stopScreencast` ends.
  user: Option<u64>,
}

struct Running {
  params: ScreencastParams,
  stopping: Arc<AtomicBool>,
  ended: Arc<AtomicBool>,
  shutdown: oneshot::Sender<()>,
  task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct Fanout {
  subscribers: Vec<Subscriber>,
  /// Replayed to a subscriber that attaches to a running stream: an idle
  /// page sends no frames, so without it the newcomer would see nothing
  /// until the page next repaints.
  last_frame: Option<ScreencastFrame>,
}

struct Subscriber {
  id: u64,
  tx: Option<mpsc::UnboundedSender<ScreencastFrame>>,
}

impl Fanout {
  fn publish(&mut self, frame: ScreencastFrame) {
    for sub in &self.subscribers {
      if let Some(tx) = &sub.tx {
        let _ = tx.send(frame.clone());
      }
    }
    self.last_frame = Some(frame);
  }

  fn remove(&mut self, id: u64) {
    self.subscribers.retain(|s| s.id != id);
  }
}

impl ScreencastHub {
  fn fanout(&self) -> MutexGuard<'_, Fanout> {
    self.fanout.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Subscribe to the page's screencast, starting or upgrading the backend
  /// stream as needed. Frames arrive on the returned receiver until the
  /// subscription stops; [`ScreencastSubscription::stop`] delivers the
  /// frames the backend emitted before it stopped.
  pub(crate) async fn subscribe<S: ScreencastSource>(
    &self,
    source: &S,
    params: ScreencastParams,
  ) -> Result<(ScreencastSubscription, mpsc::UnboundedReceiver<ScreencastFrame>)> {
    let mut control = self.control.lock().await;
    if control
      .running
      .as_ref()
      .is_some_and(|r| r.ended.load(Ordering::Acquire))
    {
      control.running = None;
    }
    let id = control.next_id;
    control.next_id += 1;
    let (tx, rx) = mpsc::unbounded_channel();
    let subscriber = Subscriber { id, tx: Some(tx) };

    match plan(control.running.as_ref().map(|r| r.params), params) {
      Plan::Attach => {
        let mut fanout = self.fanout();
        if let (Some(frame), Some(tx)) = (fanout.last_frame.clone(), &subscriber.tx) {
          let _ = tx.send(frame);
        }
        fanout.subscribers.push(subscriber);
      },
      Plan::Start(wanted) => {
        self.fanout().subscribers.push(subscriber);
        if let Err(e) = self.start_running(&mut control, source, wanted).await {
          self.fanout().remove(id);
          return Err(e);
        }
      },
      Plan::Restart(wanted) => {
        let previous = control.running.as_ref().map_or(wanted, |r| r.params);
        self.stop_running(&mut control, source).await;
        self.fanout().subscribers.push(subscriber);
        if let Err(e) = self.start_running(&mut control, source, wanted).await {
          tracing::debug!(target: "ferridriver::screencast", "restart at {wanted:?} failed ({e}); keeping {previous:?}");
          if let Err(e) = self.start_running(&mut control, source, previous).await {
            let mut fanout = self.fanout();
            fanout.remove(id);
            for sub in &mut fanout.subscribers {
              sub.tx = None;
            }
            return Err(e);
          }
        }
      },
    }

    let subscription = ScreencastSubscription {
      hub: self.clone(),
      source: Arc::new(source.clone()) as Arc<dyn StreamStop>,
      id,
      active: true,
    };
    Ok((subscription, rx))
  }

  async fn unsubscribe(&self, source: &dyn StreamStop, id: u64) {
    let mut control = self.control.lock().await;
    let others = self.fanout().subscribers.iter().any(|s| s.id != id);
    if !others {
      // Still subscribed while the stream drains, so the frames the
      // backend emitted before it stopped reach this last consumer.
      self.stop_running(&mut control, source).await;
      self.fanout().last_frame = None;
    }
    self.fanout().remove(id);
    if control.user == Some(id) {
      control.user = None;
    }
  }

  async fn start_running<S: ScreencastSource>(
    &self,
    control: &mut Control,
    source: &S,
    params: ScreencastParams,
  ) -> Result<()> {
    // Boxed: the backend dispatch future is sized for the largest
    // backend and would otherwise inflate every caller's future.
    let (mut raw, shutdown) = Box::pin(source.start_stream(params)).await?;
    let stopping = Arc::new(AtomicBool::new(false));
    let stop_requested = Arc::clone(&stopping);
    let ended = Arc::new(AtomicBool::new(false));
    let stream_ended = Arc::clone(&ended);
    let fanout = Arc::clone(&self.fanout);
    let task = tokio::spawn(async move {
      while let Some((data, timestamp)) = raw.recv().await {
        let frame = ScreencastFrame {
          data,
          timestamp,
          received_at: Instant::now(),
        };
        fanout.lock().unwrap_or_else(PoisonError::into_inner).publish(frame);
      }
      let mut fanout = fanout.lock().unwrap_or_else(PoisonError::into_inner);
      stream_ended.store(true, Ordering::Release);
      // The backend ended the stream on its own (page closed): end every
      // subscriber's stream too, as a direct backend consumer would see.
      if !stop_requested.load(Ordering::Acquire) {
        for sub in &mut fanout.subscribers {
          sub.tx = None;
        }
      }
    });
    control.running = Some(Running {
      params,
      stopping,
      ended,
      shutdown,
      task,
    });
    Ok(())
  }

  async fn stop_running(&self, control: &mut Control, source: &dyn StreamStop) {
    let Some(running) = control.running.take() else {
      return;
    };
    running.stopping.store(true, Ordering::Release);
    // A closed target never answers; on cdp-raw the stop would wait out
    // the 30s command timeout.
    if !source.is_closed()
      && let Err(e) = source.stop_stream().await
    {
      tracing::debug!(target: "ferridriver::screencast", "stop screencast: {e}");
    }
    let _ = running.shutdown.send(());
    let _ = running.task.await;
  }

  /// Back the user-facing `page.startScreencast`: one subscription per
  /// page, replaced by a later call and ended by [`Self::stop_user`].
  pub(crate) async fn start_user<S: ScreencastSource>(
    &self,
    source: &S,
    params: ScreencastParams,
  ) -> Result<mpsc::UnboundedReceiver<ScreencastFrame>> {
    self.stop_user(source).await;
    let (subscription, rx) = self.subscribe(source, params).await?;
    let id = subscription.detach();
    self.control.lock().await.user = Some(id);
    Ok(rx)
  }

  pub(crate) async fn stop_user<S: ScreencastSource>(&self, source: &S) {
    let user = self.control.lock().await.user.take();
    if let Some(id) = user {
      self.unsubscribe(source, id).await;
    }
  }
}

/// One consumer's hold on a page's screencast. Dropping it without
/// [`Self::stop`] unsubscribes in the background.
pub(crate) struct ScreencastSubscription {
  hub: ScreencastHub,
  source: Arc<dyn StreamStop>,
  id: u64,
  active: bool,
}

impl ScreencastSubscription {
  /// Leave the screencast. Returns once this subscriber's receiver has
  /// every frame it will get; the receiver then reports end-of-stream.
  pub(crate) async fn stop(mut self) {
    self.active = false;
    self.hub.unsubscribe(&*self.source, self.id).await;
  }

  fn detach(mut self) -> u64 {
    self.active = false;
    self.id
  }
}

impl Drop for ScreencastSubscription {
  fn drop(&mut self) {
    if !self.active {
      return;
    }
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
      let hub = self.hub.clone();
      let source = Arc::clone(&self.source);
      let id = self.id;
      runtime.spawn(async move { hub.unsubscribe(&*source, id).await });
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  type FrameSender = mpsc::UnboundedSender<(Vec<u8>, f64)>;

  #[derive(Default)]
  struct FakeState {
    starts: Vec<ScreencastParams>,
    stops: usize,
    closed: bool,
    fail_starts_above: Option<u32>,
    live: Option<FrameSender>,
  }

  /// Mirrors the CDP listener: frames flow until the shutdown signal, the
  /// ones already queued are still delivered, then the stream closes.
  #[derive(Clone, Default)]
  struct FakeSource(Arc<StdMutex<FakeState>>);

  impl FakeSource {
    fn state(&self) -> MutexGuard<'_, FakeState> {
      self.0.lock().unwrap()
    }

    fn emit(&self, byte: u8) {
      let state = self.state();
      state
        .live
        .as_ref()
        .unwrap()
        .send((vec![byte], f64::from(byte)))
        .unwrap();
    }
  }

  impl ScreencastSource for FakeSource {
    fn start_stream(&self, params: ScreencastParams) -> impl Future<Output = Result<RawScreencast>> + Send {
      let mut state = self.state();
      state.starts.push(params);
      if state.fail_starts_above.is_some_and(|max| params.max_width > max) {
        return std::future::ready(Err(crate::error::FerriError::unsupported("restart refused")));
      }
      assert!(
        state.live.is_none(),
        "second backend screencast started while one is live"
      );
      let (inner_tx, mut inner_rx) = mpsc::unbounded_channel::<(Vec<u8>, f64)>();
      let (tx, rx) = mpsc::unbounded_channel();
      let (shutdown_tx, mut shutdown_rx) = oneshot::channel::<()>();
      state.live = Some(inner_tx);
      let fake = self.clone();
      tokio::spawn(async move {
        loop {
          tokio::select! {
            biased;
            frame = inner_rx.recv() => match frame {
              Some(frame) => { let _ = tx.send(frame); },
              None => break,
            },
            _ = &mut shutdown_rx => {
              while let Ok(frame) = inner_rx.try_recv() {
                let _ = tx.send(frame);
              }
              break;
            },
          }
        }
        fake.state().live = None;
      });
      std::future::ready(Ok((rx, shutdown_tx)))
    }

    fn stop_stream(&self) -> impl Future<Output = Result<()>> + Send {
      self.state().stops += 1;
      std::future::ready(Ok(()))
    }

    fn is_closed(&self) -> bool {
      self.state().closed
    }
  }

  fn params(quality: u8, w: u32, h: u32) -> ScreencastParams {
    ScreencastParams::new(quality, w, h)
  }

  async fn next(rx: &mut mpsc::UnboundedReceiver<ScreencastFrame>) -> u8 {
    rx.recv().await.expect("frame").data[0]
  }

  #[test]
  fn plan_starts_attaches_and_upgrades_to_the_union() {
    assert_eq!(plan(None, params(70, 800, 600)), Plan::Start(params(70, 800, 600)));
    assert_eq!(plan(Some(params(80, 1280, 720)), params(70, 800, 600)), Plan::Attach);
    assert_eq!(plan(Some(params(70, 800, 600)), params(70, 800, 600)), Plan::Attach);
    assert_eq!(
      plan(Some(params(70, 800, 600)), params(80, 1280, 480)),
      Plan::Restart(params(80, 1280, 600))
    );
    assert_eq!(
      plan(Some(params(90, 800, 600)), params(70, 800, 601)),
      Plan::Restart(params(90, 800, 601))
    );
  }

  #[tokio::test]
  async fn later_subscribers_share_one_backend_stream() {
    let source = FakeSource::default();
    let hub = ScreencastHub::default();
    let (trace, mut trace_rx) = hub.subscribe(&source, params(70, 800, 600)).await.unwrap();
    source.emit(1);
    assert_eq!(next(&mut trace_rx).await, 1);

    let (video, mut video_rx) = hub.subscribe(&source, params(60, 640, 480)).await.unwrap();
    assert_eq!(source.state().starts, vec![params(70, 800, 600)]);
    assert_eq!(next(&mut video_rx).await, 1, "attach replays the current frame");

    source.emit(2);
    assert_eq!(next(&mut trace_rx).await, 2);
    assert_eq!(next(&mut video_rx).await, 2);

    video.stop().await;
    assert!(video_rx.recv().await.is_none());
    assert_eq!(source.state().stops, 0, "a remaining subscriber keeps the stream");
    source.emit(3);
    assert_eq!(next(&mut trace_rx).await, 3);

    trace.stop().await;
    assert_eq!(source.state().stops, 1);
    assert!(trace_rx.recv().await.is_none());
    assert!(source.state().live.is_none());
  }

  #[tokio::test]
  async fn a_larger_request_restarts_with_the_union() {
    let source = FakeSource::default();
    let hub = ScreencastHub::default();
    let (_trace, mut trace_rx) = hub.subscribe(&source, params(70, 800, 600)).await.unwrap();
    let (_video, mut video_rx) = hub.subscribe(&source, params(80, 1280, 480)).await.unwrap();
    assert_eq!(source.state().starts, vec![params(70, 800, 600), params(80, 1280, 600)]);
    assert_eq!(source.state().stops, 1);

    source.emit(7);
    assert_eq!(
      next(&mut trace_rx).await,
      7,
      "the earlier subscriber survives the restart"
    );
    assert_eq!(next(&mut video_rx).await, 7);
  }

  #[tokio::test]
  async fn the_last_subscriber_gets_the_frames_queued_before_stop() {
    let source = FakeSource::default();
    let hub = ScreencastHub::default();
    let (video, mut rx) = hub.subscribe(&source, params(80, 800, 600)).await.unwrap();
    source.emit(1);
    source.emit(2);
    video.stop().await;
    assert_eq!(next(&mut rx).await, 1);
    assert_eq!(next(&mut rx).await, 2);
    assert!(rx.recv().await.is_none());
  }

  #[tokio::test]
  async fn a_closed_page_is_not_sent_a_stop() {
    let source = FakeSource::default();
    let hub = ScreencastHub::default();
    let (sub, _rx) = hub.subscribe(&source, params(80, 800, 600)).await.unwrap();
    source.state().closed = true;
    sub.stop().await;
    assert_eq!(source.state().stops, 0);
    assert!(source.state().live.is_none());
  }

  #[tokio::test]
  async fn a_refused_restart_keeps_the_running_parameters() {
    let source = FakeSource::default();
    source.state().fail_starts_above = Some(800);
    let hub = ScreencastHub::default();
    let (_trace, mut trace_rx) = hub.subscribe(&source, params(70, 800, 600)).await.unwrap();
    let (_video, mut video_rx) = hub.subscribe(&source, params(80, 1280, 720)).await.unwrap();
    assert_eq!(
      source.state().starts,
      vec![params(70, 800, 600), params(80, 1280, 720), params(70, 800, 600)]
    );
    source.emit(4);
    assert_eq!(next(&mut trace_rx).await, 4);
    assert_eq!(next(&mut video_rx).await, 4);
  }

  #[tokio::test]
  async fn a_failed_start_leaves_no_subscriber_behind() {
    let source = FakeSource::default();
    source.state().fail_starts_above = Some(0);
    let hub = ScreencastHub::default();
    assert!(hub.subscribe(&source, params(70, 800, 600)).await.is_err());
    assert!(hub.fanout().subscribers.is_empty());
    source.state().fail_starts_above = None;
    let (_sub, mut rx) = hub.subscribe(&source, params(70, 800, 600)).await.unwrap();
    source.emit(5);
    assert_eq!(next(&mut rx).await, 5);
  }

  #[tokio::test]
  async fn a_dropped_subscription_unsubscribes() {
    let source = FakeSource::default();
    let hub = ScreencastHub::default();
    let (sub, _rx) = hub.subscribe(&source, params(70, 800, 600)).await.unwrap();
    drop(sub);
    for _ in 0..100 {
      if source.state().stops == 1 {
        break;
      }
      tokio::task::yield_now().await;
    }
    assert_eq!(source.state().stops, 1);
    assert!(hub.fanout().subscribers.is_empty());
  }

  #[tokio::test]
  async fn a_stream_ended_by_the_backend_ends_every_subscriber_and_restarts_fresh() {
    let source = FakeSource::default();
    let hub = ScreencastHub::default();
    let (_a, mut a_rx) = hub.subscribe(&source, params(70, 800, 600)).await.unwrap();
    source.state().live = None;
    assert!(a_rx.recv().await.is_none());
    let (_b, mut b_rx) = hub.subscribe(&source, params(70, 800, 600)).await.unwrap();
    assert_eq!(source.state().starts.len(), 2);
    source.emit(9);
    assert_eq!(next(&mut b_rx).await, 9);
  }

  #[tokio::test]
  async fn the_user_screencast_is_replaced_and_stopped() {
    let source = FakeSource::default();
    let hub = ScreencastHub::default();
    let mut first = hub.start_user(&source, params(70, 800, 600)).await.unwrap();
    let mut second = hub.start_user(&source, params(70, 800, 600)).await.unwrap();
    assert!(first.recv().await.is_none());
    source.emit(3);
    assert_eq!(next(&mut second).await, 3);
    hub.stop_user(&source).await;
    assert!(second.recv().await.is_none());
    assert_eq!(source.state().stops, 2);
  }
}

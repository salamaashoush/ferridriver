use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rustc_hash::FxHashMap;
use serde_json::json;
use tokio::sync::Notify;

use super::transport::IframeTargetEvent;
use super::{CdpPage, CdpWrap, InjectedScriptManager, LifecycleState};
use crate::error::{FerriError, Result};

pub(super) struct AdoptedPages<T: CdpWrap> {
  pages: Vec<CdpPage<T>>,
}

impl<T: CdpWrap> AdoptedPages<T> {
  pub fn new() -> Self {
    Self { pages: Vec::new() }
  }

  pub fn track(&mut self, page: CdpPage<T>) {
    self.pages.push(page);
  }

  pub fn finish(mut self) -> Vec<crate::backend::AnyPage> {
    self.pages.drain(..).map(T::wrap_page).collect()
  }
}

impl<T: CdpWrap> Drop for AdoptedPages<T> {
  fn drop(&mut self) {
    for page in &self.pages {
      page.dispose_local();
    }
  }
}

#[derive(Default)]
pub(super) struct FetchState {
  pub enabled: AtomicBool,
  pub transition: tokio::sync::Mutex<()>,
  /// The live `Fetch.*` interceptor task, if interception is enabled.
  /// `unroute`/`unroute_all` MUST abort it when they `Fetch.disable` on
  /// the last route: a later `route` re-enables and spawns a fresh
  /// loop, and a surviving old loop would double-process every
  /// `Fetch.requestPaused` — its chain sees a `times`-consumed route as
  /// gone and its `Fetch.continueRequest` races (and can beat) the real
  /// handler's fulfill.
  pub interceptor: std::sync::Mutex<Option<tokio::task::AbortHandle>>,
}

#[derive(Default)]
pub(super) struct Renderers {
  sessions: std::sync::Mutex<FxHashMap<String, Arc<Renderer>>>,
  tracker: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<IframeTargetEvent>>>,
  generation: AtomicU64,
  scripts: tokio::sync::Mutex<Vec<(String, String)>>,
}

impl Renderers {
  fn remove_session_tree(&self, session: &str) -> Vec<Arc<Renderer>> {
    let removed = {
      let mut sessions = self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
      let mut pending = vec![session.to_owned()];
      let mut removed = Vec::new();
      while let Some(session) = pending.pop() {
        let frames: Vec<_> = sessions
          .iter()
          .filter(|(_, renderer)| *renderer.session == session || renderer.parent_session.as_deref() == Some(&session))
          .map(|(frame, _)| frame.clone())
          .collect();
        for frame in frames {
          if let Some(renderer) = sessions.remove(&frame) {
            pending.push(renderer.session.to_string());
            removed.push(renderer);
          }
        }
      }
      if !removed.is_empty() {
        self.generation.fetch_add(1, Ordering::AcqRel);
      }
      removed
    };
    for renderer in &removed {
      renderer.stop();
    }
    removed
  }
}

struct Renderer {
  frame: String,
  main_frame: Arc<tokio::sync::OnceCell<String>>,
  session: Arc<str>,
  parent_session: Option<Arc<str>>,
  active: AtomicBool,
  swapped_in: AtomicBool,
  scripts: std::sync::Mutex<FxHashMap<String, String>>,
  contexts: Arc<std::sync::RwLock<FxHashMap<String, i64>>>,
  contexts_notify: Arc<Notify>,
  injected: Arc<InjectedScriptManager>,
  fetch: Arc<FetchState>,
  tasks: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
  ready: tokio::sync::watch::Sender<Option<std::result::Result<(), String>>>,
}

impl Renderer {
  fn new(frame: String, session: Arc<str>, parent_session: Option<Arc<str>>) -> Self {
    Self {
      main_frame: Arc::new(tokio::sync::OnceCell::new_with(Some(frame.clone()))),
      frame,
      session,
      parent_session,
      active: AtomicBool::new(true),
      swapped_in: AtomicBool::new(false),
      scripts: std::sync::Mutex::default(),
      contexts: Arc::default(),
      contexts_notify: Arc::default(),
      injected: Arc::new(InjectedScriptManager::new()),
      fetch: Arc::default(),
      tasks: Arc::default(),
      ready: tokio::sync::watch::channel(None).0,
    }
  }

  fn stop(&self) {
    self.active.store(false, Ordering::Release);
    self.ready.send_replace(Some(Err("frame renderer detached".into())));
    self.abort_tasks();
  }

  fn abort_tasks(&self) {
    for task in self
      .tasks
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .drain(..)
    {
      task.abort();
    }
  }
}

impl<T: CdpWrap> CdpPage<T> {
  fn current_renderers(&self) -> Vec<Arc<Renderer>> {
    self
      .renderers
      .sessions
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .values()
      .filter(|renderer| renderer.active.load(Ordering::Acquire) && !renderer.swapped_in.load(Ordering::Acquire))
      .cloned()
      .collect()
  }

  pub(super) fn renderer_pages(&self) -> Vec<Self> {
    self
      .current_renderers()
      .iter()
      .filter(|renderer| Some(&*renderer.session) != self.session_id.as_deref())
      .map(|renderer| self.renderer_page(renderer))
      .collect()
  }

  pub(super) async fn evaluate_in_renderer_frames(&self, expression: &str) -> Result<()> {
    let mut renderers: Vec<_> = self
      .current_renderers()
      .iter()
      .filter(|renderer| matches!(&*renderer.ready.borrow(), Some(Ok(()))))
      .map(|renderer| self.renderer_page(renderer))
      .collect();
    renderers.push(self.clone());
    for renderer in &renderers {
      renderer.evaluate(expression).await?;
      let main = renderer.peek_main_frame_id();
      let frames: Vec<_> = renderer
        .frame_contexts
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .keys()
        .filter(|frame| Some(frame.as_str()) != main.as_deref())
        .cloned()
        .collect();
      for frame in frames {
        renderer.evaluate_in_frame(expression, &frame).await?;
      }
    }
    Ok(())
  }

  pub(super) async fn install_init_script(&self, source: &str) -> Result<String> {
    let mut scripts = self.renderers.scripts.lock().await;
    let result = self
      .cmd("Page.addScriptToEvaluateOnNewDocument", json!({"source":source}))
      .await?;
    let identifier = script_identifier(&result)?;
    scripts.push((identifier.clone(), source.to_owned()));
    for renderer in self.current_renderers() {
      self.install_renderer_script(&renderer, &identifier, source).await?;
    }
    Ok(identifier)
  }

  async fn install_renderer_script(&self, renderer: &Renderer, identifier: &str, source: &str) -> Result<()> {
    if renderer
      .scripts
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .contains_key(identifier)
    {
      return Ok(());
    }
    let result = self
      .renderer_command(
        renderer,
        "Page.addScriptToEvaluateOnNewDocument",
        json!({"source":source}),
      )
      .await?;
    renderer
      .scripts
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(identifier.to_owned(), script_identifier(&result)?);
    Ok(())
  }

  pub(super) async fn uninstall_init_script(&self, identifier: &str) -> Result<()> {
    let mut scripts = self.renderers.scripts.lock().await;
    self
      .cmd(
        "Page.removeScriptToEvaluateOnNewDocument",
        json!({"identifier":identifier}),
      )
      .await?;
    scripts.retain(|(id, _)| id != identifier);
    for renderer in self.current_renderers() {
      let remote = renderer
        .scripts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(identifier);
      if let Some(remote) = remote {
        self
          .renderer_command(
            &renderer,
            "Page.removeScriptToEvaluateOnNewDocument",
            json!({"identifier":remote}),
          )
          .await?;
      }
    }
    Ok(())
  }

  async fn start_renderer_document(&self, renderer: &Renderer) -> Result<()> {
    let binding = self.binding_initialized.lock().await;
    if *binding {
      self
        .renderer_command(renderer, "Runtime.addBinding", json!({"name":"__fd_binding__"}))
        .await?;
    }
    let scripts = self.renderers.scripts.lock().await;
    for (identifier, source) in scripts.iter() {
      self.install_renderer_script(renderer, identifier, source).await?;
    }
    // Keep configuration locked until live calls can see the initialized renderer.
    self
      .renderer_command(renderer, "Runtime.runIfWaitingForDebugger", json!({}))
      .await?;
    renderer.ready.send_if_modified(|ready| {
      if ready.is_some() {
        return false;
      }
      *ready = Some(Ok(()));
      true
    });
    Ok(())
  }

  pub(super) fn frame_observer(&self) -> super::transport::FrameStateObserver {
    self.renderer_observer(self.session_id.clone(), None)
  }

  fn renderer_observer(
    &self,
    session: Option<Arc<str>>,
    root: Option<(String, String)>,
  ) -> super::transport::FrameStateObserver {
    let observer = match &root {
      Some((frame, parent)) => super::transport::frame_state_observer_for_renderer(
        self.frame_cache.clone(),
        self.events.clone(),
        frame.clone(),
        parent.clone(),
      ),
      None => super::transport::frame_state_observer(self.frame_cache.clone(), self.events.clone()),
    };
    let renderers = Arc::downgrade(&self.renderers);
    let contexts = self.context_observer();
    Arc::new(move |raw, method| {
      contexts(raw, method);
      let Some(renderers) = renderers.upgrade() else { return };
      if matches!(
        method,
        "Page.frameAttached" | "Page.frameDetached" | "Page.frameNavigated" | "Page.navigatedWithinDocument"
      ) {
        let Ok(event) = serde_json::from_slice::<serde_json::Value>(raw) else {
          return;
        };
        let params = &event["params"];
        let frame = if method == "Page.frameNavigated" {
          params["frame"]["id"].as_str()
        } else {
          params["frameId"].as_str()
        };
        let sessions = renderers
          .sessions
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((root, _)) = &root
          && sessions.get(root).is_none_or(|renderer| {
            Some(&*renderer.session) != session.as_deref()
              || !renderer.active.load(Ordering::Acquire)
              || renderer.swapped_in.load(Ordering::Acquire)
          })
        {
          return;
        }
        if let Some(renderer) = frame.and_then(|frame| sessions.get(frame)) {
          if method == "Page.frameAttached" && Some(&*renderer.session) != session.as_deref() {
            renderer.swapped_in.store(true, Ordering::Release);
          }
          if method == "Page.frameDetached" {
            if params["reason"] == "swap" && !renderer.swapped_in.load(Ordering::Acquire) {
              return;
            }
            if params["reason"] != "swap" {
              renderer.stop();
            }
          }
          if matches!(method, "Page.frameNavigated" | "Page.navigatedWithinDocument")
            && Some(&*renderer.session) != session.as_deref()
            && !renderer.swapped_in.load(Ordering::Acquire)
          {
            return;
          }
        }
      }
      observer(raw, method);
    })
  }

  pub(super) fn page_for_session(&self, session: Option<&str>) -> Result<Box<Self>> {
    if session == self.session_id.as_deref() {
      return Ok(Box::new(self.clone()));
    }
    let sessions = self
      .renderers
      .sessions
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    let renderer = sessions
      .values()
      .find(|renderer| {
        Some(&*renderer.session) == session
          && renderer.active.load(Ordering::Acquire)
          && !renderer.swapped_in.load(Ordering::Acquire)
      })
      .ok_or_else(|| FerriError::target_closed(Some("CDP handle renderer is unavailable on this page".into())))?;
    Ok(Box::new(self.renderer_page(renderer)))
  }

  async fn renderer_command(
    &self,
    renderer: &Renderer,
    method: &str,
    params: serde_json::Value,
  ) -> Result<serde_json::Value> {
    self
      .transport
      .send_command(Some(&renderer.session), method, &params)
      .await
  }

  fn renderer_page(&self, renderer: &Renderer) -> Self {
    let mut page = self.clone();
    page.session_id = Some(renderer.session.clone());
    page.main_frame_id = renderer.main_frame.clone();
    page.frame_contexts = renderer.contexts.clone();
    page.frame_contexts_notify = renderer.contexts_notify.clone();
    page.injected_script = renderer.injected.clone();
    page.fetch = renderer.fetch.clone();
    page.listener_tasks = renderer.tasks.clone();
    page
  }

  pub(super) async fn page_for_frame(&self, frame: &str) -> Result<Option<Box<Self>>> {
    let renderer = {
      let sessions = self
        .renderers
        .sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      let cache = self
        .frame_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      let mut current = Some(frame.to_owned());
      let mut found = None;
      while let Some(frame) = current {
        if let Some(renderer) = sessions.get(&frame)
          && !renderer.swapped_in.load(Ordering::Acquire)
        {
          found = Some(renderer.clone());
          break;
        }
        current = cache.parent_id(&frame).map(|parent| parent.to_string());
      }
      found
    };
    let Some(renderer) = renderer else { return Ok(None) };
    if self.session_id.as_deref() == Some(&renderer.session) {
      return Ok(None);
    }
    let mut ready = renderer.ready.subscribe();
    let initialized = ready
      .wait_for(Option::is_some)
      .await
      .map_err(|_| FerriError::target_closed(None))?
      .clone();
    if let Some(Err(error)) = initialized {
      return Err(FerriError::backend(error));
    }
    Ok(Some(Box::new(self.renderer_page(&renderer))))
  }

  pub(super) fn spawn_renderer_tracker(&self) -> Option<tokio::task::AbortHandle> {
    let mut tracker = self
      .renderers
      .tracker
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    if tracker.is_some() {
      return None;
    }
    let subscription = self.transport.tap_iframe_targets();
    *tracker = Some(subscription.checkpoints);
    let mut events = subscription.events;
    let page = self.clone();
    Some(
      tokio::spawn(async move {
        while let Some(event) = events.recv().await {
          match event {
            IframeTargetEvent::Target(event) => page.accept_renderer_target(&event),
            IframeTargetEvent::Checkpoint(complete) => {
              let _ = complete.send(());
            },
            IframeTargetEvent::Closed => break,
          }
        }
        page.dispose_local();
      })
      .abort_handle(),
    )
  }

  async fn renderer_checkpoint(&self) -> Result<()> {
    let tracker = self
      .renderers
      .tracker
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .clone()
      .ok_or_else(|| FerriError::protocol("frame renderer", "tracker has not started"))?;
    let (complete, completed) = tokio::sync::oneshot::channel();
    tracker
      .send(IframeTargetEvent::Checkpoint(complete))
      .map_err(|_| FerriError::target_closed(None))?;
    completed.await.map_err(|_| FerriError::target_closed(None))
  }

  pub(super) async fn initialize_existing_renderers(&self) -> Result<()> {
    if let Some(task) = self.spawn_renderer_tracker() {
      page_task(&self.listener_tasks, task);
    }
    loop {
      self.renderer_checkpoint().await?;
      let (generation, renderers) = {
        let sessions = self
          .renderers
          .sessions
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
          self.renderers.generation.load(Ordering::Acquire),
          sessions.values().cloned().collect::<Vec<_>>(),
        )
      };
      for renderer in renderers {
        let mut ready = renderer.ready.subscribe();
        let result = ready
          .wait_for(Option::is_some)
          .await
          .map_err(|_| FerriError::target_closed(None))?
          .clone();
        if let Some(Err(error)) = result {
          let still_owned = self
            .renderers
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&renderer.frame)
            .is_some_and(|current| Arc::ptr_eq(current, &renderer));
          if still_owned {
            return Err(FerriError::backend(error));
          }
        }
      }
      self.renderer_checkpoint().await?;
      if self.renderers.generation.load(Ordering::Acquire) == generation {
        return Ok(());
      }
    }
  }

  fn accept_renderer_target(&self, event: &serde_json::Value) {
    let parent_session = event["sessionId"].as_str();
    let belongs = parent_session == self.session_id.as_deref() || {
      let sessions = self
        .renderers
        .sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      sessions
        .values()
        .any(|renderer| Some(&*renderer.session) == parent_session)
    };
    if !belongs {
      return;
    }
    let params = &event["params"];
    let Some(session) = params["sessionId"].as_str() else {
      return;
    };
    if event["method"] == "Target.detachedFromTarget" {
      self.remove_renderer(session);
      return;
    }
    if params["targetInfo"]["type"] != "iframe" {
      return;
    }
    let Some(frame) = params["targetInfo"]["targetId"].as_str() else {
      return;
    };
    let renderer = Arc::new(Renderer::new(
      frame.to_owned(),
      Arc::from(session),
      parent_session.map(Arc::from),
    ));
    let previous = {
      let mut sessions = self
        .renderers
        .sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      if sessions
        .get(frame)
        .is_some_and(|current| current.session == renderer.session)
      {
        return;
      }
      self.renderers.generation.fetch_add(1, Ordering::AcqRel);
      sessions.insert(frame.to_owned(), renderer.clone())
    };
    if let Some(old) = previous {
      self.remove_renderer(&old.session);
      old.stop();
      self.transport.unregister_session(&old.session);
    }
    let page = self.clone();
    let parent = params["targetInfo"]["parentFrameId"].as_str().map(str::to_owned);
    let tasks = renderer.tasks.clone();
    let task = tokio::spawn(async move {
      let result = match page.initialize_renderer(&renderer, parent).await {
        Ok(()) => Ok(()),
        Err(error) => match page
          .renderer_command(&renderer, "Runtime.runIfWaitingForDebugger", json!({}))
          .await
        {
          Ok(_) => Err(error),
          Err(resume) => Err(FerriError::backend(format!("{error}; resuming frame failed: {resume}"))),
        },
      };
      if let Err(error) = &result {
        tracing::warn!(frame = %renderer.frame, %error, "Initializing frame renderer failed");
        renderer.active.store(false, Ordering::Release);
        renderer.abort_tasks();
        page.transport.unregister_session(&renderer.session);
      }
      renderer.ready.send_if_modified(|ready| {
        if ready.is_some() {
          return false;
        }
        *ready = Some(result.map_err(|error| error.to_string()));
        true
      });
    });
    page_task(&tasks, task.abort_handle());
  }

  async fn initialize_renderer(&self, renderer: &Arc<Renderer>, parent: Option<String>) -> Result<()> {
    let parent = parent.or_else(|| {
      self
        .frame_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .parent_id(&renderer.frame)
        .map(|id| id.to_string())
    });
    let parent = parent.ok_or_else(|| FerriError::protocol("Target.attachedToTarget", "iframe has no parent frame"))?;
    self
      .frame_cache
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .attach(crate::backend::FrameInfo {
        frame_id: renderer.frame.clone(),
        parent_frame_id: Some(parent.clone()),
        name: String::new(),
        url: String::new(),
      });
    let page = Box::new(self.renderer_page(renderer));
    let observer = page.renderer_observer(
      Some(renderer.session.clone()),
      Some((renderer.frame.clone(), parent.clone())),
    );
    self.transport.register_lifecycle_tracker(
      &renderer.session,
      Arc::new(std::sync::Mutex::new(LifecycleState::new())),
      Arc::new(Notify::new()),
      observer,
    );
    let params = json!({});
    let (enable_page, enable_runtime, auto_attach) = tokio::join!(
      page.cmd("Page.enable", params.clone()),
      page.cmd("Runtime.enable", params),
      page.cmd(
        "Target.setAutoAttach",
        json!({"autoAttach":true,"waitForDebuggerOnStart":true,"flatten":true})
      ),
    );
    enable_page?;
    enable_runtime?;
    auto_attach?;
    let tree = page.cmd("Page.getFrameTree", json!({})).await?;
    let mut frames = Vec::new();
    super::collect_frames(&tree["frameTree"], &mut frames);
    for mut frame in frames {
      if frame.frame_id == renderer.frame {
        frame.parent_frame_id = Some(parent.clone());
      }
      self
        .frame_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .attach(frame);
    }
    page.ensure_engine_injected().await?;
    if !page.routes.read().await.is_empty() || page.http_credentials.read().await.is_some() {
      page.ensure_fetch_enabled_current().await?;
    }
    self.start_renderer_document(renderer).await
  }

  fn remove_renderer(&self, session: &str) {
    for renderer in self.renderers.remove_session_tree(session) {
      self.transport.unregister_session(&renderer.session);
    }
  }

  pub(super) fn dispose_renderers(&self) {
    self.renderers.generation.fetch_add(1, Ordering::AcqRel);
    let renderers: Vec<_> = self
      .renderers
      .sessions
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .drain()
      .map(|(_, renderer)| renderer)
      .collect();
    for renderer in renderers {
      renderer.stop();
      self.transport.unregister_session(&renderer.session);
    }
  }
}

fn script_identifier(result: &serde_json::Value) -> Result<String> {
  result["identifier"]
    .as_str()
    .map(str::to_owned)
    .ok_or_else(|| FerriError::protocol("Page.addScriptToEvaluateOnNewDocument", "missing identifier"))
}

fn page_task(tasks: &std::sync::Mutex<Vec<tokio::task::AbortHandle>>, task: tokio::task::AbortHandle) {
  tasks
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .push(task);
}

#[cfg(test)]
mod tests {
  use super::*;

  fn renderer(frame: &str, session: &str, parent: &str) -> Arc<Renderer> {
    Arc::new(Renderer::new(frame.into(), Arc::from(session), Some(Arc::from(parent))))
  }

  #[test]
  fn parent_detachment_stops_descendants_before_their_detach_events() {
    let registry = Renderers::default();
    let parent = renderer("parent-frame", "parent", "root");
    let child = renderer("child-frame", "child", "parent");
    let nested = renderer("nested-frame", "nested", "child");
    let sibling = renderer("sibling-frame", "sibling", "root");
    for item in [&parent, &child, &nested, &sibling] {
      registry
        .sessions
        .lock()
        .unwrap()
        .insert(item.frame.clone(), item.clone());
    }
    assert_eq!(registry.remove_session_tree("parent").len(), 3);
    assert_eq!(registry.sessions.lock().unwrap().len(), 1);
    for item in [&parent, &child, &nested] {
      assert!(!item.active.load(Ordering::Acquire));
      assert!(matches!(&*item.ready.borrow(), Some(Err(_))));
    }
    assert!(sibling.active.load(Ordering::Acquire));
    assert!(registry.remove_session_tree("child").is_empty());
  }

  #[test]
  fn replacing_a_parent_keeps_its_new_session_and_retires_old_children() {
    let registry = Renderers::default();
    let replacement = renderer("parent-frame", "replacement", "root");
    let child = renderer("child-frame", "child", "old-parent");
    registry
      .sessions
      .lock()
      .unwrap()
      .insert(replacement.frame.clone(), replacement.clone());
    registry
      .sessions
      .lock()
      .unwrap()
      .insert(child.frame.clone(), child.clone());
    assert_eq!(registry.remove_session_tree("old-parent").len(), 1);
    assert!(replacement.active.load(Ordering::Acquire));
    assert!(!child.active.load(Ordering::Acquire));
    assert_eq!(registry.sessions.lock().unwrap().len(), 1);
  }
}

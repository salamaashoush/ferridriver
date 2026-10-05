use serde_json::{Value, json};

use super::page::WebDriverPage;
use super::session::Command;
use crate::backend::{AnyElement, FrameInfo, NavLifecycle};
use crate::error::{FerriError, Result};

impl WebDriverPage {
  pub fn attach_listeners(
    &self,
    console_log: std::sync::Arc<tokio::sync::RwLock<Vec<crate::console_message::ConsoleMessage>>>,
    network_log: std::sync::Arc<tokio::sync::RwLock<Vec<crate::network::Request>>>,
    dialog_log: std::sync::Arc<tokio::sync::RwLock<Vec<crate::state::DialogEvent>>>,
  ) {
    let mut events = self.events.subscribe();
    tokio::spawn(async move {
      while let Some(event) = events.recv().await {
        match event {
          crate::events::PageEvent::Console(message) => {
            crate::state::push_capped(&mut *console_log.write().await, message, crate::state::CONSOLE_LOG_CAP);
          },
          crate::events::PageEvent::Request(request) => {
            crate::state::push_capped(&mut *network_log.write().await, request, crate::state::NETWORK_LOG_CAP);
          },
          crate::events::PageEvent::Dialog(dialog) => crate::state::push_capped(
            &mut *dialog_log.write().await,
            crate::state::DialogEvent {
              dialog_type: dialog.dialog_type().as_str().into(),
              message: dialog.message().into(),
              action: "opened".into(),
            },
            crate::state::DIALOG_LOG_CAP,
          ),
          crate::events::PageEvent::Close => break,
          _ => {},
        }
      }
    });
  }

  pub(crate) fn frame_id(&self) -> String {
    json!([self.target.window, self.target.frames]).to_string()
  }

  pub(crate) fn in_frame(&self, frame_id: Option<&str>) -> Result<Self> {
    let mut page = self.clone();
    if let Some(frame_id) = frame_id {
      let (window, frames): (Option<String>, Vec<Value>) =
        serde_json::from_str(frame_id).map_err(|error| FerriError::invalid_argument("frame", error.to_string()))?;
      if window != self.target.window {
        return Err(FerriError::invalid_argument("frame", "frame belongs to another window"));
      }
      page.target.frames = frames;
    }
    Ok(page)
  }

  pub(crate) fn page_guid(&self) -> String {
    self.target.window.clone().unwrap_or_default()
  }

  pub async fn evaluate_in_frame(&self, expression: &str, frame_id: &str) -> Result<Option<Value>> {
    let page = self.in_frame(Some(frame_id))?;
    if expression.contains("window.__fd") {
      page.ensure_engine_injected().await?;
    }
    page.evaluate(expression).await
  }

  pub async fn bring_to_front(&self) -> Result<()> {
    self.ensure_open()?;
    if super::capabilities::uses_xcuitest(&self.capabilities) {
      return self
        .session
        .activate_native_page(self.target.clone(), self.timeout_ms)
        .await;
    }
    self.command(Command::get(&["window"])).await?;
    Ok(())
  }

  pub async fn get_frame_tree(&self) -> Result<Vec<FrameInfo>> {
    let mut pending = vec![(self.in_frame(None)?, None)];
    let mut frames = Vec::new();
    while let Some((page, parent_frame_id)) = pending.pop() {
      let metadata = page
        .execute_script(
          "return {name:window.name,url:location.href,children:Array.from(document.querySelectorAll('iframe,frame'))};",
          vec![],
        )
        .await?;
      let frame_id = page.frame_id();
      frames.push(FrameInfo {
        frame_id: frame_id.clone(),
        parent_frame_id,
        name: metadata["name"].as_str().unwrap_or_default().to_owned(),
        url: metadata["url"]
          .as_str()
          .ok_or_else(|| FerriError::protocol("frame tree", "missing frame URL"))?
          .to_owned(),
      });
      let children = metadata["children"]
        .as_array()
        .ok_or_else(|| FerriError::protocol("frame tree", "missing child frames"))?;
      for element in children.iter().rev() {
        let mut child = page.clone();
        child.target.frames.push(element.clone());
        pending.push((child, Some(frame_id.clone())));
      }
    }
    Ok(frames)
  }

  pub async fn content_frame_id(&self, object_id: &str) -> Result<Option<String>> {
    let element = json!({"element-6066-11e4-a52e-4f735466cecf":object_id});
    if self
      .execute_script(
        "return /^(IFRAME|FRAME)$/.test(arguments[0].tagName);",
        vec![element.clone()],
      )
      .await?
      != json!(true)
    {
      return Ok(None);
    }
    let mut child = self.clone();
    child.target.frames.push(element);
    Ok(Some(child.frame_id()))
  }

  pub async fn mark_snapshot_iframe(
    &self,
    child_frame_id: &str,
    parent_frame_id: &str,
    streamer_global: &str,
  ) -> Result<()> {
    let child = self.in_frame(Some(child_frame_id))?;
    let element = child
      .target
      .frames
      .last()
      .ok_or_else(|| FerriError::invalid_argument("frame", "main frame has no owner element"))?;
    self
      .in_frame(Some(parent_frame_id))?
      .execute_script(
        "const streamer=window[arguments[0]]; if(streamer) streamer.markIframe(arguments[1],arguments[2]);",
        vec![json!(streamer_global), element.clone(), json!(child_frame_id)],
      )
      .await?;
    Ok(())
  }

  pub async fn resolve_backend_node(&self, backend_node_id: i64, _ref_id: &str) -> Result<AnyElement> {
    let selector = format!("[data-fdref='{backend_node_id}']");
    self.find_element(&selector).await.map(AnyElement::WebDriver)
  }

  pub async fn goto(
    &self,
    url: &str,
    lifecycle: NavLifecycle,
    timeout_ms: u64,
    referer: Option<&str>,
  ) -> Result<Option<crate::network::Response>> {
    if referer.is_some() {
      return Err(FerriError::unsupported(
        "Classic WebDriver cannot override navigation request headers",
      ));
    }
    let navigation = async {
      self.navigate(url, timeout_ms).await?;
      self.navigation_completed(lifecycle, timeout_ms).await
    };
    if timeout_ms == 0 {
      navigation.await?;
    } else {
      tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), navigation)
        .await
        .map_err(|_| FerriError::timeout("navigating WebDriver page", timeout_ms))??;
    }
    // Classic navigation returns no HTTP response metadata. Do not invent a status or headers.
    Ok(None)
  }

  async fn navigation_completed(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<()> {
    let event = match lifecycle {
      NavLifecycle::Commit => None,
      NavLifecycle::DomContentLoaded => Some("DOMContentLoaded"),
      NavLifecycle::Load => Some("load"),
    };
    let metadata = self.navigation_metadata(event, timeout_ms).await?;
    let frame = FrameInfo {
      frame_id: self.frame_id(),
      parent_frame_id: None,
      name: metadata["name"].as_str().unwrap_or_default().to_owned(),
      url: metadata["url"]
        .as_str()
        .ok_or_else(|| FerriError::protocol("navigation", "missing URL"))?
        .to_owned(),
    };
    // The state observer runs in the dispatcher. Navigation must not return
    // before it has updated synchronous page and frame accessors.
    let observed = self.events.wait_for(
      |event| {
        matches!(event, crate::events::PageEvent::FrameNavigated(info)
        if info.frame_id == frame.frame_id && info.url == frame.url)
      },
      timeout_ms,
    );
    self
      .events
      .emit(crate::events::PageEvent::FrameNavigated(frame.clone()));
    observed.await?;
    if metadata["state"] == "interactive" || metadata["state"] == "complete" {
      self.events.emit(crate::events::PageEvent::DomContentLoaded);
    }
    if metadata["state"] == "complete" {
      self.events.emit(crate::events::PageEvent::Load);
    }
    Ok(())
  }

  async fn navigation_metadata(&self, event: Option<&str>, timeout_ms: u64) -> Result<Value> {
    let read = async {
      loop {
        let result = self.execute_script("const event=arguments[0]; const result=()=>({name:window.name,url:location.href,state:document.readyState}); if(!event || document.readyState==='complete' || (event==='DOMContentLoaded' && document.readyState==='interactive')) return result(); return new Promise(resolve=>window.addEventListener(event,()=>resolve(result()),{once:true}));", vec![json!(event)]).await;
        // Appium can return this Inspector error inside HTTP 200 when history
        // navigation replaces the callback's realm. Only this read is replayed.
        if matches!(&result, Err(FerriError::Protocol { method, message })
          if method == "Runtime.awaitPromise" && message == "Missing injected script for given promiseObjectId")
        {
          continue;
        }
        return result;
      }
    };
    if timeout_ms == 0 {
      read.await
    } else {
      tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), read)
        .await
        .map_err(|_| FerriError::timeout("waiting for navigation document", timeout_ms))?
    }
  }

  pub async fn wait_for_navigation(&self) -> Result<()> {
    self
      .events
      .wait_for(|event| matches!(event, crate::events::PageEvent::Load), self.timeout_ms)
      .await?;
    Ok(())
  }

  async fn history_navigation(
    &self,
    command: &str,
    lifecycle: NavLifecycle,
    timeout_ms: u64,
  ) -> Result<Option<crate::network::Response>> {
    self
      .session
      .execute_sequence(
        self.target.clone(),
        vec![
          Command::post(
            &["timeouts"],
            json!({"pageLoad":if timeout_ms == 0 { Value::Null } else { json!(timeout_ms) }}),
          ),
          Command::post(&[command], json!({})),
        ],
        timeout_ms,
      )
      .await?;
    self.navigation_completed(lifecycle, timeout_ms).await?;
    Ok(None)
  }

  pub async fn reload(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<crate::network::Response>> {
    self.history_navigation("refresh", lifecycle, timeout_ms).await
  }

  pub async fn go_back(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<crate::network::Response>> {
    self.history_navigation("back", lifecycle, timeout_ms).await
  }

  pub async fn go_forward(&self, lifecycle: NavLifecycle, timeout_ms: u64) -> Result<Option<crate::network::Response>> {
    self.history_navigation("forward", lifecycle, timeout_ms).await
  }

  pub async fn get_cookies(&self) -> Result<Vec<crate::backend::CookieData>> {
    let cookies = self.command(Command::get(&["cookie"])).await?;
    let cookies = cookies
      .as_array()
      .ok_or_else(|| FerriError::protocol("get cookies", "expected an array"))?;
    cookies
      .iter()
      .map(|cookie| {
        let mut cookie = cookie.clone();
        let object = cookie
          .as_object_mut()
          .ok_or_else(|| FerriError::protocol("get cookies", "expected a cookie object"))?;
        let expiry = object.remove("expiry").unwrap_or(json!(-1));
        object.insert("expires".into(), expiry);
        serde_json::from_value(cookie).map_err(|error| FerriError::protocol("get cookies", error.to_string()))
      })
      .collect()
  }

  pub async fn set_cookie(&self, cookie: crate::backend::CookieData) -> Result<()> {
    let mut value = serde_json::to_value(cookie)?;
    let object = value
      .as_object_mut()
      .ok_or_else(|| FerriError::protocol("add cookie", "expected a cookie object"))?;
    if let Some(expiry) = object.remove("expires")
      && expiry.as_f64().is_some_and(|expiry| expiry >= 0.0)
    {
      object.insert("expiry".into(), expiry);
    }
    self
      .command(Command::post(&["cookie"], json!({"cookie":value})))
      .await?;
    Ok(())
  }

  pub async fn delete_cookie(&self, name: &str, domain: Option<&str>) -> Result<()> {
    if let Some(domain) = domain {
      let cookies = self.get_cookies().await?;
      if !cookies
        .iter()
        .any(|cookie| cookie.name == name && cookie.domain == domain)
      {
        return Ok(());
      }
      if cookies
        .iter()
        .any(|cookie| cookie.name == name && cookie.domain != domain)
      {
        return Err(FerriError::unsupported(
          "Classic WebDriver cannot delete only one domain's cookie when visible cookies share a name",
        ));
      }
    }
    self
      .command(Command {
        method: reqwest::Method::DELETE,
        path: vec!["cookie".into(), name.into()],
        body: None,
      })
      .await?;
    Ok(())
  }

  pub async fn close_page(&self, opts: crate::options::PageCloseOptions) -> Result<()> {
    if opts.run_before_unload == Some(true) {
      return Err(FerriError::unsupported(
        "Classic WebDriver cannot request beforeunload handling when closing a window",
      ));
    }
    self.close().await
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Arc;
  use tokio::io::AsyncWriteExt;

  fn missing_realm() -> Value {
    json!({"error":{"code":-32000,"message":"Missing injected script for given promiseObjectId"},"id":70})
  }

  async fn callback_page(values: Vec<Value>) -> (WebDriverPage, tokio::task::JoinHandle<usize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = reqwest::Url::parse(&format!("http://{}/session", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
      let mut calls = 0;
      loop {
        let (mut socket, _) = listener.accept().await.unwrap();
        let Some((request, _)) = super::super::session::tests::optional_request(&mut socket).await else {
          continue;
        };
        let value = match request.as_str() {
          "POST /session/callback/timeouts HTTP/1.1" | "DELETE /session/callback HTTP/1.1" => Value::Null,
          "POST /session/callback/execute/async HTTP/1.1" => {
            let value = values[calls.min(values.len() - 1)].clone();
            calls += 1;
            value
          },
          _ => panic!("unexpected request {request}"),
        };
        let body = json!({"value":value}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        if request == "DELETE /session/callback HTTP/1.1" {
          return calls;
        }
      }
    });
    let session =
      Arc::new(super::super::session::WebDriverSession::new(reqwest::Client::new(), endpoint, "callback").unwrap());
    let mut page = WebDriverPage::new(session, super::super::session::Target::default(), 1000);
    page.capabilities = Arc::new(json!({"platformName":"iOS","automationName":"XCUITest"}));
    (page, server)
  }

  #[tokio::test]
  async fn navigation_metadata_retries_a_replaced_callback_realm() {
    let metadata = json!({"name":"","url":"https://example.com/restored","state":"complete"});
    let (page, server) = callback_page(vec![missing_realm(), json!({"ok":true,"value":metadata})]).await;
    assert_eq!(page.navigation_metadata(Some("load"), 1000).await.unwrap(), metadata);
    page.session.close().await.unwrap();
    assert_eq!(server.await.unwrap(), 2);
  }

  #[tokio::test]
  async fn user_evaluation_does_not_replay_after_a_replaced_callback_realm() {
    let (page, server) = callback_page(vec![missing_realm()]).await;
    let error = page.execute_script("window.counter++;", vec![]).await.unwrap_err();
    assert!(
      matches!(error, FerriError::Protocol { message, .. } if message == "Missing injected script for given promiseObjectId")
    );
    page.session.close().await.unwrap();
    assert_eq!(server.await.unwrap(), 1);
  }

  #[tokio::test]
  async fn navigation_metadata_preserves_other_script_errors() {
    let (page, server) = callback_page(vec![json!({"ok":false,"error":"Error: metadata failed"})]).await;
    assert!(
      matches!(page.navigation_metadata(Some("load"), 1000).await, Err(FerriError::Evaluation(message)) if message == "Error: metadata failed")
    );
    page.session.close().await.unwrap();
    assert_eq!(server.await.unwrap(), 1);
  }

  #[tokio::test]
  async fn navigation_metadata_retries_share_one_deadline() {
    let (page, server) = callback_page(vec![missing_realm()]).await;
    assert!(matches!(
      page.navigation_metadata(Some("load"), 25).await,
      Err(FerriError::Timeout { timeout_ms: 25, .. })
    ));
    page.session.close().await.unwrap();
    server.await.unwrap();
  }
}

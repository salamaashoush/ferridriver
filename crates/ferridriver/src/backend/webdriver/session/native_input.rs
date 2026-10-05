use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use super::{Command, Target, WebDriverSession};
use crate::error::{FerriError, Result};

static MARKERS: AtomicU64 = AtomicU64::new(0);

enum NativeOperation {
  Activate,
  Pointer(f64, f64),
  Input(Command),
  Navigate(String),
}

impl WebDriverSession {
  pub(crate) async fn native_navigate(self: &Arc<Self>, target: Target, url: &str, timeout_ms: u64) -> Result<()> {
    self
      .native_operation(target, NativeOperation::Navigate(url.to_owned()), timeout_ms)
      .await?;
    Ok(())
  }

  pub(crate) async fn native_pointer_position(
    self: &Arc<Self>,
    target: Target,
    x: f64,
    y: f64,
    timeout_ms: u64,
  ) -> Result<Value> {
    self
      .native_operation(target, NativeOperation::Pointer(x, y), timeout_ms)
      .await
  }

  pub(crate) async fn activate_native_page(self: &Arc<Self>, target: Target, timeout_ms: u64) -> Result<()> {
    self
      .native_operation(target, NativeOperation::Activate, timeout_ms)
      .await?;
    Ok(())
  }

  pub(crate) fn native_input_command(
    self: &Arc<Self>,
    target: Target,
    command: Command,
    timeout_ms: u64,
  ) -> impl std::future::Future<Output = Result<Value>> {
    self.native_operation(target, NativeOperation::Input(command), timeout_ms)
  }

  async fn native_operation(
    self: &Arc<Self>,
    target: Target,
    operation: NativeOperation,
    timeout_ms: u64,
  ) -> Result<Value> {
    let session = Arc::clone(self);
    let activity = match &operation {
      NativeOperation::Activate => "activating native browser page",
      NativeOperation::Pointer(..) => "resolving native pointer position",
      NativeOperation::Input(_) => "performing native input",
      NativeOperation::Navigate(_) => "navigating native browser page",
    };
    let budget = crate::operation_budget::OperationBudget::current(timeout_ms)?;
    let timeout_ms = budget.timeout_ms();
    let (caller_alive, caller_cancelled) = tokio::sync::oneshot::channel::<()>();
    // Keep selection and marker restoration alive when the caller drops its future.
    let task = tokio::spawn(async move {
      let mut shutdown = session.shutdown.subscribe();
      let _selection = session.lock_selection(caller_cancelled, budget).await?;
      let marker = format!(
        "__fd_native_{}_{}",
        std::process::id(),
        MARKERS.fetch_add(1, Ordering::Relaxed)
      );
      let operation = async {
        session.select(&target, timeout_ms).await?;
        session.activate_safari_tab(&target, &marker, timeout_ms).await?;
        match operation {
          NativeOperation::Activate => Ok(Value::Null),
          NativeOperation::Pointer(x, y) => session.resolve_native_pointer(&marker, x, y, timeout_ms).await,
          NativeOperation::Input(command) => {
            session
              .request(Command::post(&["context"], json!({"name":"NATIVE_APP"})), timeout_ms)
              .await?;
            trace_native_actions(&target, &command);
            session.request(command, timeout_ms).await
          },
          NativeOperation::Navigate(url) => session.navigate_safari_tab(&target, &marker, &url, timeout_ms).await,
        }
      };
      let result = tokio::select! {
        _ = shutdown.changed() => Err(FerriError::target_closed(None)),
        result = budget.scope(budget.wait(operation)) => match result {
          Ok(result) => result,
          Err(error) => {
            session.uncertain.store(true, Ordering::Release);
            Err(error.error(activity))
          },
        },
      };
      let cleanup_budget = crate::operation_budget::OperationBudget::wall_clock(5000)?;
      let cleanup = cleanup_budget.scope(cleanup_budget.wait(async {
        session.select(&target, 5000).await?;
        session.request(Command::post(&["execute", "sync"], json!({
          "script":"const key=arguments[0],state=window[key];if(state){if(state.element){if(state.had)state.element.setAttribute('aria-label',state.original);else state.element.removeAttribute('aria-label');}if('title' in state&&document.title===key)document.title=state.title;if(state.navigationRestore)window.removeEventListener('pageshow',state.navigationRestore);delete window[key];}",
          "args":[marker]
        })), 5000).await
      })).await;
      let detail = match cleanup {
        Ok(Ok(_)) => return result,
        Ok(Err(error)) => error.to_string(),
        Err(_) => "cleanup timed out".into(),
      };
      session.uncertain.store(true, Ordering::Release);
      tracing::warn!(reason = %detail, "Native browser operation cleanup failed");
      Err(FerriError::backend(format!(
        "Native browser operation {result:?}; restoring browser context and document markers failed: {detail}"
      )))
    });
    let result = task
      .await
      .map_err(|error| FerriError::backend(format!("Native browser operation task failed: {error}")));
    drop(caller_alive);
    result?
  }

  async fn navigate_safari_tab(&self, target: &Target, marker: &str, url: &str, timeout_ms: u64) -> Result<Value> {
    self
      .request(
        Command::post(
          &["execute", "sync"],
          json!({
            "script":"const key=arguments[0],state=window[key]=Object.assign(window[key]||{},{url:location.href});state.navigationRestore=event=>{if(event.persisted){window.removeEventListener('pageshow',state.navigationRestore);delete window[key];}};window.addEventListener('pageshow',state.navigationRestore);",
            "args":[marker]
          }),
        ),
        timeout_ms,
      )
      .await?;
    self
      .request(Command::post(&["context"], json!({"name":"NATIVE_APP"})), timeout_ms)
      .await?;
    let address = self
      .request(
        Command::post(
          &["element"],
          json!({
            "using":"accessibility id","value":"TabBarItemTitle"
          }),
        ),
        timeout_ms,
      )
      .await?;
    self
      .request(
        Command::post(&["element", element_id(&address)?, "click"], json!({})),
        timeout_ms,
      )
      .await?;
    let field = self.request(Command::get(&["element", "active"]), timeout_ms).await?;
    let field = element_id(&field)?;
    self
      .request(Command::post(&["element", field, "clear"], json!({})), timeout_ms)
      .await?;
    let value = format!("{url}\n");
    self
      .request(
        Command::post(&["element", field, "value"], json!({"text":value,"value":[value]})),
        timeout_ms,
      )
      .await?;
    self.select(target, timeout_ms).await?;
    loop {
      let committed = self.request(Command::post(&["execute", "sync"], json!({
        "script":"const expected=new URL(arguments[1],location.href).href;if(location.href!==expected)return false;const state=window[arguments[0]];if(!state)return true;return state.url!==expected&&state.url.split('#')[0]===expected.split('#')[0];",
        "args":[marker,url]
      })), timeout_ms).await?;
      if committed == json!(true) {
        return Ok(Value::Null);
      }
      if committed != json!(false) {
        return Err(FerriError::protocol(
          "native navigation",
          "missing document commit state",
        ));
      }
      tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
  }

  async fn activate_safari_tab(&self, target: &Target, marker: &str, timeout_ms: u64) -> Result<()> {
    let visible = self.request(Command::post(&["execute", "sync"], json!({
      "script":"if(document.visibilityState==='visible')return true;const key=arguments[0];window[key]={title:document.title};document.title=key;return false;",
      "args":[marker]
    })), timeout_ms).await?;
    if visible == json!(true) {
      return Ok(());
    }
    if visible != json!(false) {
      return Err(FerriError::protocol(
        "Safari tab activation",
        "missing document visibility",
      ));
    }
    self
      .request(Command::post(&["context"], json!({"name":"NATIVE_APP"})), timeout_ms)
      .await?;
    let address = self
      .request(
        Command::post(
          &["element"],
          json!({
            "using":"accessibility id","value":"TabBarItemTitle"
          }),
        ),
        timeout_ms,
      )
      .await?;
    let id = element_id(&address)?;
    let rect = self.request(Command::get(&["element", id, "rect"]), timeout_ms).await?;
    let x = number(&rect, "x")? + number(&rect, "width")? / 2.0;
    let y = number(&rect, "y")? + number(&rect, "height")? / 2.0;
    // Safari's address-field swipe opens its tab overview without localized menu labels.
    self
      .request(
        Command::post(
          &["execute", "sync"],
          json!({
            "script":"mobile: dragFromToForDuration",
            "args":[{"duration":0.15,"fromX":x,"fromY":y,"toX":x,"toY":(y-250.0).max(0.0)}]
          }),
        ),
        timeout_ms,
      )
      .await?;
    let tab = self.request(Command::post(&["element"], json!({
      "using":"-ios predicate string",
      "value":format!("type == 'XCUIElementTypeButton' AND name BEGINSWITH 'TabOverviewItemView?' AND label == '{marker}'")
    })), timeout_ms).await?;
    self
      .request(
        Command::post(&["element", element_id(&tab)?, "click"], json!({})),
        timeout_ms,
      )
      .await?;
    self.select(target, timeout_ms).await?;
    let visible = self
      .request(
        Command::post(
          &["execute", "sync"],
          json!({
            "script":"return document.visibilityState === 'visible';","args":[]
          }),
        ),
        timeout_ms,
      )
      .await?;
    if visible != json!(true) {
      return Err(FerriError::unsupported(
        "Safari's native tab selection did not activate the requested document",
      ));
    }
    Ok(())
  }

  async fn resolve_native_pointer(&self, marker: &str, x: f64, y: f64, timeout_ms: u64) -> Result<Value> {
    // Safari exposes the temporary document title as the web view's accessibility name.
    let element_marker = format!("{marker}_point");
    let web = self.request(Command::post(&["execute", "sync"], json!({
      "script":"const [key,x,y,label]=arguments;const element=document.elementFromPoint(x,y);if(!element)return null;const r=element.getBoundingClientRect();if(r.width<=0||r.height<=0)return null;window[key]=Object.assign(window[key]||{},{element,had:element.hasAttribute('aria-label'),original:element.getAttribute('aria-label')});element.setAttribute('aria-label',label);return {x:x-r.left,y:y-r.top,width:r.width,height:r.height,tag:element.tagName};",
      "args":[marker,x,y,element_marker]
    })), timeout_ms).await?;
    if web.is_null() {
      return Err(FerriError::invalid_argument(
        "position",
        "point is outside the web viewport",
      ));
    }
    self
      .request(Command::post(&["context"], json!({"name":"NATIVE_APP"})), timeout_ms)
      .await?;
    let element = self
      .request(
        Command::post(&["element"], json!({"using":"accessibility id","value":element_marker})),
        timeout_ms,
      )
      .await?;
    let id = element["element-6066-11e4-a52e-4f735466cecf"]
      .as_str()
      .ok_or_else(|| FerriError::protocol("native element lookup", "missing element reference"))?;
    let native = self.request(Command::get(&["element", id, "rect"]), timeout_ms).await?;
    let mut point = json!({});
    for (axis, size) in [("x", "width"), ("y", "height")] {
      let extent = number(&web, size)?;
      let native_extent = number(&native, size)?;
      if extent <= 0.0 || native_extent <= 0.0 {
        return Err(FerriError::protocol("native element rectangle", "element has no area"));
      }
      point[axis] = json!(number(&native, axis)? + number(&web, axis)? * native_extent / extent);
    }
    tracing::debug!(target: "ferridriver::webdriver::native_input", x, y, %web, %native, %point, "native pointer geometry");
    Ok(point)
  }
}

fn trace_native_actions(target: &Target, command: &Command) {
  if !tracing::enabled!(target: "ferridriver::webdriver::native_input", tracing::Level::DEBUG) {
    return;
  }
  let points: Vec<_> = command
    .body
    .as_ref()
    .and_then(|body| body["actions"].as_array())
    .into_iter()
    .flatten()
    .filter(|source| source["type"] == "pointer")
    .flat_map(|source| source["actions"].as_array().into_iter().flatten())
    .filter(|step| step["type"] == "pointerMove")
    .map(|step| json!({"x":step["x"],"y":step["y"],"origin":step["origin"]}))
    .collect();
  if !points.is_empty() {
    tracing::debug!(target: "ferridriver::webdriver::native_input", window = ?target.window, ?points, "native pointer dispatch");
  }
}

fn element_id(value: &Value) -> Result<&str> {
  value["element-6066-11e4-a52e-4f735466cecf"]
    .as_str()
    .ok_or_else(|| FerriError::protocol("native element lookup", "missing element reference"))
}

fn number(value: &Value, key: &str) -> Result<f64> {
  value[key]
    .as_f64()
    .filter(|number| number.is_finite())
    .ok_or_else(|| FerriError::protocol("native element rectangle", format!("missing or invalid {key}")))
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::io::AsyncWriteExt;

  async fn navigation_case(reject: bool, cancel: bool, never_commit: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = reqwest::Url::parse(&format!("http://{}/session", listener.local_addr().unwrap())).unwrap();
    let session = Arc::new(WebDriverSession::new(reqwest::Client::new(), endpoint, "navigation").unwrap());
    let (started, started_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let (restored, restored_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
      let mut requests = Vec::new();
      let mut started = Some(started);
      let mut release = Some(release_rx);
      let mut restored = Some(restored);
      let mut reads = 0;
      loop {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = super::super::tests::request(&mut socket).await;
        let script = request.1["script"].as_str().unwrap_or_default();
        let input = request.0.starts_with("POST /session/navigation/element/field/value ");
        if input {
          started.take().unwrap().send(()).unwrap();
          release.take().unwrap().await.unwrap();
        }
        let value = if input && reject {
          json!({"error":"unknown error","message":"URL entry rejected"})
        } else if script.contains("document.visibilityState") {
          json!(true)
        } else if request.1["value"] == "TabBarItemTitle" {
          json!({"element-6066-11e4-a52e-4f735466cecf":"address"})
        } else if request.0.starts_with("GET /session/navigation/element/active ") {
          json!({"element-6066-11e4-a52e-4f735466cecf":"field"})
        } else if script.starts_with("const expected=") {
          reads += 1;
          json!(!never_commit && reads > 1)
        } else {
          Value::Null
        };
        let cleanup = script.contains("if(state.element)");
        let done = request.0.starts_with("DELETE ");
        let body = json!({"value":value}).to_string();
        let status = if input && reject {
          "500 Internal Server Error"
        } else {
          "200 OK"
        };
        socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        requests.push(request);
        if cleanup {
          restored.take().unwrap().send(()).unwrap();
        }
        if done {
          return (requests, reads);
        }
      }
    });
    let worker = session.clone();
    let caller = tokio::spawn(async move {
      worker
        .native_navigate(
          Target {
            appium_context: Some("WEBVIEW_device".into()),
            window: Some("device".into()),
            ..Default::default()
          },
          "data:text/html,sashoush",
          if never_commit { 100 } else { 1000 },
        )
        .await
    });
    started_rx.await.unwrap();
    if cancel {
      caller.abort();
    }
    release.send(()).unwrap();
    if cancel {
      assert!(caller.await.unwrap_err().is_cancelled());
    } else if reject || never_commit {
      let error = caller.await.unwrap().unwrap_err();
      assert!(
        error.to_string().contains(if reject {
          "URL entry rejected"
        } else {
          "navigating native browser page"
        }),
        "{error}"
      );
    } else {
      caller.await.unwrap().unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), restored_rx)
      .await
      .unwrap()
      .unwrap();
    session.close().await.unwrap();
    let (requests, reads) = server.await.unwrap();
    assert_navigation_requests(&requests, reads, reject, never_commit);
  }

  fn assert_navigation_requests(requests: &[(String, Value)], reads: usize, reject: bool, never_commit: bool) {
    let input = requests
      .iter()
      .find(|request| request.0.starts_with("POST /session/navigation/element/field/value "))
      .unwrap();
    assert_eq!(
      input.1,
      json!({"text":"data:text/html,sashoush\n","value":["data:text/html,sashoush\n"]})
    );
    let contexts: Vec<_> = requests
      .iter()
      .filter(|request| request.0.starts_with("POST /session/navigation/context "))
      .map(|request| request.1["name"].as_str().unwrap())
      .collect();
    assert_eq!(
      contexts,
      if reject {
        vec!["WEBVIEW_device", "NATIVE_APP", "WEBVIEW_device"]
      } else {
        vec!["WEBVIEW_device", "NATIVE_APP", "WEBVIEW_device", "WEBVIEW_device"]
      }
    );
    if !reject && !never_commit {
      assert_eq!(reads, 2);
    }
  }

  #[tokio::test]
  async fn native_navigation_waits_for_commit_and_restores_target() {
    navigation_case(false, false, false).await;
  }

  #[tokio::test]
  async fn rejected_native_navigation_restores_target() {
    navigation_case(true, false, false).await;
  }

  #[tokio::test]
  async fn cancelled_native_navigation_restores_target() {
    navigation_case(false, true, false).await;
  }

  #[tokio::test]
  async fn native_navigation_deadline_bounds_commit_wait() {
    navigation_case(false, false, true).await;
  }

  #[tokio::test]
  async fn cancelled_queued_input_never_reaches_the_device() {
    queued_input_case(true).await;
  }

  #[tokio::test]
  async fn cancelled_queued_webdriver_input_never_reaches_the_device() {
    queued_input_case(false).await;
  }

  async fn queued_input_case(native: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = reqwest::Url::parse(&format!("http://{}/session", listener.local_addr().unwrap())).unwrap();
    let session = Arc::new(WebDriverSession::new(reqwest::Client::new(), endpoint, "native").unwrap());
    let server = tokio::spawn(async move {
      let mut requests = Vec::new();
      loop {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = super::super::tests::request(&mut socket).await;
        let done = request.0.starts_with("DELETE ");
        requests.push(request);
        let body = json!({"value":true}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        if done {
          return requests;
        }
      }
    });
    let lock = session.closed.lock().await;
    let mut input = Box::pin(async {
      let command = Command::post(
        &["actions"],
        json!({"actions":[{
          "type":"key","id":"keyboard","actions":[{"type":"keyDown","value":"a"},{"type":"keyUp","value":"a"}]
        }]}),
      );
      if native {
        session.native_input_command(Target::default(), command, 1000).await
      } else {
        session.execute(Target::default(), command, 1000).await
      }
    });
    std::future::poll_fn(|cx| {
      assert!(std::future::Future::poll(input.as_mut(), cx).is_pending());
      std::task::Poll::Ready(())
    })
    .await;
    tokio::task::yield_now().await;
    drop(input);
    drop(lock);
    session
      .execute(Target::default(), Command::get(&["title"]), 1000)
      .await
      .unwrap();
    session.close().await.unwrap();
    let requests = tokio::time::timeout(std::time::Duration::from_secs(2), server)
      .await
      .unwrap()
      .unwrap();
    assert_eq!(
      requests.iter().map(|request| request.0.as_str()).collect::<Vec<_>>(),
      ["GET /session/native/title HTTP/1.1", "DELETE /session/native HTTP/1.1",]
    );
  }

  async fn input_case(fail: bool, cancel: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = reqwest::Url::parse(&format!("http://{}/session", listener.local_addr().unwrap())).unwrap();
    let session = Arc::new(WebDriverSession::new(reqwest::Client::new(), endpoint, "native").unwrap());
    let (started, started_rx) = tokio::sync::oneshot::channel();
    let (release, release_rx) = tokio::sync::oneshot::channel();
    let (restored, restored_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
      let mut requests = Vec::new();
      let mut started = Some(started);
      let mut release = Some(release_rx);
      let mut restored = Some(restored);
      for index in 0..11 {
        let (mut socket, _) = listener.accept().await.unwrap();
        requests.push(super::super::tests::request(&mut socket).await);
        if index == 5 {
          started.take().unwrap().send(()).unwrap();
          release.take().unwrap().await.unwrap();
        }
        let value = if index == 3 {
          json!(true)
        } else if index == 5 && fail {
          json!({"error":"unknown error","message":"native input rejected"})
        } else {
          Value::Null
        };
        let status = if index == 5 && fail {
          "500 Internal Server Error"
        } else {
          "200 OK"
        };
        let body = json!({"value":value}).to_string();
        socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        if index == 9 {
          restored.take().unwrap().send(()).unwrap();
        }
      }
      requests
    });
    let worker = session.clone();
    let caller = tokio::spawn(async move {
      worker
        .native_input_command(
          Target {
            appium_context: Some("WEBVIEW_anchor".into()),
            window: Some("page-opaque".into()),
            ..Default::default()
          },
          Command::post(
            &["actions"],
            json!({"actions":[{"type":"key","id":"keyboard","actions":[{"type":"keyDown","value":"a"}]}]}),
          ),
          1000,
        )
        .await
    });
    started_rx.await.unwrap();
    if cancel {
      caller.abort();
    }
    release.send(()).unwrap();
    if cancel {
      assert!(caller.await.unwrap_err().is_cancelled());
    } else if fail {
      assert!(
        caller
          .await
          .unwrap()
          .unwrap_err()
          .to_string()
          .contains("native input rejected")
      );
    } else {
      caller.await.unwrap().unwrap();
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), restored_rx)
      .await
      .unwrap()
      .unwrap();
    session.close().await.unwrap();
    let requests = server.await.unwrap();
    assert_eq!(requests[0].1, json!({"name":"WEBVIEW_anchor"}));
    assert_eq!(requests[1].1, json!({"handle":"page-opaque"}));
    assert_eq!(requests[4].1, json!({"name":"NATIVE_APP"}));
    assert_eq!(requests[5].0, "POST /session/native/actions HTTP/1.1");
    assert_eq!(requests[5].1["actions"][0]["actions"][0]["value"], "a");
    assert_eq!(requests[6].1, json!({"name":"WEBVIEW_anchor"}));
    assert_eq!(requests[7].1, json!({"handle":"page-opaque"}));
    assert_eq!(requests[10].0, "DELETE /session/native HTTP/1.1");
  }

  #[tokio::test]
  async fn native_input_restores_the_browser_target() {
    input_case(false, false).await;
  }

  #[tokio::test]
  async fn rejected_native_input_restores_the_browser_target() {
    input_case(true, false).await;
  }

  #[tokio::test]
  async fn cancelled_native_input_restores_the_browser_target() {
    input_case(false, true).await;
  }

  async fn lookup_case(fail: bool, cancel: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = reqwest::Url::parse(&format!("http://{}/session", listener.local_addr().unwrap())).unwrap();
    let session = Arc::new(WebDriverSession::new(reqwest::Client::new(), endpoint, "native").unwrap());
    let release = Arc::new(tokio::sync::Notify::new());
    let wait_release = release.clone();
    let (lookup_started, started) = tokio::sync::oneshot::channel();
    let (restored, wait_restored) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
      let mut requests = Vec::new();
      let mut lookup_started = Some(lookup_started);
      let mut restored = Some(restored);
      for (index, value) in [
        Value::Null,
        json!(true),
        json!({"x":20,"y":20,"width":180,"height":45}),
        Value::Null,
        json!({"element-6066-11e4-a52e-4f735466cecf":"native-button"}),
        if fail {
          json!({"error":"unknown error","message":"native rectangle failed"})
        } else {
          json!({"x":46,"y":248,"width":263,"height":67})
        },
        Value::Null,
        Value::Null,
        Value::Null,
      ]
      .into_iter()
      .enumerate()
      {
        let (mut socket, _) = listener.accept().await.unwrap();
        requests.push(super::super::tests::request(&mut socket).await);
        if index == 5 {
          lookup_started.take().unwrap().send(()).unwrap();
          wait_release.notified().await;
        }
        let body = json!({"value":value}).to_string();
        let status = if index == 5 && fail {
          "500 Internal Server Error"
        } else {
          "200 OK"
        };
        socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        if index == 7 {
          restored.take().unwrap().send(()).unwrap();
        }
      }
      requests
    });
    let worker = session.clone();
    let caller = tokio::spawn(async move {
      worker
        .native_pointer_position(
          Target {
            appium_context: Some("WEBVIEW_device".into()),
            ..Default::default()
          },
          52.0,
          177.59375,
          1000,
        )
        .await
    });
    started.await.unwrap();
    if cancel {
      caller.abort();
    }
    release.notify_one();
    if cancel {
      assert!(caller.await.unwrap_err().is_cancelled());
    } else {
      let result = caller.await.unwrap();
      if fail {
        assert!(result.unwrap_err().to_string().contains("native rectangle failed"));
      } else {
        let point = result.unwrap();
        assert!((point["x"].as_f64().unwrap() - (46.0 + 20.0 * 263.0 / 180.0)).abs() < f64::EPSILON);
        assert!((point["y"].as_f64().unwrap() - (248.0 + 20.0 * 67.0 / 45.0)).abs() < f64::EPSILON);
      }
    }
    tokio::time::timeout(std::time::Duration::from_secs(1), wait_restored)
      .await
      .unwrap()
      .unwrap();
    session.close().await.unwrap();
    let requests = server.await.unwrap();
    assert_eq!(requests[0].1, json!({"name":"WEBVIEW_device"}));
    assert_eq!(requests[3].1, json!({"name":"NATIVE_APP"}));
    assert_eq!(requests[4].1["value"], requests[2].1["args"][3]);
    assert_ne!(requests[4].1["value"], requests[2].1["args"][0]);
    assert_eq!(requests[6].1, json!({"name":"WEBVIEW_device"}));
    assert_eq!(requests[7].1["args"][0], requests[2].1["args"][0]);
    assert_eq!(requests[8].0, "DELETE /session/native HTTP/1.1");
  }

  #[tokio::test]
  async fn native_pointer_preserves_position_and_restores_browser_context() {
    lookup_case(false, false).await;
  }

  #[tokio::test]
  async fn failed_native_lookup_restores_browser_context() {
    lookup_case(true, false).await;
  }

  #[tokio::test]
  async fn cancelled_caller_does_not_leave_an_accessibility_label_or_native_context() {
    lookup_case(false, true).await;
  }

  async fn activation_case(fail_lookup: bool, visible: bool, cancel: bool) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = reqwest::Url::parse(&format!("http://{}/session", listener.local_addr().unwrap())).unwrap();
    let session = Arc::new(WebDriverSession::new(reqwest::Client::new(), endpoint, "activation").unwrap());
    let (started, wait_started) = tokio::sync::oneshot::channel();
    let (release, wait_release) = tokio::sync::oneshot::channel();
    let (restored, wait_restored) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
      let mut values = vec![
        Value::Null,
        json!(false),
        Value::Null,
        json!({"element-6066-11e4-a52e-4f735466cecf":"address"}),
        json!({"x":154,"y":599,"width":67,"height":21}),
        Value::Null,
      ];
      if fail_lookup {
        values.push(json!({"error":"no such element","message":"tab not found"}));
      } else {
        values.extend([
          json!({"element-6066-11e4-a52e-4f735466cecf":"tab"}),
          Value::Null,
          Value::Null,
          json!(visible),
        ]);
      }
      values.extend([Value::Null, Value::Null, Value::Null]);
      let mut started = Some(started);
      let mut wait_release = Some(wait_release);
      let mut restored = Some(restored);
      let cleanup_index = values.len() - 2;
      let mut requests = Vec::new();
      for (index, value) in values.into_iter().enumerate() {
        let (mut socket, _) = listener.accept().await.unwrap();
        requests.push(super::super::tests::request(&mut socket).await);
        if index == 5 {
          started.take().unwrap().send(()).unwrap();
          wait_release.take().unwrap().await.unwrap();
        }
        let body = json!({"value":value}).to_string();
        let status = if fail_lookup && index == 6 {
          "404 Not Found"
        } else {
          "200 OK"
        };
        socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        if index == cleanup_index {
          restored.take().unwrap().send(()).unwrap();
        }
      }
      requests
    });
    let worker = session.clone();
    let caller = tokio::spawn(async move {
      worker
        .activate_native_page(
          Target {
            appium_context: Some("WEBVIEW_device".into()),
            ..Default::default()
          },
          1000,
        )
        .await
    });
    wait_started.await.unwrap();
    if cancel {
      caller.abort();
    }
    release.send(()).unwrap();
    if cancel {
      assert!(caller.await.unwrap_err().is_cancelled());
    } else {
      let result = caller.await.unwrap();
      if fail_lookup {
        assert!(result.unwrap_err().to_string().contains("tab not found"));
      } else if !visible {
        assert!(matches!(result, Err(FerriError::Unsupported { .. })));
      } else {
        result.unwrap();
      }
    }
    tokio::time::timeout(std::time::Duration::from_secs(1), wait_restored)
      .await
      .unwrap()
      .unwrap();
    session.close().await.unwrap();
    let requests = server.await.unwrap();
    assert_activation_requests(&requests);
  }

  fn assert_activation_requests(requests: &[(String, Value)]) {
    let marker = requests[1].1["args"][0].as_str().unwrap();
    assert_eq!(requests[0].1, json!({"name":"WEBVIEW_device"}));
    assert_eq!(requests[2].1, json!({"name":"NATIVE_APP"}));
    assert_eq!(
      requests[5].1["args"][0],
      json!({"duration":0.15,"fromX":187.5,"fromY":609.5,"toX":187.5,"toY":359.5})
    );
    assert!(
      requests[6].1["value"]
        .as_str()
        .unwrap()
        .ends_with(&format!("label == '{marker}'"))
    );
    assert_eq!(requests[requests.len() - 3].1, json!({"name":"WEBVIEW_device"}));
    assert_eq!(requests[requests.len() - 2].1["args"][0], marker);
    assert_eq!(requests.last().unwrap().0, "DELETE /session/activation HTTP/1.1");
  }

  #[tokio::test]
  async fn native_activation_selects_exact_tab_and_restores_context() {
    activation_case(false, true, false).await;
  }

  #[tokio::test]
  async fn failed_tab_lookup_restores_document_marker() {
    activation_case(true, false, false).await;
  }

  #[tokio::test]
  async fn native_activation_rejects_a_document_that_remains_hidden() {
    activation_case(false, false, false).await;
  }

  #[tokio::test]
  async fn cancelled_activation_restores_document_marker() {
    activation_case(false, true, true).await;
  }
}

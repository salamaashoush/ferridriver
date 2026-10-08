use std::sync::Arc;

use reqwest::{Method, Url};
use serde_json::{Value, json};

use crate::error::{FerriError, Result};
use crate::operation_budget::OperationBudget;

mod native_input;

tokio::task_local! {
  static COMMAND_ADMISSION: OperationBudget;
}

pub(crate) async fn with_command_admission<F: std::future::Future>(
  budget: Option<OperationBudget>,
  future: F,
) -> F::Output {
  match budget {
    Some(budget) => COMMAND_ADMISSION.scope(budget, future).await,
    None => future.await,
  }
}

pub struct WebDriverSession {
  client: reqwest::Client,
  url: Url,
  secrets: crate::response::Secrets,
  closed: tokio::sync::Mutex<bool>,
  shutdown: tokio::sync::watch::Sender<bool>,
  uncertain: std::sync::atomic::AtomicBool,
  script_timeout_scoped: std::sync::atomic::AtomicBool,
}

pub struct CreatedSession {
  pub session: Arc<WebDriverSession>,
  pub id: String,
  pub capabilities: Value,
}

#[derive(Clone, Default)]
pub struct Target {
  pub appium_context: Option<String>,
  pub window: Option<String>,
  pub frames: Vec<Value>,
}

pub struct Command {
  pub method: Method,
  pub path: Vec<String>,
  pub body: Option<Value>,
}

impl Command {
  fn script_timeout(&self) -> Option<&Value> {
    if self.method != Method::POST || self.path != ["timeouts"] {
      return None;
    }
    self.body.as_ref()?.get("script")
  }

  #[must_use]
  pub fn get(path: &[&str]) -> Self {
    Self {
      method: Method::GET,
      path: path.iter().map(|s| (*s).to_owned()).collect(),
      body: None,
    }
  }

  #[must_use]
  pub fn post(path: &[&str], body: Value) -> Self {
    Self {
      method: Method::POST,
      path: path.iter().map(|s| (*s).to_owned()).collect(),
      body: Some(body),
    }
  }
}

impl WebDriverSession {
  pub(crate) fn protocol_error(&self, method: &str, message: &str) -> FerriError {
    FerriError::protocol(method, self.secrets.redact(message).into_owned())
  }

  pub(crate) fn is_closed(&self) -> bool {
    *self.shutdown.borrow() || self.uncertain.load(std::sync::atomic::Ordering::Acquire)
  }

  pub async fn create(
    client: reqwest::Client,
    url: Url,
    capabilities: Value,
    timeout_ms: u64,
    headers: Option<&rustc_hash::FxHashMap<String, String>>,
  ) -> Result<CreatedSession> {
    let secrets = connection_secrets(&url, &capabilities, headers);
    let mut request = client
      .post(url.clone())
      .json(&json!({"capabilities": {"alwaysMatch": capabilities}}));
    if timeout_ms != 0 {
      request = request.timeout(std::time::Duration::from_millis(timeout_ms));
    }
    let response = request
      .send()
      .await
      .map_err(|error| request_error(error, "creating WebDriver session", timeout_ms))?;
    let status = response.status();
    let mut payload = response
      .json::<Value>()
      .await
      .map_err(|error| request_error(error, "reading WebDriver session response", timeout_ms))?;
    if !status.is_success()
      && let Some(message) = payload.pointer_mut("/value/message")
    {
      secrets.redact_json(message);
    }
    let value = decode_response(status, payload, "WebDriver /session", timeout_ms)?;
    let id = value
      .get("sessionId")
      .and_then(Value::as_str)
      .filter(|id| !id.is_empty())
      .ok_or_else(|| FerriError::protocol("WebDriver /session", "response omitted sessionId"))?
      .to_owned();
    let mut session = Self::new(client, url, &id)?;
    session.secrets = secrets;
    let session = Arc::new(session);
    crate::state::allocation::retain_session(&session);
    let Some(capabilities) = value.get("capabilities").filter(|value| value.is_object()).cloned() else {
      let error = FerriError::protocol("WebDriver /session", "response omitted capabilities");
      if let Err(cleanup) = session.close().await {
        return Err(FerriError::backend(format!("{error}; cleanup failed: {cleanup}")));
      }
      return Err(error);
    };
    Ok(CreatedSession {
      session,
      id,
      capabilities,
    })
  }

  pub fn new(client: reqwest::Client, mut url: Url, session_id: &str) -> Result<Self> {
    url
      .path_segments_mut()
      .map_err(|()| FerriError::invalid_argument("endpoint", "WebDriver endpoint cannot hold path segments"))?
      .push(session_id);
    Ok(Self {
      client,
      url,
      secrets: crate::response::Secrets::default(),
      closed: tokio::sync::Mutex::new(false),
      shutdown: tokio::sync::watch::channel(false).0,
      uncertain: std::sync::atomic::AtomicBool::new(false),
      script_timeout_scoped: std::sync::atomic::AtomicBool::new(false),
    })
  }

  pub fn execute(
    self: &Arc<Self>,
    target: Target,
    command: Command,
    timeout_ms: u64,
  ) -> impl std::future::Future<Output = Result<Value>> {
    self.execute_sequence(target, vec![command], timeout_ms)
  }

  pub async fn execute_sequence(
    self: &Arc<Self>,
    target: Target,
    mut commands: Vec<Command>,
    timeout_ms: u64,
  ) -> Result<Value> {
    if commands.is_empty() {
      return Err(FerriError::invalid_argument(
        "commands",
        "expected at least one command",
      ));
    }
    let admission = COMMAND_ADMISSION.try_with(|budget| *budget).ok();
    let session = Arc::clone(self);
    let explicit_budget = OperationBudget::capture().is_some();
    let budget = OperationBudget::current(timeout_ms)?;
    let timeout_ms = budget.timeout_ms();
    let (caller_alive, caller_cancelled) = tokio::sync::oneshot::channel::<()>();
    let (reply, replied) = tokio::sync::oneshot::channel::<Result<Value>>();
    // The remote command can continue after its caller is cancelled. Keep the
    // selection lock until its response so another target cannot overtake it.
    tokio::spawn(budget.scope(async move {
      let mut reply = Some(reply);
      let mut shutdown = session.shutdown.subscribe();
      if *shutdown.borrow() {
        if let Some(reply) = reply.take() {
          let _ = reply.send(Err(FerriError::target_closed(Some(
            "WebDriver session is closing".into(),
          ))));
        }
        return;
      }
      let execute = async {
        let _selection = session.lock_selection(caller_cancelled, budget).await?;
        if !explicit_budget
          && session.script_timeout_scoped.load(std::sync::atomic::Ordering::Relaxed)
          && commands
            .iter()
            .any(|command| command.path == ["execute", "sync"] || command.path == ["execute", "async"])
          && !commands.iter().any(|command| command.script_timeout().is_some())
        {
          commands.insert(
            0,
            Command::post(
              &["timeouts"],
              json!({"script": if timeout_ms == 0 { Value::Null } else { json!(timeout_ms) }}),
            ),
          );
        }
        let operation = async {
          session.select(&target, timeout_ms).await?;
          let mut result = Value::Null;
          for mut command in commands {
            if let Some(admission) = admission {
              admission
                .remaining_ms()
                .map_err(|error| error.error("dispatching WebDriver command"))?;
            }
            let sets_script_timeout = command.script_timeout().is_some();
            if explicit_budget
              && sets_script_timeout
              && let Some(script) = command.body.as_mut().and_then(|body| body.get_mut("script"))
            {
              *script = json!(
                budget
                  .remaining_ms()
                  .map_err(|error| error.error("setting WebDriver script timeout"))?
              );
            }
            result = session.request(command, timeout_ms).await?;
            if sets_script_timeout {
              session
                .script_timeout_scoped
                .store(explicit_budget, std::sync::atomic::Ordering::Relaxed);
            }
          }
          Ok(result)
        };
        let mut operation = std::pin::pin!(operation);
        match budget.wait(operation.as_mut()).await {
          Ok(result) => result,
          Err(error) => {
            // The caller's time is up, but a command is already on the wire.
            // Dropping it left the session's command state unknown, and the
            // whole session unusable after one timed-out call. Answer the
            // caller now and let that command finish under the selection;
            // `request` dispatches nothing further once the budget is spent.
            if let Some(reply) = reply.take() {
              let _ = reply.send(Err(error.error("executing WebDriver command")));
            }
            operation.await
          },
        }
      };
      let result = tokio::select! {
        biased;
        _ = shutdown.changed() => Err(FerriError::target_closed(Some("WebDriver session is closing".into()))),
        result = execute => result,
      };
      if let Some(reply) = reply.take() {
        let _ = reply.send(result);
      }
    }));
    let result = replied
      .await
      .map_err(|_| FerriError::backend("WebDriver command task ended without a reply"));
    drop(caller_alive);
    result?
  }

  async fn lock_selection(
    &self,
    caller_cancelled: tokio::sync::oneshot::Receiver<()>,
    budget: OperationBudget,
  ) -> Result<tokio::sync::MutexGuard<'_, bool>> {
    let mut shutdown = self.shutdown.subscribe();
    if self.is_closed() {
      return Err(FerriError::target_closed(None));
    }
    let closed = tokio::select! {
      biased;
      _ = caller_cancelled => return Err(FerriError::Interrupted("queued WebDriver command".into())),
      _ = shutdown.changed() => return Err(FerriError::target_closed(None)),
      closed = budget.wait(self.closed.lock()) => closed.map_err(|error| error.error("waiting for WebDriver command"))?,
    };
    if *closed || self.is_closed() {
      return Err(FerriError::target_closed(Some(
        "WebDriver session is closed or its command state is uncertain".into(),
      )));
    }
    Ok(closed)
  }

  async fn select(&self, target: &Target, timeout_ms: u64) -> Result<()> {
    if let Some(context) = &target.appium_context {
      self
        .request(Command::post(&["context"], json!({"name": context})), timeout_ms)
        .await?;
    }
    if let Some(window) = &target.window {
      self
        .request(Command::post(&["window"], json!({"handle": window})), timeout_ms)
        .await?;
      self
        .request(Command::post(&["frame"], json!({"id": null})), timeout_ms)
        .await?;
    }
    for frame in &target.frames {
      self
        .request(Command::post(&["frame"], json!({"id": frame})), timeout_ms)
        .await?;
    }
    Ok(())
  }

  async fn request(&self, command: Command, timeout_ms: u64) -> Result<Value> {
    let mut url = self.url.clone();
    url
      .path_segments_mut()
      .map_err(|()| FerriError::invalid_argument("endpoint", "WebDriver endpoint cannot hold path segments"))?
      .extend(&command.path);
    let method = format!("WebDriver {} /{}", command.method, command.path.join("/"));
    let unlimited_script = command.script_timeout() == Some(&Value::Null)
      && command
        .body
        .as_ref()
        .and_then(Value::as_object)
        .is_some_and(|body| body.len() == 1);
    let mut request = self.client.request(command.method, url);
    if let Some(body) = command.body {
      request = request.json(&body);
    }
    if let Some(budget) = OperationBudget::capture() {
      budget
        .remaining_ms()
        .map_err(|error| error.error(format!("dispatching {method}")))?;
    }
    let response = request.send().await.map_err(|error| {
      self.uncertain.store(true, std::sync::atomic::Ordering::Release);
      request_error(error, &method, timeout_ms)
    })?;
    let status = response.status();
    let mut payload = response.json::<Value>().await.map_err(|error| {
      self.uncertain.store(true, std::sync::atomic::Ordering::Release);
      request_error(error, &method, timeout_ms)
    })?;
    if !status.is_success()
      && let Some(message) = payload.pointer_mut("/value/message")
    {
      self.secrets.redact_json(message);
    }
    match decode_response(status, payload, &method, timeout_ms) {
      Err(error @ (FerriError::InvalidArgument { .. } | FerriError::Unsupported(_))) if unlimited_script => Err(
        FerriError::unsupported(format!("WebDriver provider rejects unlimited script timeouts: {error}")),
      ),
      result => result,
    }
  }

  pub(crate) fn deletion_complete(&self) -> bool {
    self.closed.try_lock().is_ok_and(|closed| *closed)
  }

  pub async fn close(&self) -> Result<()> {
    self.shutdown.send_replace(true);
    let mut closed = self.closed.lock().await;
    if !*closed {
      delete_session(&self.client, self.url.clone(), &self.secrets).await?;
      *closed = true;
    }
    Ok(())
  }
}

impl Drop for WebDriverSession {
  fn drop(&mut self) {
    if *self.closed.get_mut() {
      return;
    }
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
      let client = self.client.clone();
      let url = self.url.clone();
      let secrets = self.secrets.clone();
      runtime.spawn(async move {
        if let Err(error) = delete_session(&client, url, &secrets).await {
          tracing::warn!(%error, "WebDriver session cleanup failed");
        }
      });
    } else {
      tracing::warn!("WebDriver session dropped without a runtime for cleanup");
    }
  }
}

fn connection_secrets(
  url: &Url,
  capabilities: &Value,
  headers: Option<&rustc_hash::FxHashMap<String, String>>,
) -> crate::response::Secrets {
  let mut values = vec![url.username().to_owned(), url.password().unwrap_or_default().to_owned()];
  for value in headers.into_iter().flat_map(|headers| headers.values()) {
    values.push(value.clone());
    if let Some((_, credential)) = value.split_once(' ') {
      values.push(credential.to_owned());
    }
  }
  // Providers can echo vendor capabilities, including credentials, in errors.
  let mut pending = vec![capabilities];
  while let Some(value) = pending.pop() {
    match value {
      Value::String(value) => values.push(value.clone()),
      Value::Array(values) => pending.extend(values),
      Value::Object(values) => pending.extend(values.values()),
      _ => {},
    }
  }
  crate::response::Secrets::new(values.into_iter().map(|value| ("WEBDRIVER".into(), value)))
}

fn request_error(error: reqwest::Error, method: &str, timeout_ms: u64) -> FerriError {
  if error.is_timeout() {
    FerriError::timeout(method, timeout_ms)
  } else {
    FerriError::backend(format!("{method}: {}", error.without_url()))
  }
}

fn decode_response(status: reqwest::StatusCode, mut payload: Value, method: &str, timeout_ms: u64) -> Result<Value> {
  let value = payload
    .get_mut("value")
    .ok_or_else(|| FerriError::protocol(method, "response omitted value"))?
    .take();
  if !status.is_success()
    && let Some(code) = value.get("error").and_then(Value::as_str)
  {
    let message = value
      .get("message")
      .and_then(Value::as_str)
      .filter(|message| !message.is_empty());
    let detail = message.map_or_else(|| code.to_owned(), |message| format!("{code}: {message}"));
    return Err(match code {
      "invalid session id" | "no such window" => FerriError::target_closed(Some(detail)),
      "timeout" | "script timeout" => FerriError::timeout(format!("{method}: {detail}"), timeout_ms),
      "javascript error" => FerriError::evaluation(detail),
      "invalid argument" => FerriError::invalid_argument(method, detail),
      "unknown command" | "unknown method" | "unsupported operation"
        if matches!(status.as_u16(), 404 | 405 | 500 | 501) =>
      {
        FerriError::unsupported(format!("{method}: {detail}"))
      },
      _ => FerriError::protocol(method, format!("server returned {status}, error {detail}")),
    });
  }
  if !status.is_success() {
    return Err(FerriError::protocol(method, format!("server returned {status}")));
  }
  Ok(value)
}

async fn delete_session(client: &reqwest::Client, url: Url, secrets: &crate::response::Secrets) -> Result<()> {
  let response = client
    .delete(url)
    .timeout(std::time::Duration::from_secs(5))
    .send()
    .await
    .map_err(|error| request_error(error, "WebDriver session deletion", 5000))?;
  let status = response.status();
  if status.is_success() {
    return Ok(());
  }
  let mut payload = response
    .json::<Value>()
    .await
    .map_err(|error| request_error(error, "WebDriver session deletion", 5000))?;
  if payload.pointer("/value/error").and_then(Value::as_str) == Some("invalid session id") {
    return Ok(());
  }
  if let Some(message) = payload.pointer_mut("/value/message") {
    secrets.redact_json(message);
  }
  decode_response(status, payload, "WebDriver DELETE /session", 5000).map(|_| ())
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;
  use tokio::io::{AsyncReadExt, AsyncWriteExt};

  #[tokio::test]
  async fn malformed_capabilities_release_the_created_session() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/wd/hub/session", listener.local_addr().unwrap())).unwrap();
    let (delete_started, deletion) = tokio::sync::oneshot::channel();
    let (allow_reply, reply) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
      let mut delete_started = Some(delete_started);
      let mut reply = Some(reply);
      for (expected, body) in [
        (
          "POST /wd/hub/session HTTP/1.1",
          r#"{"value":{"sessionId":"safari-contract","capabilities":null}}"#,
        ),
        ("DELETE /wd/hub/session/safari-contract HTTP/1.1", r#"{"value":null}"#),
      ] {
        let (mut socket, _) = listener.accept().await.unwrap();
        assert_eq!(request(&mut socket).await.0, expected);
        if expected.starts_with("DELETE") {
          delete_started.take().unwrap().send(()).unwrap();
          reply.take().unwrap().await.unwrap();
        }
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
      }
    });
    let create = WebDriverSession::create(reqwest::Client::new(), url, json!({"browserName":"safari"}), 1000, None);
    tokio::pin!(create);
    tokio::select! {
      biased;
      _ = &mut create => panic!("connect returned before session cleanup completed"),
      result = deletion => result.unwrap(),
      () = tokio::time::sleep(std::time::Duration::from_secs(2)) => panic!("session deletion was not requested"),
    }
    allow_reply.send(()).unwrap();
    let result = create.await;
    assert!(matches!(result, Err(FerriError::Protocol { message, .. }) if message.contains("capabilities")));
    tokio::time::timeout(std::time::Duration::from_secs(2), server)
      .await
      .unwrap()
      .unwrap();
  }

  #[test]
  fn script_results_with_error_fields_are_values() {
    let value = json!({"error":"application-level error", "message":"preserve me"});
    assert_eq!(
      decode_response(reqwest::StatusCode::OK, json!({"value":value}), "execute", 1000).unwrap(),
      value
    );
  }

  #[test]
  fn protocol_errors_keep_the_core_error_taxonomy() {
    for (code, name) in [
      ("script timeout", "TimeoutError"),
      ("invalid session id", "TargetClosedError"),
    ] {
      let result = decode_response(
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        json!({"value":{"error":code}}),
        "execute",
        1000,
      );
      assert_eq!(result.unwrap_err().name(), name);
    }
    assert!(matches!(
      decode_response(
        reqwest::StatusCode::NOT_FOUND,
        json!({"value":{"error":"unknown command"}}),
        "execute",
        1000
      ),
      Err(FerriError::Unsupported(_))
    ));
    assert!(decode_response(reqwest::StatusCode::OK, json!({}), "execute", 1000).is_err());
    assert!(matches!(
      decode_response(
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        json!({"value":{"error":"javascript error","message":"script threw"}}),
        "execute",
        1000
      ),
      Err(FerriError::Evaluation(message)) if message.contains("script threw")
    ));
    assert!(matches!(
      decode_response(
        reqwest::StatusCode::BAD_REQUEST,
        json!({"value":{"error":"invalid argument","message":"invalid action"}}),
        "actions",
        1000
      ),
      Err(FerriError::InvalidArgument { reason, .. }) if reason.contains("invalid action")
    ));
  }

  #[test]
  fn appium_unimplemented_commands_preserve_the_operation_and_reason() {
    let error = decode_response(
      reqwest::StatusCode::METHOD_NOT_ALLOWED,
      json!({"value":{"error":"unknown method","message":"Method has not yet been implemented"}}),
      "POST window/new",
      1000,
    )
    .unwrap_err();
    assert!(matches!(error, FerriError::Unsupported(ref reason)
      if reason.contains("POST window/new") && reason.contains("Method has not yet been implemented")));
  }

  #[test]
  fn provider_authentication_and_quota_errors_cannot_trigger_protocol_fallback() {
    for status in [401, 403, 429, 503] {
      let error = decode_response(
        reqwest::StatusCode::from_u16(status).unwrap(),
        json!({"value":{"error":"unsupported operation","message":"provider rejected request"}}),
        "execute",
        1000,
      )
      .unwrap_err();
      assert!(matches!(error, FerriError::Protocol { .. }));
      assert!(error.to_string().contains(&status.to_string()));
    }
  }

  #[test]
  fn provider_errors_preserve_diagnostics_without_connection_credentials() {
    let secrets = connection_secrets(
      &Url::parse("https://sashoush:url-password@example.com/wd/hub").unwrap(),
      &json!({"bst:options":{"accessKey":"capability-token"}}),
      Some(&rustc_hash::FxHashMap::from_iter([(
        "Authorization".into(),
        "Bearer header-token".into(),
      )])),
    );
    let mut message = json!("device offline: url-password capability-token header-token");
    secrets.redact_json(&mut message);
    let error = decode_response(
      reqwest::StatusCode::INTERNAL_SERVER_ERROR,
      json!({"value":{"error":"session not created", "message":message}}),
      "session",
      1000,
    )
    .unwrap_err();
    let text = error.to_string();
    assert!(text.contains("device offline"));
    for credential in ["url-password", "capability-token", "header-token"] {
      assert!(!text.contains(credential));
    }
  }

  #[tokio::test(start_paused = true)]
  async fn operation_budgets_survive_the_selection_worker_and_set_remote_script_timeouts() {
    use crate::backend::webdriver::page::WebDriverPage;
    let keep_awake = tokio::spawn(async {
      loop {
        tokio::task::yield_now().await;
      }
    });
    for timeout in [0, 90_000] {
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let url = Url::parse(&format!("http://{}/session", listener.local_addr().unwrap())).unwrap();
      let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
      let (finish, completed) = tokio::sync::oneshot::channel();
      let server = tokio::spawn(async move {
        let mut completed = Some(completed);
        let mut requests = Vec::new();
        loop {
          let (mut socket, _) = listener.accept().await.unwrap();
          let (line, body) = request(&mut socket).await;
          events.send((line.clone(), body.clone())).unwrap();
          requests.push((line.clone(), body));
          let value = if line.contains("/execute/sync") {
            if let Some(completed) = completed.take() {
              completed.await.unwrap();
            }
            json!(42)
          } else {
            Value::Null
          };
          let payload = json!({"value":value}).to_string();
          socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",payload.len()).as_bytes()).await.unwrap();
          if line.starts_with("DELETE ") {
            break;
          }
        }
        requests
      });
      let session = Arc::new(WebDriverSession::new(reqwest::Client::new(), url, "budget").unwrap());
      let page = WebDriverPage::new(session.clone(), Target::default(), 30_000);
      let mut command = Box::pin(
        OperationBudget::new(timeout)
          .unwrap()
          .scope(page.execute_script("return 42", vec![])),
      );
      assert!(futures::poll!(&mut command).is_pending());
      let (line, body) = received.recv().await.unwrap();
      assert_eq!(line, "POST /session/budget/timeouts HTTP/1.1");
      assert_eq!(body["script"], if timeout == 0 { Value::Null } else { json!(timeout) });
      assert!(received.recv().await.unwrap().0.contains("/execute/sync"));
      tokio::time::advance(std::time::Duration::from_secs(61)).await;
      assert!(futures::poll!(&mut command).is_pending());
      assert!(received.try_recv().is_err(), "a tool must never be replayed");
      finish.send(()).unwrap();
      assert_eq!(command.await.unwrap(), json!(42));
      assert_eq!(page.execute_script("return 42", vec![]).await.unwrap(), json!(42));
      let (line, body) = received.recv().await.unwrap();
      assert_eq!(line, "POST /session/budget/timeouts HTTP/1.1");
      assert_eq!(body["script"], 30_000);
      assert!(received.recv().await.unwrap().0.contains("/execute/sync"));
      session
        .execute(
          Target::default(),
          Command::post(&["timeouts"], json!({"script":7000})),
          30_000,
        )
        .await
        .unwrap();
      assert_eq!(received.recv().await.unwrap().1["script"], 7000);
      assert_eq!(page.execute_script("return 42", vec![]).await.unwrap(), json!(42));
      assert!(received.recv().await.unwrap().0.contains("/execute/sync"));
      session.close().await.unwrap();
      assert_eq!(server.await.unwrap().len(), 7);
    }
    keep_awake.abort();
  }

  #[tokio::test(start_paused = true)]
  async fn expired_admission_drains_setup_without_dispatching_the_script() {
    use crate::backend::webdriver::page::WebDriverPage;
    let keep_awake = tokio::spawn(async {
      loop {
        tokio::task::yield_now().await;
      }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/session", listener.local_addr().unwrap())).unwrap();
    let (started, received) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
      let (mut socket, _) = listener.accept().await.unwrap();
      assert_eq!(
        request(&mut socket).await.0,
        "POST /session/admission/timeouts HTTP/1.1"
      );
      started.send(()).unwrap();
      released.await.unwrap();
      socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"value\":null}")
        .await
        .unwrap();
      for expected in [
        "GET /session/admission/title HTTP/1.1",
        "DELETE /session/admission HTTP/1.1",
      ] {
        let (mut socket, _) = listener.accept().await.unwrap();
        assert_eq!(
          request(&mut socket).await.0,
          expected,
          "expired script must not be dispatched"
        );
        socket
          .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"value\":null}")
          .await
          .unwrap();
      }
    });
    let session = Arc::new(WebDriverSession::new(reqwest::Client::new(), url, "admission").unwrap());
    let page = WebDriverPage::new(session.clone(), Target::default(), 30_000);
    let admission = OperationBudget::new(100).unwrap();
    let transport = OperationBudget::wall_clock(30_000).unwrap();
    let mut command = Box::pin(admission.wait(with_command_admission(
      Some(admission),
      transport.scope(page.execute_script("return 42", vec![])),
    )));
    assert!(futures::poll!(&mut command).is_pending());
    received.await.unwrap();
    tokio::time::advance(std::time::Duration::from_millis(101)).await;
    assert!(command.await.is_err());
    release.send(()).unwrap();
    session
      .execute(Target::default(), Command::get(&["title"]), 1000)
      .await
      .unwrap();
    session.close().await.unwrap();
    server.await.unwrap();
    keep_awake.abort();
  }

  #[tokio::test(start_paused = true)]
  async fn queued_unscoped_scripts_reset_temporary_timeouts_after_acquiring_selection() {
    use crate::backend::webdriver::page::WebDriverPage;
    let keep_awake = tokio::spawn(async {
      loop {
        tokio::task::yield_now().await;
      }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/session", listener.local_addr().unwrap())).unwrap();
    let (setting, observed) = tokio::sync::oneshot::channel();
    let (release, resume) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
      let mut gate = Some((setting, resume));
      let mut timeout = json!(30_000);
      let mut requests = Vec::new();
      loop {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (line, body) = request(&mut socket).await;
        requests.push(line.clone());
        let value = if line.contains("/timeouts") {
          timeout = body["script"].clone();
          if let Some((setting, resume)) = gate.take() {
            setting.send(()).unwrap();
            resume.await.unwrap();
          }
          Value::Null
        } else if line.contains("/execute/sync") {
          timeout.clone()
        } else {
          Value::Null
        };
        let payload = json!({"value":value}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",payload.len()).as_bytes()).await.unwrap();
        if line.starts_with("DELETE ") {
          break;
        }
      }
      requests
    });
    let session = Arc::new(WebDriverSession::new(reqwest::Client::new(), url, "queued").unwrap());
    let page = WebDriverPage::new(session.clone(), Target::default(), 30_000);
    let mut scoped = Box::pin(
      OperationBudget::new(1000)
        .unwrap()
        .scope(page.execute_script("return 1", vec![])),
    );
    assert!(futures::poll!(&mut scoped).is_pending());
    observed.await.unwrap();
    let mut unscoped = Box::pin(page.execute_script("return 2", vec![]));
    assert!(futures::poll!(&mut unscoped).is_pending());
    release.send(()).unwrap();
    assert_eq!(scoped.await.unwrap(), 1000);
    assert_eq!(unscoped.await.unwrap(), 30_000);
    session.close().await.unwrap();
    assert_eq!(server.await.unwrap().len(), 5);
    keep_awake.abort();
  }

  #[tokio::test]
  async fn rejected_unlimited_script_budget_is_unsupported_without_invoking_or_poisoning_the_session() {
    use crate::backend::webdriver::page::WebDriverPage;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/session", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
      let mut requests = Vec::new();
      for (status, value) in [
        (
          "400 Bad Request",
          json!({"error":"invalid argument","message":"Non-number timeout value"}),
        ),
        ("200 OK", json!(42)),
        ("200 OK", Value::Null),
      ] {
        let (mut socket, _) = listener.accept().await.unwrap();
        requests.push(request(&mut socket).await);
        let payload = json!({"value":value}).to_string();
        socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",payload.len()).as_bytes()).await.unwrap();
      }
      requests
    });
    let session = Arc::new(WebDriverSession::new(reqwest::Client::new(), url, "limited").unwrap());
    let page = WebDriverPage::new(session.clone(), Target::default(), 30_000);
    let error = OperationBudget::new(0)
      .unwrap()
      .scope(page.execute_script("must not run", vec![]))
      .await
      .unwrap_err();
    assert!(matches!(error, FerriError::Unsupported(_)), "{error}");
    assert!(!session.is_closed());
    assert_eq!(page.execute_script("return 42", vec![]).await.unwrap(), 42);
    session.close().await.unwrap();
    let requests = server.await.unwrap();
    assert_eq!(requests[0].0, "POST /session/limited/timeouts HTTP/1.1");
    assert_eq!(requests[0].1["script"], Value::Null);
    assert_eq!(requests[1].1["script"], "return 42");
  }

  #[tokio::test]
  async fn operation_budgets_reject_unlimited_or_excessive_xcuitest_atom_waits_before_invocation() {
    use crate::backend::webdriver::page::WebDriverPage;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/session", listener.local_addr().unwrap())).unwrap();
    let session = Arc::new(WebDriverSession::new(reqwest::Client::new(), url, "unstarted").unwrap());
    let mut page = WebDriverPage::new(session.clone(), Target::default(), 30_000);
    page.capabilities = Arc::new(json!({"platformName":"iOS", "appium:automationName":"XCUITest"}));
    for timeout in [0, 121_000] {
      let error = OperationBudget::new(timeout)
        .unwrap()
        .scope(page.execute_script("throw new Error('must not run')", vec![]))
        .await
        .unwrap_err();
      assert!(matches!(error, FerriError::Unsupported(_)), "{error}");
    }
    assert!(futures::poll!(Box::pin(listener.accept())).is_pending());
    *session.closed.lock().await = true;
  }

  pub(crate) async fn request(socket: &mut tokio::net::TcpStream) -> (String, Value) {
    optional_request(socket).await.unwrap()
  }

  pub(crate) async fn optional_request(socket: &mut tokio::net::TcpStream) -> Option<(String, Value)> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    let (head, length) = loop {
      let count = socket.read(&mut buffer).await.unwrap();
      if count == 0 {
        return None;
      }
      bytes.extend_from_slice(&buffer[..count]);
      if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
        let head = String::from_utf8(bytes[..end].to_vec()).unwrap();
        let length = head
          .lines()
          .find_map(|line| {
            line
              .to_ascii_lowercase()
              .strip_prefix("content-length:")
              .map(|v| v.trim().parse::<usize>().unwrap())
          })
          .unwrap_or(0);
        break (end + 4, length);
      }
    };
    while bytes.len() < head + length {
      let count = socket.read(&mut buffer).await.unwrap();
      if count == 0 {
        return None;
      }
      bytes.extend_from_slice(&buffer[..count]);
    }
    let line = String::from_utf8(bytes[..head].to_vec())
      .unwrap()
      .lines()
      .next()
      .unwrap()
      .to_owned();
    let body = if length == 0 {
      Value::Null
    } else {
      serde_json::from_slice(&bytes[head..head + length]).unwrap()
    };
    Some((line, body))
  }

  #[tokio::test]
  async fn a_completed_remote_timeout_does_not_poison_the_next_command() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/session", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
      for (status, body) in [
        ("500 Internal Server Error", json!({"value":{"error":"script timeout"}})),
        ("200 OK", json!({"value":"recovered"})),
        ("200 OK", json!({"value":null})),
      ] {
        let (mut socket, _) = listener.accept().await.unwrap();
        request(&mut socket).await;
        let body = body.to_string();
        socket.write_all(format!(
          "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
          body.len()
        ).as_bytes()).await.unwrap();
      }
    });
    let session = Arc::new(WebDriverSession::new(reqwest::Client::new(), url, "timeout-contract").unwrap());
    let result = session
      .execute(
        Target::default(),
        Command::post(
          &["execute", "sync"],
          json!({
            "script":"return 1", "args":[]
          }),
        ),
        1000,
      )
      .await;
    assert!(matches!(result, Err(FerriError::Timeout { .. })));
    let value = session
      .execute(Target::default(), Command::get(&["title"]), 1000)
      .await
      .unwrap();
    assert_eq!(value, "recovered");
    session.close().await.unwrap();
    server.await.unwrap();
  }

  #[tokio::test]
  async fn close_cancels_an_unbounded_command_before_deleting_the_session() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/session", listener.local_addr().unwrap())).unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
      let (mut blocked, _) = listener.accept().await.unwrap();
      assert!(request(&mut blocked).await.0.contains("/execute/sync"));
      started_tx.send(()).unwrap();
      let (mut cleanup, _) = listener.accept().await.unwrap();
      assert_eq!(request(&mut cleanup).await.0, "DELETE /session/cancel-session HTTP/1.1");
      cleanup
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"value\":null}")
        .await
        .unwrap();
    });
    let session = Arc::new(WebDriverSession::new(reqwest::Client::new(), url, "cancel-session").unwrap());
    let operation = session.clone();
    let task = tokio::spawn(async move {
      operation
        .execute(
          Target::default(),
          Command::post(
            &["execute", "sync"],
            json!({"script":"return new Promise(() => {})", "args":[]}),
          ),
          0,
        )
        .await
    });
    started_rx.await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(1), session.close())
      .await
      .unwrap()
      .unwrap();
    assert_eq!(task.await.unwrap().unwrap_err().name(), "TargetClosedError");
    server.await.unwrap();
  }

  #[tokio::test]
  async fn a_command_that_outlives_its_caller_finishes_and_the_session_stays_usable() {
    async fn answer(socket: &mut tokio::net::TcpStream, value: &str) {
      let payload = format!("{{\"value\":{value}}}");
      socket
        .write_all(
          format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
          )
          .as_bytes(),
        )
        .await
        .unwrap();
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/session", listener.local_addr().unwrap())).unwrap();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
      let (mut first, _) = listener.accept().await.unwrap();
      assert!(request(&mut first).await.0.contains("/execute/sync"));
      release_rx.await.unwrap();
      answer(&mut first, "\"first\"").await;
      let (mut second, _) = listener.accept().await.unwrap();
      assert!(request(&mut second).await.0.contains("/execute/sync"));
      answer(&mut second, "\"second\"").await;
    });
    let session = Arc::new(WebDriverSession::new(reqwest::Client::new(), url, "drain-session").unwrap());
    let command = || Command::post(&["execute", "sync"], json!({"script":"return 1", "args":[]}));
    let timed_out = tokio::time::timeout(
      std::time::Duration::from_secs(1),
      session.execute(Target::default(), command(), 50),
    )
    .await
    .expect("the caller is answered at its deadline, not when the command returns")
    .unwrap_err();
    assert_eq!(timed_out.name(), "TimeoutError", "{timed_out}");
    release_tx.send(()).unwrap();
    assert_eq!(
      session.execute(Target::default(), command(), 5000).await.unwrap(),
      json!("second")
    );
    server.await.unwrap();
  }

  #[tokio::test]
  async fn cancellation_cannot_interleave_target_selection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/wd/hub/session?region=eu", listener.local_addr().unwrap());
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
      let mut release = Some(release_rx);
      for index in 0..7 {
        let (mut socket, _) = listener.accept().await.unwrap();
        let (line, body) = request(&mut socket).await;
        seen_tx.send((line, body)).unwrap();
        if index == 0 {
          release.take().unwrap().await.unwrap();
        }
        let payload = b"{\"value\":null}";
        socket
          .write_all(
            format!(
              "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
              payload.len()
            )
            .as_bytes(),
          )
          .await
          .unwrap();
        socket.write_all(payload).await.unwrap();
      }
    });
    let session =
      Arc::new(WebDriverSession::new(reqwest::Client::new(), Url::parse(&endpoint).unwrap(), "mobile/id").unwrap());
    let first = Arc::clone(&session);
    let task = tokio::spawn(async move {
      first
        .execute(
          Target {
            window: Some("first".into()),
            ..Target::default()
          },
          Command::get(&["title"]),
          3000,
        )
        .await
    });
    let (line, body) = seen_rx.recv().await.unwrap();
    assert_eq!(line, "POST /wd/hub/session/mobile%2Fid/window?region=eu HTTP/1.1");
    assert_eq!(body["handle"], "first");
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let second = Arc::clone(&session);
    let task = tokio::spawn(async move {
      second
        .execute(
          Target {
            window: Some("second".into()),
            ..Target::default()
          },
          Command::get(&["title"]),
          3000,
        )
        .await
    });
    assert!(
      tokio::time::timeout(std::time::Duration::from_millis(25), seen_rx.recv())
        .await
        .is_err()
    );
    release_tx.send(()).unwrap();
    assert!(seen_rx.recv().await.unwrap().0.contains("/frame?"));
    assert!(seen_rx.recv().await.unwrap().0.contains("/title?"));
    assert_eq!(seen_rx.recv().await.unwrap().1["handle"], "second");
    task.await.unwrap().unwrap();
    session.close().await.unwrap();
    server.await.unwrap();
  }
}

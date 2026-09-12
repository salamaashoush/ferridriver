use std::sync::Arc;

use reqwest::{Method, Url};
use serde_json::{Value, json};

use crate::error::{FerriError, Result};

pub struct WebDriverSession {
  client: reqwest::Client,
  url: Url,
  secrets: crate::response::Secrets,
  closed: tokio::sync::Mutex<bool>,
  uncertain: std::sync::atomic::AtomicBool,
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
  pub async fn create(
    client: reqwest::Client,
    url: Url,
    capabilities: Value,
    timeout_ms: u64,
    headers: Option<&rustc_hash::FxHashMap<String, String>>,
  ) -> Result<CreatedSession> {
    let secrets = connection_secrets(&url, &capabilities, headers);
    let response = client
      .post(url.clone())
      .json(&json!({"capabilities": {"alwaysMatch": capabilities}}))
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
    let capabilities = value
      .get("capabilities")
      .filter(|value| value.is_object())
      .ok_or_else(|| FerriError::protocol("WebDriver /session", "response omitted capabilities"))?
      .clone();
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
      uncertain: std::sync::atomic::AtomicBool::new(false),
    })
  }

  pub async fn execute(self: &Arc<Self>, target: Target, command: Command, timeout_ms: u64) -> Result<Value> {
    self.execute_sequence(target, vec![command], timeout_ms).await
  }

  pub async fn execute_sequence(
    self: &Arc<Self>,
    target: Target,
    commands: Vec<Command>,
    timeout_ms: u64,
  ) -> Result<Value> {
    if commands.is_empty() {
      return Err(FerriError::invalid_argument(
        "commands",
        "expected at least one command",
      ));
    }
    let session = Arc::clone(self);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    // The remote command can continue after its caller is cancelled. Keep the
    // selection lock until its response so another target cannot overtake it.
    tokio::spawn(async move {
      let execute = async {
        let closed = if timeout_ms == 0 {
          session.closed.lock().await
        } else {
          tokio::time::timeout_at(deadline, session.closed.lock())
            .await
            .map_err(|_| FerriError::timeout("waiting for WebDriver command", timeout_ms))?
        };
        if *closed || session.uncertain.load(std::sync::atomic::Ordering::Acquire) {
          return Err(FerriError::target_closed(Some(
            "WebDriver session is closed or its command state is uncertain".into(),
          )));
        }
        let operation = async {
          session.select(&target, timeout_ms).await?;
          let mut result = Value::Null;
          for command in commands {
            result = session.request(command, timeout_ms).await?;
          }
          Ok(result)
        };
        if timeout_ms == 0 {
          return operation.await;
        }
        if let Ok(result) = tokio::time::timeout_at(deadline, operation).await {
          result
        } else {
          session.uncertain.store(true, std::sync::atomic::Ordering::Release);
          Err(FerriError::timeout("executing WebDriver command", timeout_ms))
        }
      };
      execute.await
    })
    .await
    .map_err(|error| FerriError::backend(format!("WebDriver command task failed: {error}")))?
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
    let mut request = self.client.request(command.method, url);
    if let Some(body) = command.body {
      request = request.json(&body);
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
    decode_response(status, payload, &method, timeout_ms)
  }

  pub async fn close(&self) -> Result<()> {
    let mut closed = self.closed.lock().await;
    if !*closed {
      delete_session(&self.client, self.url.clone()).await?;
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
      runtime.spawn(async move {
        if let Err(error) = delete_session(&client, url).await {
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
      "unknown command" | "unsupported operation" => FerriError::unsupported(format!("{method}: {detail}")),
      _ => FerriError::protocol(method, format!("server returned {status}, error {detail}")),
    });
  }
  if !status.is_success() {
    return Err(FerriError::protocol(method, format!("server returned {status}")));
  }
  Ok(value)
}

async fn delete_session(client: &reqwest::Client, url: Url) -> Result<()> {
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
  let payload = response
    .json::<Value>()
    .await
    .map_err(|error| request_error(error, "WebDriver session deletion", 5000))?;
  if payload.pointer("/value/error").and_then(Value::as_str) == Some("invalid session id") {
    return Ok(());
  }
  Err(FerriError::protocol(
    "WebDriver DELETE /session",
    format!("server returned {status}"),
  ))
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

  async fn request(socket: &mut tokio::net::TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    let (head, length) = loop {
      let count = socket.read(&mut buffer).await.unwrap();
      assert_ne!(count, 0);
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
      assert_ne!(count, 0);
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
    (line, body)
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

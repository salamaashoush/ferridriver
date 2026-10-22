//! `browserType.launchServer()`: a launched Chromium whose `DevTools` endpoint
//! other processes can reach.
//!
//! The browser listens for CDP on loopback only, as every launch here does.
//! The server in front of it is what a remote client dials: one WebSocket per
//! client, admitted on the unguessable `wsPath` and, when one is set, a token,
//! then joined frame for frame to a connection of its own to the browser.
//! Nothing is translated, so any CDP client works, `connectOverCDP` included.
//!
//! A launch proxy wanting credentials is answered by the server itself, on a
//! `DevTools` connection of its own that attaches to every target before it
//! runs, so a client's contexts reach the proxy without knowing its password.

use std::net::SocketAddr;
use std::sync::Arc;

use futures::{SinkExt as _, StreamExt as _};
use tokio_tungstenite::tungstenite::{Message, handshake::server, http};

use crate::browser::Browser;
use crate::browser_type::BrowserType;
use crate::error::{FerriError, Result};
use crate::options::{BrowserKind, LaunchOptions};

/// Playwright's `launchServer` options: the launch itself plus where the
/// server listens.
#[derive(Debug, Clone, Default)]
pub struct LaunchServerOptions {
  pub launch: LaunchOptions,
  pub serve: ServeOptions,
}

/// Where a [`BrowserServer`] listens and whom it admits.
#[derive(Debug, Clone, Default)]
pub struct ServeOptions {
  /// Address to listen on. `127.0.0.1` when unset; a non-loopback address
  /// serves the browser to the network, so set a `token` with it.
  pub host: Option<String>,
  /// Port to listen on; `0` or unset picks a free one.
  pub port: Option<u16>,
  /// Path the endpoint is served at. Unset, it is an unguessable string,
  /// which is all that guards a server without a `token`.
  pub ws_path: Option<String>,
  /// A secret every client must present, as `Authorization: Bearer <token>`
  /// or a `token` query parameter. The endpoint URL carries it.
  pub token: Option<String>,
}

/// A launched browser served to other processes. Dropping it stops serving;
/// [`Self::close`] also closes the browser.
pub struct BrowserServer {
  browser: Browser,
  ws_endpoint: String,
  local_addr: SocketAddr,
  stop: tokio::sync::watch::Sender<bool>,
  accept: tokio::task::JoinHandle<()>,
  proxy_answers: Option<tokio::task::JoinHandle<()>>,
}

impl BrowserServer {
  /// Playwright: `browserServer.wsEndpoint()`. The URL a client dials,
  /// token included when the server has one.
  #[must_use]
  pub fn ws_endpoint(&self) -> &str {
    &self.ws_endpoint
  }

  /// The address the server is listening on.
  #[must_use]
  pub fn local_addr(&self) -> SocketAddr {
    self.local_addr
  }

  /// The browser being served, for the process that launched it.
  #[must_use]
  pub fn browser(&self) -> &Browser {
    &self.browser
  }

  /// Resolves once the served browser is gone, closed or crashed.
  ///
  /// # Errors
  ///
  /// The browser's state cannot be read.
  pub async fn disconnected(&self) -> Result<()> {
    let closed = {
      let state = self.browser.state().read().await;
      state
        .page_open_plan(&self.browser.default_context().key)?
        .browser
        .closed()
    };
    closed.await;
    Ok(())
  }

  /// Playwright: `browserServer.close()`. Stop accepting clients, drop the
  /// ones connected, and close the browser.
  ///
  /// # Errors
  ///
  /// The browser fails to close.
  pub async fn close(mut self) -> Result<()> {
    self.stop_serving();
    self.browser.close().await
  }

  fn stop_serving(&mut self) {
    self.stop.send_replace(true);
    self.accept.abort();
    if let Some(answers) = self.proxy_answers.take() {
      answers.abort();
    }
  }
}

impl Drop for BrowserServer {
  fn drop(&mut self) {
    self.stop_serving();
  }
}

impl BrowserType {
  /// Playwright: `browserType.launchServer(options?) -> BrowserServer`.
  ///
  /// Serves the Chrome `DevTools` Protocol rather than Playwright's own wire
  /// protocol, so it is Chromium only; a client reaches it with
  /// `connectOverCDP` or `connect`.
  ///
  /// # Errors
  ///
  /// The browser is not Chromium, fails to launch, or the address cannot be
  /// bound.
  pub async fn launch_server(self, options: LaunchServerOptions) -> Result<BrowserServer> {
    let launcher = Self::server_launcher(self.kind())?;
    let browser = launcher.launch(options.launch).await?;
    BrowserServer::serve(browser, options.serve).await
  }

  /// The browser type [`Self::launch_server`] launches with: Chromium over a
  /// `DevTools` WebSocket, the only transport a second client can dial.
  ///
  /// # Errors
  ///
  /// `kind` is not Chromium.
  pub fn server_launcher(kind: BrowserKind) -> Result<Self> {
    if kind != BrowserKind::Chromium {
      return Err(FerriError::unsupported(format!(
        "launchServer serves the Chrome DevTools Protocol, which {} does not speak",
        kind.name()
      )));
    }
    Ok(Self::with_backend(
      BrowserKind::Chromium,
      crate::backend::BackendKind::CdpWs,
    ))
  }
}

impl BrowserServer {
  /// Serve `browser`, launched by [`BrowserType::server_launcher`], to other
  /// processes. Closing the server closes it. When the browser was launched
  /// with a proxy that wants credentials, the server answers the proxy's
  /// challenges for every page, those of contexts its clients create
  /// included.
  ///
  /// # Errors
  ///
  /// The browser is not Chromium over a `DevTools` WebSocket, the address
  /// cannot be bound, or the browser refuses to attach to its targets.
  pub async fn serve(browser: Browser, options: ServeOptions) -> Result<Self> {
    let (upstream, proxy) = {
      let state = browser.state().read().await;
      let key = &browser.default_context().key;
      let plan = state.page_open_plan(key)?;
      let upstream = plan
        .browser
        .cdp_ws_endpoint()
        .map(str::to_string)
        .ok_or_else(|| FerriError::backend("launchServer: the browser exposes no DevTools endpoint"))?;
      (upstream, state.effective_proxy(&key.to_composite()))
    };
    let host = options.host.clone().unwrap_or_else(|| "127.0.0.1".to_string());
    let listener = tokio::net::TcpListener::bind((host.as_str(), options.port.unwrap_or(0)))
      .await
      .map_err(|e| FerriError::backend(format!("launchServer: cannot listen on {host}: {e}")))?;
    let local_addr = listener
      .local_addr()
      .map_err(|e| FerriError::backend(format!("launchServer: {e}")))?;

    let ws_path = normalized_path(options.ws_path.as_deref().map_or_else(random_secret, str::to_string));
    let admission = Arc::new(Admission {
      path: ws_path.clone(),
      token: options.token.as_deref().map(digest),
    });
    let shown_host = match local_addr.ip() {
      ip if ip.is_unspecified() => "127.0.0.1".to_string(),
      std::net::IpAddr::V6(ip) => format!("[{ip}]"),
      std::net::IpAddr::V4(ip) => ip.to_string(),
    };
    let mut ws_endpoint = format!("ws://{shown_host}:{}{ws_path}", local_addr.port());
    if let Some(token) = &options.token {
      ws_endpoint.push_str("?token=");
      ws_endpoint.push_str(&percent_encode(token));
    }

    // An authenticated SOCKS5 proxy is reached through a relay that logs in
    // for the browser, so it never challenges.
    let proxy_answers = match proxy
      .filter(|proxy| !proxy.is_authenticated_socks())
      .as_ref()
      .and_then(crate::options::ProxyConfig::credentials)
    {
      Some(credentials) => Some(answer_proxy_challenges(&upstream, credentials).await?),
      None => None,
    };
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let accept = tokio::spawn(accept_clients(listener, upstream, admission, stopped));
    Ok(Self {
      browser,
      ws_endpoint,
      local_addr,
      stop,
      accept,
      proxy_answers,
    })
  }
}

/// What a client must present to be joined to the browser.
struct Admission {
  path: String,
  /// SHA-256 of the token, compared digest to digest so the comparison
  /// time says nothing about how much of a guess was right.
  token: Option<[u8; 32]>,
}

impl Admission {
  fn admits(&self, request: &server::Request) -> std::result::Result<(), &'static str> {
    if request.uri().path() != self.path {
      return Err("unknown endpoint");
    }
    let Some(expected) = &self.token else {
      return Ok(());
    };
    let bearer = request
      .headers()
      .get(http::header::AUTHORIZATION)
      .and_then(|value| value.to_str().ok())
      .and_then(|value| value.strip_prefix("Bearer "))
      .map(str::to_string);
    let query = request.uri().query().and_then(|query| {
      query.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == "token").then(|| percent_decode(value))
      })
    });
    if [bearer, query]
      .into_iter()
      .flatten()
      .any(|token| digest(&token) == *expected)
    {
      Ok(())
    } else {
      Err("missing or wrong token")
    }
  }
}

async fn accept_clients(
  listener: tokio::net::TcpListener,
  upstream: String,
  admission: Arc<Admission>,
  stopped: tokio::sync::watch::Receiver<bool>,
) {
  loop {
    let mut stop = stopped.clone();
    let accepted = tokio::select! {
      accepted = listener.accept() => accepted,
      () = until_stopped(&mut stop) => return,
    };
    let Ok((socket, peer)) = accepted else { continue };
    let upstream = upstream.clone();
    let admission = Arc::clone(&admission);
    let stopped = stopped.clone();
    tokio::spawn(async move {
      if let Err(error) = join(socket, &upstream, &admission, stopped).await {
        tracing::debug!(target: "ferridriver::browser_server", %peer, %error, "client connection ended");
      }
    });
  }
}

/// Admit one client and relay its frames to a browser connection of its own
/// until either side hangs up or the server stops.
async fn join(
  socket: tokio::net::TcpStream,
  upstream: &str,
  admission: &Admission,
  mut stopped: tokio::sync::watch::Receiver<bool>,
) -> std::result::Result<(), String> {
  let check = |request: &server::Request, response: server::Response| match admission.admits(request) {
    Ok(()) => Ok(response),
    Err(reason) => {
      let mut refusal = server::ErrorResponse::new(Some(reason.to_string()));
      *refusal.status_mut() = if reason == "unknown endpoint" {
        http::StatusCode::NOT_FOUND
      } else {
        http::StatusCode::UNAUTHORIZED
      };
      Err(refusal)
    },
  };
  let client = tokio_tungstenite::accept_hdr_async(socket, check)
    .await
    .map_err(|e| format!("handshake: {e}"))?;
  let (browser, _) = tokio_tungstenite::connect_async(upstream)
    .await
    .map_err(|e| format!("browser: {e}"))?;
  let (mut client_tx, mut client_rx) = client.split();
  let (mut browser_tx, mut browser_rx) = browser.split();
  loop {
    tokio::select! {
      frame = client_rx.next() => match frame {
        Some(Ok(Message::Close(_)) | Err(_)) | None => break,
        Some(Ok(frame)) => if browser_tx.send(frame).await.is_err() { break },
      },
      frame = browser_rx.next() => match frame {
        Some(Ok(Message::Close(_)) | Err(_)) | None => break,
        Some(Ok(frame)) => if client_tx.send(frame).await.is_err() { break },
      },
      () = until_stopped(&mut stopped) => break,
    }
  }
  let _ = client_tx.close().await;
  let _ = browser_tx.close().await;
  Ok(())
}

fn auto_attach() -> serde_json::Value {
  serde_json::json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true })
}

/// Connect to the browser at `upstream` and answer the proxy's challenges on
/// every target, present and future, until the task is aborted. Returns once
/// the browser holds each new target until it is armed.
async fn answer_proxy_challenges(
  upstream: &str,
  credentials: crate::options::HttpCredentials,
) -> Result<tokio::task::JoinHandle<()>> {
  let refused = |e: &dyn std::fmt::Display| FerriError::backend(format!("launchServer: proxy credentials: {e}"));
  let (socket, _) = tokio_tungstenite::connect_async(upstream)
    .await
    .map_err(|e| refused(&e))?;
  let (mut sink, mut stream) = socket.split();
  let attach = serde_json::json!({ "id": 0, "method": "Target.setAutoAttach", "params": auto_attach() });
  sink
    .send(Message::Text(attach.to_string().into()))
    .await
    .map_err(|e| refused(&e))?;
  let mut early = Vec::new();
  loop {
    let message = match stream.next().await {
      Some(Ok(Message::Text(text))) => serde_json::from_str::<serde_json::Value>(&text).unwrap_or_default(),
      Some(Ok(_)) => continue,
      Some(Err(e)) => return Err(refused(&e)),
      None => return Err(refused(&"the browser closed the connection")),
    };
    if message["id"] == 0 {
      if let Some(error) = message.get("error") {
        return Err(refused(error));
      }
      break;
    }
    // Targets already open attach before the reply arrives.
    early.push(message);
  }
  Ok(tokio::spawn(async move {
    let mut answers = ProxyAnswers {
      sink,
      next_id: 0,
      answered: rustc_hash::FxHashSet::default(),
      credentials,
    };
    for message in early {
      if answers.handle(&message).await.is_err() {
        return;
      }
    }
    while let Some(Ok(message)) = stream.next().await {
      if let Message::Text(text) = message
        && let Ok(message) = serde_json::from_str::<serde_json::Value>(&text)
        && answers.handle(&message).await.is_err()
      {
        return;
      }
    }
  }))
}

/// Bounds the requests remembered as answered on a long-lived server.
const ANSWERED_REMEMBERED: usize = 10_000;

type BrowserSink = futures::stream::SplitSink<
  tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
  Message,
>;

struct ProxyAnswers {
  sink: BrowserSink,
  next_id: u64,
  /// Requests already answered once: a second challenge means the proxy
  /// refused the credentials, and answering again would never end.
  answered: rustc_hash::FxHashSet<String>,
  credentials: crate::options::HttpCredentials,
}

impl ProxyAnswers {
  async fn command(
    &mut self,
    session: &str,
    method: &str,
    params: serde_json::Value,
  ) -> std::result::Result<(), tokio_tungstenite::tungstenite::Error> {
    self.next_id += 1;
    let message = serde_json::json!({ "id": self.next_id, "sessionId": session, "method": method, "params": params });
    self.sink.send(Message::Text(message.to_string().into())).await
  }

  async fn handle(
    &mut self,
    message: &serde_json::Value,
  ) -> std::result::Result<(), tokio_tungstenite::tungstenite::Error> {
    let session = message["sessionId"].as_str().unwrap_or_default().to_string();
    let params = &message["params"];
    match message["method"].as_str() {
      Some("Target.attachedToTarget") => {
        let Some(attached) = params["sessionId"].as_str() else {
          return Ok(());
        };
        // In order on one session: interception is on before the target
        // runs, and the targets it starts are held the same way.
        let fetch = serde_json::json!({ "handleAuthRequests": true, "patterns": [{ "urlPattern": "*" }] });
        self.command(attached, "Fetch.enable", fetch).await?;
        self.command(attached, "Target.setAutoAttach", auto_attach()).await?;
        self
          .command(attached, "Runtime.runIfWaitingForDebugger", serde_json::json!({}))
          .await
      },
      Some("Fetch.requestPaused") => {
        let request = serde_json::json!({ "requestId": params["requestId"] });
        self.command(&session, "Fetch.continueRequest", request).await
      },
      Some("Fetch.authRequired") => {
        let request = params["requestId"].as_str().unwrap_or_default().to_string();
        let response = if params["authChallenge"]["source"] != "Proxy" {
          serde_json::json!({ "response": "Default" })
        } else if !self.answered.contains(&request) {
          if self.answered.len() >= ANSWERED_REMEMBERED {
            self.answered.clear();
          }
          self.answered.insert(request.clone());
          serde_json::json!({
            "response": "ProvideCredentials",
            "username": self.credentials.username,
            "password": self.credentials.password,
          })
        } else {
          serde_json::json!({ "response": "CancelAuth" })
        };
        let answer = serde_json::json!({ "requestId": request, "authChallengeResponse": response });
        self.command(&session, "Fetch.continueWithAuth", answer).await
      },
      _ => Ok(()),
    }
  }
}

async fn until_stopped(stopped: &mut tokio::sync::watch::Receiver<bool>) {
  while !*stopped.borrow_and_update() {
    if stopped.changed().await.is_err() {
      return;
    }
  }
}

fn digest(token: &str) -> [u8; 32] {
  use sha2::Digest as _;
  sha2::Sha256::digest(token.as_bytes()).into()
}

/// 32 random bytes, hex: the default `wsPath`, and a token a caller can mint.
#[must_use]
pub fn random_secret() -> String {
  use ring::rand::SecureRandom as _;
  let mut bytes = [0u8; 32];
  if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
    // The system source failing is not a reason to serve a guessable path;
    // a process-unique fallback from the clock and address space keeps it
    // unguessable to anyone not on this machine.
    let seed = format!("{:?}{:p}", std::time::SystemTime::now(), &bytes);
    bytes = digest(&seed);
  }
  bytes.iter().fold(String::with_capacity(64), |mut hex, byte| {
    use std::fmt::Write as _;
    let _ = write!(hex, "{byte:02x}");
    hex
  })
}

fn normalized_path(path: String) -> String {
  if path.starts_with('/') {
    path
  } else {
    format!("/{path}")
  }
}

fn percent_encode(value: &str) -> String {
  value
    .bytes()
    .map(|byte| match byte {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (byte as char).to_string(),
      other => format!("%{other:02X}"),
    })
    .collect()
}

fn percent_decode(value: &str) -> String {
  let bytes = value.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut index = 0;
  while index < bytes.len() {
    let decoded = (bytes[index] == b'%')
      .then(|| bytes.get(index + 1..index + 3))
      .flatten()
      .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
    if let Some(byte) = decoded {
      out.push(byte);
      index += 3;
    } else {
      out.push(if bytes[index] == b'+' { b' ' } else { bytes[index] });
      index += 1;
    }
  }
  String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
  use super::{Admission, digest, percent_decode, percent_encode};
  use tokio_tungstenite::tungstenite::http;

  fn request(uri: &str, bearer: Option<&str>) -> http::Request<()> {
    let mut builder = http::Request::builder().uri(uri);
    if let Some(token) = bearer {
      builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(()).unwrap()
  }

  #[test]
  fn a_client_needs_the_path_and_the_token_in_either_place() {
    let admission = Admission {
      path: "/secret-path".into(),
      token: Some(digest("t0k/en")),
    };
    assert!(admission.admits(&request("/secret-path?token=t0k%2Fen", None)).is_ok());
    assert!(admission.admits(&request("/secret-path", Some("t0k/en"))).is_ok());
    assert!(admission.admits(&request("/secret-path", None)).is_err());
    assert!(admission.admits(&request("/secret-path?token=wrong", None)).is_err());
    assert!(admission.admits(&request("/other?token=t0k%2Fen", None)).is_err());
  }

  #[test]
  fn without_a_token_the_path_is_the_secret() {
    let admission = Admission {
      path: "/abc".into(),
      token: None,
    };
    assert!(admission.admits(&request("/abc", None)).is_ok());
    assert!(admission.admits(&request("/abd", None)).is_err());
  }

  #[test]
  fn a_token_survives_the_url() {
    let token = "a b&c=d/é";
    assert_eq!(percent_decode(&percent_encode(token)), token);
  }
}

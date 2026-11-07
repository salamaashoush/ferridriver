//! A session host connected to a browser another process serves, the way a
//! web app runs one: `browser serve` owns Chromium, `session open --connect`
//! records and relays input to it, and a raw CDP client of the app's own
//! watches the tabs. A popup a page opens has to become a page of the
//! session's context there too: the active page, a page `input` can name by
//! the target id the watcher saw, and a page the recorder records in.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::process::{Child, Command as Process, Stdio};
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use ferridriver_session::{
  Command, EventPayload, INPUT_VERB, RECORDER_START_VERB, RECORDER_STOP_VERB, RecordingResult, Registry, Response,
  SessionClient,
};
use futures::{SinkExt as _, StreamExt as _};
use tokio::sync::mpsc;

const SHOP: &str = "<!doctype html><title>Shop</title><meta name=viewport content=width=device-width><body style=margin:0>\
<a href=/help target=_blank style=position:absolute;left:10px;top:10px;width:120px;height:30px>Help</a></body>";

const HELP: &str = "<!doctype html><title>Help</title><meta name=viewport content=width=device-width><body style=margin:0>\
<label style=position:absolute;left:10px;top:10px>Question <input style=width:200px></label>\
<button type=button style=position:absolute;left:10px;top:60px;width:120px;height:30px>Ask</button></body>";

fn ferridriver_bin() -> std::path::PathBuf {
  std::env::var_os("FERRIDRIVER_BIN").map_or_else(
    || std::path::PathBuf::from(env!("CARGO_BIN_EXE_ferridriver")),
    std::path::PathBuf::from,
  )
}

/// Serve the two pages over HTTP, a thread per connection and every reply
/// closing it, so a browser's idle preconnection never holds up a request.
fn serve_pages() -> anyhow::Result<String> {
  let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
  let origin = format!("http://{}", listener.local_addr()?);
  std::thread::spawn(move || {
    for stream in listener.incoming().flatten() {
      std::thread::spawn(move || {
        let mut stream = stream;
        let mut request = [0_u8; 4096];
        let read = stream.read(&mut request).unwrap_or(0);
        let head = String::from_utf8_lossy(&request[..read]);
        let path = head.split_whitespace().nth(1).unwrap_or("/");
        let port = head
          .lines()
          .find_map(|line| line.strip_prefix("Host: "))
          .and_then(|host| host.trim().rsplit(':').next())
          .unwrap_or_default()
          .to_string();
        // The help page from another site, which site isolation puts in
        // a process of its own.
        let framed = format!("<!doctype html><title>Framed</title><iframe src=http://localhost:{port}/help></iframe>");
        let (status, body) = match path {
          "/" => ("200 OK", SHOP),
          "/help" => ("200 OK", HELP),
          "/framed" => ("200 OK", framed.as_str()),
          _ => ("404 Not Found", ""),
        };
        let _ = write!(
          stream,
          "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
          body.len()
        );
      });
    }
  });
  Ok(origin)
}

/// `browser serve`, killed when dropped.
struct Served {
  child: Child,
  endpoint: String,
}

impl Served {
  fn start() -> anyhow::Result<Self> {
    Self::start_with(&[])
  }

  fn start_with(args: &[&str]) -> anyhow::Result<Self> {
    let mut child = Process::new(ferridriver_bin())
      .args(["browser", "serve"])
      .args(args)
      .env("FERRIDRIVER_NO_INHERIT", "1")
      .stdout(Stdio::piped())
      .stderr(Stdio::null())
      .spawn()?;
    let stdout = child.stdout.take().context("serve stdout")?;
    let mut endpoint = String::new();
    BufReader::new(stdout).read_line(&mut endpoint)?;
    let endpoint = endpoint.trim().to_string();
    ensure!(endpoint.starts_with("ws://"), "serve printed {endpoint:?}");
    Ok(Self { child, endpoint })
  }
}

impl Drop for Served {
  fn drop(&mut self) {
    let _ = self.child.kill();
    let _ = self.child.wait();
  }
}

/// The CLI against the test's own session registry.
fn cli(registry: &std::path::Path, args: &[&str]) -> anyhow::Result<std::process::Output> {
  cli_in(registry, registry.parent().unwrap_or(registry), args)
}

/// [`cli`] run in `dir`, whose config file is the only one it reads.
fn cli_in(registry: &std::path::Path, dir: &std::path::Path, args: &[&str]) -> anyhow::Result<std::process::Output> {
  Ok(
    Process::new(ferridriver_bin())
      .args(args)
      .current_dir(dir)
      .env("FERRIDRIVER_SESSION_DIR", registry)
      .env("FERRIDRIVER_NO_INHERIT", "1")
      .output()?,
  )
}

/// The app's own CDP client: discovers targets and attaches to every page,
/// reporting each page's target id as it appears.
async fn watch_pages(endpoint: &str) -> anyhow::Result<mpsc::UnboundedReceiver<String>> {
  use tokio_tungstenite::tungstenite::Message;
  let (socket, _) = tokio_tungstenite::connect_async(endpoint).await?;
  let (mut sink, mut source) = socket.split();
  sink
    .send(Message::text(
      serde_json::json!({"id": 1, "method": "Target.setDiscoverTargets", "params": {"discover": true}}).to_string(),
    ))
    .await?;
  let (pages, seen) = mpsc::unbounded_channel();
  tokio::spawn(async move {
    let mut id = 1;
    while let Some(Ok(message)) = source.next().await {
      let Ok(text) = message.to_text() else { continue };
      let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        continue;
      };
      if value["method"] != "Target.targetCreated" || value["params"]["targetInfo"]["type"] != "page" {
        continue;
      }
      let Some(target) = value["params"]["targetInfo"]["targetId"].as_str() else {
        continue;
      };
      id += 1;
      let attach = serde_json::json!({
        "id": id,
        "method": "Target.attachToTarget",
        "params": {"targetId": target, "flatten": true},
      });
      if sink.send(Message::text(attach.to_string())).await.is_err() || pages.send(target.to_string()).is_err() {
        break;
      }
    }
  });
  Ok(seen)
}

async fn call(endpoint: &str, verb: &str, args: serde_json::Value) -> anyhow::Result<Response> {
  let mut client = SessionClient::connect(endpoint).await?;
  let reply = client.call(Command::new(1, verb, args.clone())).await?;
  ensure!(reply.ok, "{verb} {args}: {:?}", reply.error);
  Ok(reply)
}

async fn click(endpoint: &str, (x, y): (f64, f64), page: Option<&str>) -> anyhow::Result<()> {
  for kind in ["mouse_move", "mouse_down", "mouse_up"] {
    let mut args = serde_json::json!({"type": kind, "x": x, "y": y});
    if let Some(page) = page {
      args["page"] = page.into();
    }
    call(endpoint, INPUT_VERB, args).await?;
  }
  Ok(())
}

async fn next<T>(receiver: &mut mpsc::UnboundedReceiver<T>, what: &str) -> anyhow::Result<T> {
  tokio::time::timeout(Duration::from_secs(15), receiver.recv())
    .await
    .with_context(|| format!("timed out waiting for {what}"))?
    .with_context(|| format!("stream ended waiting for {what}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_popup_of_a_served_browser_is_a_page_of_the_session() -> anyhow::Result<()> {
  let origin = serve_pages()?;
  let served = Served::start()?;
  let scratch = tempfile::tempdir()?;
  let registry_dir = scratch.path().join("registry");
  let opened = cli(
    &registry_dir,
    &["session", "open", "served", "--headless", "--connect", &served.endpoint],
  )?;
  ensure!(
    opened.status.success(),
    "session open: {}",
    String::from_utf8_lossy(&opened.stderr)
  );
  let result = drive(&origin, &served, &Registry::open_at(&registry_dir)?).await;
  let _ = cli(&registry_dir, &["session", "close", "served"]);
  let (source, actions) = result?;

  let body: Vec<&str> = source
    .lines()
    .skip_while(|line| !line.starts_with("test("))
    .skip(2)
    .take_while(|line| *line != "});")
    .collect();
  assert_eq!(
    body,
    [
      "  const page1Promise = page.waitForEvent('popup');",
      "  await page.getByRole('link', { name: 'Help' }).click();",
      "  const page1 = await page1Promise;",
      "  await page1.getByRole('textbox', { name: 'Question' }).click();",
      "  await page1.getByRole('textbox', { name: 'Question' }).fill('sashoush');",
      "  await page1.getByRole('button', { name: 'Ask' }).click();",
    ],
    "{source}"
  );
  ensure!(
    actions.iter().filter(|action| action.page == "page1").count() == 3,
    "{actions:#?}"
  );
  Ok(())
}

async fn drive(
  origin: &str,
  served: &Served,
  registry: &Registry,
) -> anyhow::Result<(String, Vec<ferridriver::codegen::RecordedAction>)> {
  let endpoint = registry.get("served")?.context("the session's descriptor")?.endpoint;
  let mut targets = watch_pages(&served.endpoint).await?;

  // The recording opens its page on the shop, as the app starts it.
  let (events, mut streamed) = mpsc::unbounded_channel();
  let mut recorder = SessionClient::connect(&endpoint).await?;
  let start = Command::new(
    1,
    RECORDER_START_VERB,
    serde_json::json!({"target": "test", "url": format!("{origin}/")}),
  );
  let recording = tokio::spawn(async move {
    recorder
      .call_with_events(start, |event| {
        let _ = events.send(event.payload);
      })
      .await
  });
  let opening = next(&mut streamed, "the opening navigation").await?;
  ensure!(matches!(opening, EventPayload::Recorded { .. }), "{opening:?}");
  // Every tab open so far, which the watcher reports first.
  let mut before = vec![next(&mut targets, "the shop's target").await?];
  while let Ok(Some(target)) = tokio::time::timeout(Duration::from_millis(500), targets.recv()).await {
    before.push(target);
  }

  // The link opens a tab of its own; the watcher learns its target id.
  click(&endpoint, (40.0, 20.0), None).await?;
  let help = loop {
    let target = next(&mut targets, "the popup's target").await?;
    if !before.contains(&target) {
      break target;
    }
  };

  // A person clicks the page they see: input that reaches the popup
  // before its document does lands on nothing.
  until_shown(&served.endpoint, &help, "Help").await?;

  // Named, the click lands in the popup. Unnamed, the text follows it
  // there too: the popup is the active page.
  click(&endpoint, (150.0, 20.0), Some(&help)).await?;
  call(
    &endpoint,
    INPUT_VERB,
    serde_json::json!({"type": "insert_text", "text": "sashoush"}),
  )
  .await?;
  click(&endpoint, (40.0, 75.0), Some(&help)).await?;

  let stopped = call(&endpoint, RECORDER_STOP_VERB, serde_json::json!({})).await?;
  let result: RecordingResult = serde_json::from_str(&stopped.text)?;
  match recording.await? {
    Ok(reply) if reply.ok => {},
    other => bail!("recorder.start ended with {other:?}"),
  }
  Ok((result.source, result.actions))
}

/// Wait until the page `target` shows the document titled `title`, as the
/// app's own client sees it.
async fn until_shown(endpoint: &str, target: &str, title: &str) -> anyhow::Result<()> {
  let mut cdp = Cdp::connect(endpoint).await?;
  let attached = cdp
    .send(
      None,
      "Target.attachToTarget",
      serde_json::json!({"targetId": target, "flatten": true}),
    )
    .await?;
  let session = attached["sessionId"].as_str().context("a session")?.to_string();
  let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
  loop {
    let shown = cdp
      .send(
        Some(&session),
        "Runtime.evaluate",
        serde_json::json!({"expression": "document.readyState === 'complete' ? document.title : ''", "returnByValue": true}),
      )
      .await?;
    if shown["result"]["value"] == title {
      return Ok(());
    }
    ensure!(tokio::time::Instant::now() < deadline, "{target} never showed {title}");
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
}

/// A CDP client of another app's: one command at a time, answers matched by id.
struct Cdp {
  socket: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
  next: u64,
}

impl Cdp {
  async fn connect(endpoint: &str) -> anyhow::Result<Self> {
    Ok(Self {
      socket: tokio_tungstenite::connect_async(endpoint).await?.0,
      next: 0,
    })
  }

  async fn send(
    &mut self,
    session: Option<&str>,
    method: &str,
    params: serde_json::Value,
  ) -> anyhow::Result<serde_json::Value> {
    use tokio_tungstenite::tungstenite::Message;
    self.next += 1;
    let id = self.next;
    let mut command = serde_json::json!({"id": id, "method": method, "params": params});
    if let Some(session) = session {
      command["sessionId"] = session.into();
    }
    self.socket.send(Message::text(command.to_string())).await?;
    loop {
      let message = tokio::time::timeout(Duration::from_secs(15), self.socket.next())
        .await
        .with_context(|| format!("no answer to {method}"))?
        .with_context(|| format!("socket closed before answering {method}"))??;
      let Ok(text) = message.to_text() else { continue };
      let value: serde_json::Value = serde_json::from_str(text)?;
      if value["id"] == id {
        ensure!(value.get("error").is_none(), "{method}: {}", value["error"]);
        return Ok(value["result"].clone());
      }
    }
  }
}

/// What a page says about the device it runs on.
const DEVICE: &str =
  "[devicePixelRatio, matchMedia('(pointer: coarse)').matches, navigator.maxTouchPoints, innerWidth, innerHeight]";

const PHONE_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 \
(KHTML, like Gecko) Version/27.2 Mobile/15E148 Safari/604.1";

#[tokio::test(flavor = "multi_thread")]
async fn a_session_host_on_a_served_browser_opens_its_pages_as_the_configured_phone() -> anyhow::Result<()> {
  let origin = serve_pages()?;
  let served = Served::start()?;

  // The browser's own tab, which the session host takes over, and another
  // app's page in a context of its own, both open before the host arrives.
  let mut other = Cdp::connect(&served.endpoint).await?;
  other
    .send(None, "Target.createTarget", serde_json::json!({"url": "about:blank"}))
    .await?;
  let context = other
    .send(None, "Target.createBrowserContext", serde_json::json!({}))
    .await?;
  let own = other
    .send(
      None,
      "Target.createTarget",
      serde_json::json!({"url": format!("{origin}/help?own"), "browserContextId": context["browserContextId"]}),
    )
    .await?;

  let scratch = tempfile::tempdir()?;
  let config = serde_json::json!({"mcp": {"browser": {
    "viewport": {"width": 393, "height": 659},
    "deviceScaleFactor": 3,
    "isMobile": true,
    "hasTouch": true,
    "userAgent": PHONE_AGENT,
  }}});
  std::fs::write(scratch.path().join("ferridriver.json"), config.to_string())?;
  let registry_dir = scratch.path().join("registry");
  let opened = cli_in(
    &registry_dir,
    scratch.path(),
    &["session", "open", "phone", "--headless", "--connect", &served.endpoint],
  )?;
  ensure!(
    opened.status.success(),
    "session open: {}",
    String::from_utf8_lossy(&opened.stderr)
  );
  let result = observe(&origin, &Registry::open_at(&registry_dir)?).await;
  let _ = cli(&registry_dir, &["session", "close", "phone"]);
  let seen = result?;

  let phone = serde_json::json!([3, true, 5, 393, 659]);
  for (which, value) in ["the recorded page", "its popup", "a page it opens"]
    .iter()
    .zip(seen["pages"].as_array().context("the pages")?)
  {
    assert_eq!(value, &phone, "{which}: {seen}");
  }
  assert_eq!(
    seen["agents"],
    serde_json::json!([PHONE_AGENT, PHONE_AGENT, PHONE_AGENT]),
    "{seen}"
  );

  // The other app's page is not the session's: the host does not list it,
  // and it keeps the browser's own settings when the app looks.
  ensure!(seen["foreign"].is_null(), "{seen}");
  let attached = other
    .send(
      None,
      "Target.attachToTarget",
      serde_json::json!({"targetId": own["targetId"], "flatten": true}),
    )
    .await?;
  let own_session = attached["sessionId"].as_str().context("a session")?.to_string();
  let untouched = other
    .send(
      Some(&own_session),
      "Runtime.evaluate",
      serde_json::json!({"expression": format!("JSON.stringify([{DEVICE}, navigator.userAgent])"), "returnByValue": true}),
    )
    .await?;
  let untouched: serde_json::Value = serde_json::from_str(untouched["result"]["value"].as_str().context("a value")?)?;
  ensure!(untouched[0][0] == 1 && untouched[0][2] == 0, "{untouched}");
  ensure!(untouched[1] != PHONE_AGENT, "{untouched}");
  Ok(())
}

/// Start a recording on the shop as the app does, open its popup and a
/// page of the session's own, and read what each reports.
async fn observe(origin: &str, registry: &Registry) -> anyhow::Result<serde_json::Value> {
  let endpoint = registry.get("phone")?.context("the session's descriptor")?.endpoint;
  let (events, mut streamed) = mpsc::unbounded_channel();
  let mut recorder = SessionClient::connect(&endpoint).await?;
  let start = Command::new(
    1,
    RECORDER_START_VERB,
    serde_json::json!({"target": "test", "url": format!("{origin}/")}),
  );
  let recording = tokio::spawn(async move {
    recorder
      .call_with_events(start, |event| {
        let _ = events.send(event.payload);
      })
      .await
  });
  next(&mut streamed, "the opening navigation").await?;
  let script = format!(
    "const popup = page.waitForEvent('popup');
     await page.getByRole('link', {{ name: 'Help' }}).click();
     const opened = await context.newPage();
     await opened.goto('{origin}/help');
     const pages = [page, await popup, opened];
     // A page of a context another app made is not one of the session's.
     const foreign = context.pages().find(p => p.url().endsWith('?own'));
     const seen = {{ pages: [], agents: [], foreign: foreign ? await foreign.evaluate(() => {DEVICE}) : null }};
     for (const p of pages) {{
       seen.pages.push(await p.evaluate(() => {DEVICE}));
       seen.agents.push(await p.evaluate(() => navigator.userAgent));
     }}
     return JSON.stringify(seen);"
  );
  let ran = call(
    &endpoint,
    ferridriver_session::RUN_VERB,
    serde_json::json!({"code": script}),
  )
  .await?;
  call(&endpoint, RECORDER_STOP_VERB, serde_json::json!({})).await?;
  match recording.await? {
    Ok(reply) if reply.ok => {},
    other => bail!("recorder.start ended with {other:?}"),
  }
  let reply: serde_json::Value = serde_json::from_str(&ran.text)?;
  ensure!(reply["status"] == "ok", "{reply}");
  Ok(serde_json::from_str(
    reply["value"].as_str().context("the script's answer")?,
  )?)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_host_leaves_the_pages_of_another_apps_context_alone() -> anyhow::Result<()> {
  let origin = serve_pages()?;
  let served = Served::start()?;

  // Another app's context, holding a blank tab and a page, is all the
  // browser has when the session host connects.
  let mut other = Cdp::connect(&served.endpoint).await?;
  let context = other
    .send(None, "Target.createBrowserContext", serde_json::json!({}))
    .await?;
  let blank = other
    .send(
      None,
      "Target.createTarget",
      serde_json::json!({"url": "about:blank", "browserContextId": context["browserContextId"]}),
    )
    .await?;
  other
    .send(
      None,
      "Target.createTarget",
      serde_json::json!({"url": format!("{origin}/help?own"), "browserContextId": context["browserContextId"]}),
    )
    .await?;

  let scratch = tempfile::tempdir()?;
  let registry_dir = scratch.path().join("registry");
  let opened = cli(
    &registry_dir,
    &["session", "open", "apart", "--headless", "--connect", &served.endpoint],
  )?;
  ensure!(
    opened.status.success(),
    "session open: {}",
    String::from_utf8_lossy(&opened.stderr)
  );
  let ran = async {
    let endpoint = Registry::open_at(&registry_dir)?
      .get("apart")?
      .context("the session's descriptor")?
      .endpoint;
    let reply = call(
      &endpoint,
      ferridriver_session::RUN_VERB,
      serde_json::json!({"code": format!(
        "await page.goto('{origin}/?session');
         return JSON.stringify(context.pages().map(p => p.url()));"
      )}),
    )
    .await?;
    let reply: serde_json::Value = serde_json::from_str(&reply.text)?;
    ensure!(reply["status"] == "ok", "{reply}");
    let listed: Vec<String> = serde_json::from_str(reply["value"].as_str().context("the script's answer")?)?;
    anyhow::Ok(listed)
  }
  .await;
  let _ = cli(&registry_dir, &["session", "close", "apart"]);
  let listed = ran?;

  assert_eq!(listed, [format!("{origin}/?session")], "the session's pages");
  let targets = other.send(None, "Target.getTargets", serde_json::json!({})).await?;
  let theirs = targets["targetInfos"]
    .as_array()
    .context("the targets")?
    .iter()
    .find(|info| info["targetId"] == blank["targetId"])
    .context("the other app's blank tab is still open")?;
  assert_eq!(theirs["url"], "about:blank", "the other app's blank tab: {targets}");
  Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_served_browser_with_site_isolation_loads_a_cross_site_frame() -> anyhow::Result<()> {
  let origin = serve_pages()?;
  let served = Served::start_with(&["--arg=--site-per-process"])?;
  let mut app = Cdp::connect(&served.endpoint).await?;
  let page = app
    .send(
      None,
      "Target.createTarget",
      serde_json::json!({"url": format!("{origin}/framed")}),
    )
    .await?;
  let attached = app
    .send(
      None,
      "Target.attachToTarget",
      serde_json::json!({"targetId": page["targetId"], "flatten": true}),
    )
    .await?;
  let session = attached["sessionId"].as_str().context("a session")?.to_string();
  // The frame from another site loads in a renderer of its own; nothing
  // the server holds may keep it waiting.
  let deadline = std::time::Instant::now() + Duration::from_secs(10);
  loop {
    let state = app
      .send(
        Some(&session),
        "Runtime.evaluate",
        serde_json::json!({"expression": "document.readyState", "returnByValue": true}),
      )
      .await?;
    if state["result"]["value"] == "complete" {
      break;
    }
    ensure!(std::time::Instant::now() < deadline, "the page never loaded: {state}");
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  let targets = app.send(None, "Target.getTargets", serde_json::json!({})).await?;
  let frame = targets["targetInfos"]
    .as_array()
    .context("the targets")?
    .iter()
    .find(|info| info["type"] == "iframe")
    .with_context(|| format!("no frame of its own: {targets}"))?;
  ensure!(
    frame["url"].as_str().is_some_and(|url| url.ends_with("/help")),
    "{frame}"
  );
  Ok(())
}

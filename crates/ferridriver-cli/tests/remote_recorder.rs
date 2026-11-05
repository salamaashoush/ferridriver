//! Watching, driving and recording a headless browser over a session
//! endpoint, the way a web app relays a remote browser: frames out, input
//! in, recorded actions back, and a spec at the end that `ferridriver test`
//! passes.
//!
//! This speaks the session wire directly, which the JavaScript suites
//! cannot, so it lives with the binary it shells out to.

use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use ferridriver::codegen::{Action, RecordedAction};
use ferridriver::options::LaunchOptions;
use ferridriver_session::{
  BindOptions, Command, EventPayload, INPUT_VERB, RECORDER_ASSERT_VERB, RECORDER_MODE_VERB, RECORDER_START_VERB,
  RECORDER_STOP_VERB, RUN_VERB, RecordingResult, Registry, Response, SCREENCAST_VERB, SessionClient, SessionError,
  bind_in, page_for,
};
use tokio::sync::mpsc;

// No `#` anywhere: in a data URL it starts the fragment.
const PAGE: &str = "data:text/html,<title>Sign in</title>\
<body style=margin:0>\
<label for=email style=position:absolute;left:20px;top:20px>Email</label>\
<input id=email style=position:absolute;left:120px;top:20px;width:200px>\
<label style=position:absolute;left:20px;top:70px><input type=checkbox id=remember>Remember me</label>\
<button style=position:absolute;left:20px;top:120px;width:120px;height:40px \
onclick=\"document.getElementById('greeting').textContent='Welcome, '+document.getElementById('email').value.split('@')[0]\">\
Sign in</button>\
<p id=greeting style=position:absolute;left:20px;top:200px></p></body>";

const TOKEN: &str = "sashoush-recorder-token";

async fn call(endpoint: &str, verb: &str, args: serde_json::Value) -> anyhow::Result<Response> {
  let mut client = SessionClient::connect(endpoint).await?;
  Ok(client.call(Command::new(1, verb, args)).await?)
}

async fn ok(endpoint: &str, verb: &str, args: serde_json::Value) -> anyhow::Result<Response> {
  let reply = call(endpoint, verb, args.clone()).await?;
  ensure!(reply.ok, "{verb} {args}: {:?}", reply.error);
  Ok(reply)
}

async fn click(endpoint: &str, (x, y): (f64, f64)) -> anyhow::Result<()> {
  for kind in ["mouse_move", "mouse_down", "mouse_up"] {
    ok(endpoint, INPUT_VERB, serde_json::json!({"type": kind, "x": x, "y": y})).await?;
  }
  Ok(())
}

async fn center(page: &ferridriver::Page, selector: &str) -> anyhow::Result<(f64, f64)> {
  let raw = page
    .inner()
    .evaluate(&format!(
      "(() => {{ const r = document.querySelector('{selector}').getBoundingClientRect(); \
       return JSON.stringify([r.x + r.width / 2, r.y + r.height / 2]); }})()"
    ))
    .await?
    .and_then(|value| value.as_str().map(str::to_string))
    .context("no rect")?;
  Ok(serde_json::from_str(&raw)?)
}

async fn next_event(events: &mut mpsc::UnboundedReceiver<EventPayload>, what: &str) -> anyhow::Result<EventPayload> {
  tokio::time::timeout(Duration::from_secs(15), events.recv())
    .await
    .with_context(|| format!("timed out waiting for {what}"))?
    .with_context(|| format!("stream ended waiting for {what}"))
}

/// A long-running verb on a connection of its own, its events forwarded.
async fn stream(
  endpoint: &str,
  command: Command,
) -> anyhow::Result<(
  mpsc::UnboundedReceiver<EventPayload>,
  tokio::task::JoinHandle<ferridriver_session::Result<Response>>,
)> {
  let (tx, rx) = mpsc::unbounded_channel();
  let mut client = SessionClient::connect(endpoint).await?;
  let task = tokio::spawn(async move {
    client
      .call_with_events(command, |event| {
        let _ = tx.send(event.payload);
      })
      .await
  });
  Ok((rx, task))
}

/// Without the token nothing runs, and a wrong one is refused outright.
async fn check_auth(endpoint: &str) -> anyhow::Result<()> {
  let bare = endpoint.split_once('?').context("a token endpoint")?.0;
  let refused = call(bare, RUN_VERB, serde_json::json!({"code": "return 1"})).await?;
  ensure!(
    !refused.ok && refused.error.unwrap_or_default().contains("requires a token"),
    "an anonymous connection ran a command"
  );
  match SessionClient::connect(&endpoint.replace(TOKEN, "guess")).await {
    Err(SessionError::Unauthorized(_)) => Ok(()),
    Err(other) => bail!("wrong token failed differently: {other}"),
    Ok(_) => bail!("wrong token was accepted"),
  }
}

/// Drive the page through `input` alone and record it, asserting text by
/// picking an element and the URL directly.
async fn record(endpoint: &str, page: &ferridriver::Page) -> anyhow::Result<(RecordingResult, Vec<EventPayload>)> {
  let start = Command::new(
    1,
    RECORDER_START_VERB,
    serde_json::json!({"target": "test", "title": "signs in"}),
  );
  let (mut events, recording) = stream(endpoint, start).await?;
  let opening = next_event(&mut events, "the opening navigation").await?;
  let EventPayload::Recorded {
    action: RecordedAction {
      action: Action::Navigate { url },
      ..
    },
    code,
    ..
  } = &opening
  else {
    bail!("expected the opening navigation, got {opening:?}");
  };
  ensure!(url.starts_with("data:text/html,"), "{url}");
  ensure!(code.starts_with("await page.goto('data:text/html,"), "{code}");

  click(endpoint, center(page, "#email").await?).await?;
  ok(
    endpoint,
    INPUT_VERB,
    serde_json::json!({"type": "insert_text", "text": "sashoush@example.com"}),
  )
  .await?;
  click(endpoint, center(page, "#remember").await?).await?;
  click(endpoint, center(page, "button").await?).await?;
  ok(
    endpoint,
    RECORDER_MODE_VERB,
    serde_json::json!({"mode": "assertingText"}),
  )
  .await?;
  click(endpoint, center(page, "#greeting").await?).await?;
  ok(endpoint, RECORDER_ASSERT_VERB, serde_json::json!({"kind": "url"})).await?;

  let stopped = ok(endpoint, RECORDER_STOP_VERB, serde_json::json!({})).await?;
  let result: RecordingResult = serde_json::from_str(&stopped.text)?;
  let started = recording.await??;
  ensure!(started.ok, "recorder.start: {:?}", started.error);
  ensure!(
    serde_json::from_str::<RecordingResult>(&started.text)? == result,
    "start and stop disagree"
  );
  let mut streamed = Vec::new();
  while let Ok(event) = events.try_recv() {
    streamed.push(event);
  }
  Ok((result, streamed))
}

/// A frame arrives, with the viewport it depicts.
async fn first_frame(
  frames: &mut mpsc::UnboundedReceiver<EventPayload>,
  watching: tokio::task::JoinHandle<ferridriver_session::Result<Response>>,
) -> anyhow::Result<()> {
  let frame = match next_event(frames, "a screencast frame").await {
    Ok(frame) => frame,
    Err(error) => bail!("{error}: screencast answered {:?}", watching.await?),
  };
  watching.abort();
  let EventPayload::Frame {
    data,
    viewport_width,
    viewport_height,
    ..
  } = frame
  else {
    bail!("expected a frame, got {frame:?}");
  };
  ensure!(data.starts_with("/9j/"), "frames are base64 JPEG");
  ensure!(viewport_width > 0 && viewport_height > 0, "no viewport");
  Ok(())
}

fn action_kind(action: &RecordedAction) -> String {
  serde_json::to_value(action)
    .ok()
    .and_then(|value| value["type"].as_str().map(str::to_string))
    .unwrap_or_default()
}

fn ferridriver_bin() -> std::path::PathBuf {
  std::env::var_os("FERRIDRIVER_BIN").map_or_else(
    || std::path::PathBuf::from(env!("CARGO_BIN_EXE_ferridriver")),
    std::path::PathBuf::from,
  )
}

#[tokio::test(flavor = "multi_thread")]
async fn relayed_input_is_recorded_and_the_spec_passes() -> anyhow::Result<()> {
  let browser = ferridriver::chromium()
    .launch(LaunchOptions {
      headless: Some(true),
      ..Default::default()
    })
    .await?;
  let scratch = tempfile::tempdir()?;
  let registry = Registry::open_at(scratch.path().join("registry"))?;
  let options = BindOptions {
    host: Some("127.0.0.1".into()),
    port: Some(0),
    token: Some(TOKEN.into()),
    ..Default::default()
  };
  let session = bind_in(&registry, &browser, "recorder", options, None).await?;
  let endpoint = session.endpoint().to_string();
  let page = page_for(&browser, "default").await?;
  page.goto(PAGE).await?;
  check_auth(&endpoint).await?;

  let (mut frames, watching) = stream(
    &endpoint,
    Command::new(1, SCREENCAST_VERB, serde_json::json!({"quality": 50})),
  )
  .await?;
  let (result, streamed) = record(&endpoint, &page).await?;

  let kinds: Vec<String> = result.actions.iter().map(action_kind).collect();
  // A click on a text field is recorded before what was typed into it.
  assert_eq!(
    kinds,
    ["navigate", "click", "fill", "check", "click", "assertText", "assertUrl"],
    "{:#?}",
    result.actions
  );
  assert_eq!(result.actions[2].action.typed_value(), Some("sashoush@example.com"));
  assert!(matches!(
    &result.actions[5].action,
    Action::AssertText { text, substring: true, .. } if text == "Welcome, sashoush"
  ));
  assert!(
    result
      .actions
      .iter()
      .all(|action| action.page == "page" && action.popup.is_none())
  );
  let recorded: Vec<&RecordedAction> = streamed
    .iter()
    .filter_map(|event| match event {
      EventPayload::Recorded { action, .. } => Some(action),
      _ => None,
    })
    .collect();
  assert_eq!(recorded, result.actions[1..].iter().collect::<Vec<_>>());
  let modes: Vec<&str> = streamed
    .iter()
    .filter_map(|event| match event {
      EventPayload::RecorderMode { mode } => Some(mode.as_str()),
      _ => None,
    })
    .collect();
  // The mode it started in, once attached, then each change.
  assert_eq!(modes, ["recording", "assertingText", "recording"]);

  first_frame(&mut frames, watching).await?;

  // Locators come from the generator: role and accessible name, not CSS.
  let lines: Vec<&str> = result.source.lines().collect();
  assert_eq!(lines[0], "import { test, expect } from '@ferridriver/test';");
  assert_eq!(lines[2], "test('signs in', async ({ page }) => {");
  assert!(lines[3].starts_with("  await page.goto('data:text/html,"));
  assert_eq!(
    lines[4..9],
    [
      "  await page.getByRole('textbox', { name: 'Email' }).click();",
      "  await page.getByRole('textbox', { name: 'Email' }).fill('sashoush@example.com');",
      "  await page.getByRole('checkbox', { name: 'Remember me' }).check();",
      "  await page.getByRole('button', { name: 'Sign in' }).click();",
      "  await expect(page.locator('#greeting')).toContainText('Welcome, sashoush');",
    ],
    "{}",
    result.source
  );
  assert!(lines[9].starts_with("  await expect(page).toHaveURL('data:text/html,"));
  assert_eq!(lines[10..], ["});"]);

  // The spec replays the recording against a fresh browser.
  std::fs::write(scratch.path().join("recorded.spec.ts"), &result.source)?;
  let run = std::process::Command::new(ferridriver_bin())
    .args(["test", "--no-inherit", "--headless", "recorded.spec.ts"])
    .current_dir(scratch.path())
    .output()?;
  assert!(
    run.status.success(),
    "the recorded spec failed:\n{}\n{}\n{}",
    result.source,
    String::from_utf8_lossy(&run.stdout),
    String::from_utf8_lossy(&run.stderr)
  );

  drop(session);
  browser.close().await?;
  Ok(())
}

// The opener has a link that opens a popup, and both pages keep the keys
// they were sent.
const OPENER: &str = "data:text/html,<body style=margin:0>\
<a href=about:blank target=_blank style=position:absolute;left:10px;top:10px;width:120px;height:30px>open</a>\
<script>window.keys = []; addEventListener('keydown', e => keys.push(e.key));</script></body>";

async fn keys(page: &ferridriver::Page) -> anyhow::Result<String> {
  let value = page.inner().evaluate("window.keys.join('')").await?;
  Ok(value.and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default())
}

async fn press(endpoint: &str, key: &str, target: Option<&str>) -> anyhow::Result<Response> {
  let mut reply = None;
  for kind in ["key_down", "key_up"] {
    let mut args = serde_json::json!({"type": kind, "key": key});
    if let Some(target) = target {
      args["page"] = target.into();
    }
    reply = Some(call(endpoint, INPUT_VERB, args).await?);
  }
  reply.context("no reply")
}

/// The id a viewer knows the page by: the CDP target id on Chromium, read
/// from the protocol itself rather than from ferridriver.
async fn target_id(page: &std::sync::Arc<ferridriver::Page>, cdp: bool) -> anyhow::Result<String> {
  if !cdp {
    return Ok(page.main_frame().id().to_string());
  }
  let context = page.context().context("a page with a context")?;
  let session = context.new_cdp_session(page).await?;
  let info = session.send("Target.getTargetInfo", serde_json::json!({})).await?;
  info["targetInfo"]["targetId"]
    .as_str()
    .map(str::to_string)
    .context("no target id")
}

async fn input_reaches_the_page_it_names(browser: ferridriver::Browser, cdp: bool) -> anyhow::Result<()> {
  let scratch = tempfile::tempdir()?;
  let registry = Registry::open_at(scratch.path().join("registry"))?;
  let options = BindOptions {
    host: Some("127.0.0.1".into()),
    port: Some(0),
    ..Default::default()
  };
  let session = bind_in(&registry, &browser, "targets", options, None).await?;
  let endpoint = session.endpoint().to_string();
  let opener = page_for(&browser, "default").await?;
  opener.goto(OPENER).await?;
  let opener_id = target_id(&opener, cdp).await?;

  // A click with no page goes to the active page, and the popup it opens
  // becomes the active page.
  let (popup, ()) = opener
    .expect_popup(|| async {
      click(&endpoint, (40.0, 20.0))
        .await
        .map_err(|error| ferridriver::FerriError::backend(error.to_string()))
    })
    .await?;
  popup
    .set_content("<script>window.keys = []; addEventListener('keydown', e => keys.push(e.key));</script>")
    .await?;
  let active = page_for(&browser, "default").await?;
  ensure!(
    active.main_frame().id() == popup.main_frame().id(),
    "the active page did not follow the popup"
  );
  let reply = press(&endpoint, "a", None).await?;
  ensure!(reply.ok, "{:?}", reply.error);

  // Named, the event goes to that page whichever is active.
  let reply = press(&endpoint, "b", Some(&opener_id)).await?;
  ensure!(reply.ok, "{:?}", reply.error);
  let popup_id = target_id(&popup, cdp).await?;
  let reply = press(&endpoint, "c", Some(&popup_id)).await?;
  ensure!(reply.ok, "{:?}", reply.error);
  assert_eq!(keys(&opener).await?, "b");
  assert_eq!(keys(&popup).await?, "ac");

  let missing = press(&endpoint, "d", Some("no-such-target")).await?;
  ensure!(!missing.ok, "an unknown page took the input");
  ensure!(
    missing.error.unwrap_or_default().contains("no-such-target"),
    "the error does not name the page"
  );

  drop(session);
  browser.close().await?;
  Ok(())
}

async fn launch(browser: ferridriver::BrowserType) -> anyhow::Result<ferridriver::Browser> {
  Ok(
    browser
      .launch(LaunchOptions {
        headless: Some(true),
        ..Default::default()
      })
      .await?,
  )
}

#[tokio::test(flavor = "multi_thread")]
async fn input_reaches_the_page_it_names_on_cdp_pipe() -> anyhow::Result<()> {
  input_reaches_the_page_it_names(launch(ferridriver::chromium()).await?, true).await
}

#[tokio::test(flavor = "multi_thread")]
async fn input_reaches_the_page_it_names_on_cdp_raw() -> anyhow::Result<()> {
  let raw = ferridriver::BrowserType::chromium_with(&ferridriver::options::BrowserTypeOptions {
    transport: Some(ferridriver::options::ChromiumTransport::Ws),
  });
  input_reaches_the_page_it_names(launch(raw).await?, true).await
}

#[tokio::test(flavor = "multi_thread")]
async fn input_reaches_the_page_it_names_on_webkit() -> anyhow::Result<()> {
  input_reaches_the_page_it_names(launch(ferridriver::webkit()).await?, false).await
}

#[tokio::test(flavor = "multi_thread")]
async fn input_reaches_the_page_it_names_on_bidi() -> anyhow::Result<()> {
  input_reaches_the_page_it_names(launch(ferridriver::firefox()).await?, false).await
}

/// The list a watcher keeps from the recorded events: each one appended,
/// or put in the place of the entry it replaces.
fn watched_steps(events: &[EventPayload]) -> Vec<RecordedAction> {
  let mut steps: Vec<RecordedAction> = Vec::new();
  for event in events {
    if let EventPayload::Recorded {
      action,
      replaces_previous,
      replaces_index,
      ..
    } = event
    {
      match (replaces_index, replaces_previous) {
        (Some(index), _) if *index < steps.len() => steps[*index] = action.clone(),
        (_, true) => {
          steps.pop();
          steps.push(action.clone());
        },
        _ => steps.push(action.clone()),
      }
    }
  }
  steps
}

async fn bound(
  browser: &ferridriver::Browser,
  name: &str,
) -> anyhow::Result<(ferridriver_session::BoundSession, String, tempfile::TempDir)> {
  let scratch = tempfile::tempdir()?;
  let registry = Registry::open_at(scratch.path().join("registry"))?;
  let options = BindOptions {
    host: Some("127.0.0.1".into()),
    port: Some(0),
    ..Default::default()
  };
  let session = bind_in(&registry, browser, name, options, None).await?;
  let endpoint = session.endpoint().to_string();
  Ok((session, endpoint, scratch))
}

// The opener has a link that opens a popup at once and a button that
// opens another a while after it is pressed.
const LATE_OPENER: &str = "<!doctype html><body style=margin:0>\
<a href=/here target=_blank style=position:absolute;left:10px;top:10px;width:120px;height:30px>open</a>\
<button style=position:absolute;left:10px;top:60px;width:120px;height:30px \
onclick=\"setTimeout(() => window.open('/here'), 1500)\">Later</button></body>";

const LATE_POPUP: &str = "<!doctype html><body style=margin:0>\
<button style=position:absolute;left:10px;top:10px;width:120px;height:30px>Here</button></body>";

/// Serve the opener and its popup over HTTP, a thread per connection: a
/// page may not open a data URL in a window of its own.
fn serve_late() -> anyhow::Result<String> {
  use std::io::{Read as _, Write as _};
  let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
  let origin = format!("http://{}", listener.local_addr()?);
  std::thread::spawn(move || {
    for stream in listener.incoming().flatten() {
      std::thread::spawn(move || {
        let mut stream = stream;
        let mut request = [0_u8; 4096];
        let read = stream.read(&mut request).unwrap_or(0);
        let head = String::from_utf8_lossy(&request[..read]);
        let body = if head.split_whitespace().nth(1) == Some("/here") {
          LATE_POPUP
        } else {
          LATE_OPENER
        };
        let _ = write!(
          stream,
          "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
          body.len()
        );
      });
    }
  });
  Ok(origin)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_popup_opened_after_another_step_replaces_the_step_that_opened_it() -> anyhow::Result<()> {
  let browser = launch(ferridriver::chromium()).await?;
  let (session, endpoint, _scratch) = bound(&browser, "late").await?;
  let opener = page_for(&browser, "default").await?;
  opener.goto(&format!("{}/", serve_late()?)).await?;
  let opener_id = target_id(&opener, true).await?;
  let start = Command::new(1, RECORDER_START_VERB, serde_json::json!({"target": "test"}));
  let (mut events, recording) = stream(&endpoint, start).await?;
  let mut streamed = vec![next_event(&mut events, "the opening navigation").await?];

  // The link's popup is announced at once, on the click that opened it.
  let (popup, ()) = opener
    .expect_popup(|| async {
      click(&endpoint, (40.0, 20.0))
        .await
        .map_err(|error| ferridriver::FerriError::backend(error.to_string()))
    })
    .await?;
  popup.wait_for_load_state(None).await?;

  // The opener's button opens its popup only after a step on the first
  // popup has been recorded.
  let later = opener.wait_for_event("popup", Some(15_000));
  for kind in ["mouse_move", "mouse_down", "mouse_up"] {
    ok(
      &endpoint,
      INPUT_VERB,
      serde_json::json!({"type": kind, "x": 40.0, "y": 75.0, "page": opener_id}),
    )
    .await?;
  }
  click(&endpoint, (40.0, 20.0)).await?;
  later.await?;

  let late = loop {
    let event = match next_event(&mut events, "the late popup's replacement").await {
      Ok(event) => event,
      Err(error) => bail!("{error}: streamed {streamed:#?}"),
    };
    streamed.push(event.clone());
    if let EventPayload::Recorded {
      replaces_index: Some(index),
      replaces_previous: false,
      action,
      ..
    } = event
    {
      break (index, action);
    }
  };
  let stopped = ok(&endpoint, RECORDER_STOP_VERB, serde_json::json!({})).await?;
  let result: RecordingResult = serde_json::from_str(&stopped.text)?;
  ensure!(recording.await??.ok, "recorder.start failed");
  while let Ok(event) = events.try_recv() {
    streamed.push(event);
  }

  let kinds: Vec<(String, Option<&str>)> = result
    .actions
    .iter()
    .map(|action| {
      (
        format!("{}:{}", action.page, action_kind(action)),
        action.popup.as_deref(),
      )
    })
    .collect();
  assert_eq!(
    kinds,
    [
      ("page:navigate".to_string(), None),
      ("page:click".to_string(), Some("page1")),
      ("page:click".to_string(), Some("page2")),
      ("page1:click".to_string(), None),
    ],
    "{:#?}",
    result.actions
  );
  assert_eq!(late.0, 2, "the late popup replaces the step that opened it");
  assert_eq!(late.1, result.actions[2]);
  assert_eq!(
    watched_steps(&streamed),
    result.actions,
    "the watcher's list is the recording"
  );

  drop(session);
  browser.close().await?;
  Ok(())
}
const LOGIN: &str = "data:text/html,<title>Sign in</title><body style=margin:0>\
<label style=position:absolute;left:10px;top:10px>Password <input type=password id=pw style=width:150px></label>\
<input id=code autocomplete=one-time-code style=position:absolute;left:10px;top:50px;width:150px>\
<label style=position:absolute;left:10px;top:90px>PIN <input id=pin style=width:150px></label>\
<label style=position:absolute;left:10px;top:130px>Confirm PIN <input id=again style=width:150px></label>\
<button style=position:absolute;left:10px;top:170px;width:100px;height:30px>Go</button></body>";

async fn type_keys(endpoint: &str, text: &str) -> anyhow::Result<()> {
  for key in text.chars() {
    let reply = press(endpoint, &key.to_string(), None).await?;
    ensure!(reply.ok, "{:?}", reply.error);
  }
  Ok(())
}

/// Run `source` as a spec with each secret's value in the environment the
/// code reads it from, and only there.
fn replay_from_the_environment(
  dir: &std::path::Path,
  source: &str,
  secrets: &[(String, String)],
) -> anyhow::Result<()> {
  std::fs::write(dir.join("recorded.spec.ts"), source)?;
  let names: Vec<String> = secrets.iter().map(|(name, _)| format!("{name:?}")).collect();
  std::fs::write(
    dir.join("ferridriver.toml"),
    format!("[scripting]\nallowEnv = [{}]\n", names.join(", ")),
  )?;
  let run = std::process::Command::new(ferridriver_bin())
    .args(["test", "--no-inherit", "--headless", "recorded.spec.ts"])
    .current_dir(dir)
    .envs(secrets.iter().map(|(name, value)| (name, value)))
    .output()?;
  ensure!(
    run.status.success(),
    "the recorded spec failed:\n{source}\n{}\n{}",
    String::from_utf8_lossy(&run.stdout),
    String::from_utf8_lossy(&run.stderr)
  );
  Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_typed_secret_reaches_only_the_host_and_the_code_reads_it_from_the_environment() -> anyhow::Result<()> {
  let browser = launch(ferridriver::chromium()).await?;
  let (session, endpoint, scratch) = bound(&browser, "secrets").await?;
  let page = page_for(&browser, "default").await?;
  page.goto(LOGIN).await?;
  let start = Command::new(
    1,
    RECORDER_START_VERB,
    serde_json::json!({"target": "test", "secrets": "env"}),
  );
  let (mut events, recording) = stream(&endpoint, start).await?;
  let mut streamed = vec![next_event(&mut events, "the opening navigation").await?];

  // A password field and a one-time code field are found by their kind;
  // the PIN fields are plain, so the host names the first one.
  click(&endpoint, center(&page, "#pw").await?).await?;
  type_keys(&endpoint, "hunter2").await?;
  click(&endpoint, center(&page, "#code").await?).await?;
  type_keys(&endpoint, "123456").await?;
  click(&endpoint, center(&page, "#pin").await?).await?;
  type_keys(&endpoint, "4321").await?;
  click(&endpoint, center(&page, "#again").await?).await?;
  type_keys(&endpoint, "4321").await?;
  click(&endpoint, center(&page, "button").await?).await?;
  let named = ok(
    &endpoint,
    ferridriver_session::RECORDER_SECRET_VERB,
    serde_json::json!({"step": 6, "name": "PIN"}),
  )
  .await?;
  let named: serde_json::Value = serde_json::from_str(&named.text)?;
  assert_eq!(named, serde_json::json!({"name": "PIN", "steps": [6, 8]}));

  let stopped = ok(&endpoint, RECORDER_STOP_VERB, serde_json::json!({})).await?;
  let result: RecordingResult = serde_json::from_str(&stopped.text)?;
  ensure!(recording.await??.ok, "recorder.start failed");
  while let Ok(event) = events.try_recv() {
    streamed.push(event);
  }

  // Each value reaches the host once, settled, in a message of its own.
  let secrets: Vec<(String, String)> = streamed
    .iter()
    .filter_map(|event| match event {
      EventPayload::Secret { name, value } => Some((name.clone(), value.clone())),
      _ => None,
    })
    .collect();
  let expected = [
    ("PASSWORD".to_string(), "hunter2".to_string()),
    ("CODE".to_string(), "123456".to_string()),
    ("PIN".to_string(), "4321".to_string()),
  ];
  assert_eq!(secrets, expected);
  let named_secrets: Vec<(String, String)> = result
    .secrets
    .iter()
    .map(|secret| (secret.name.clone(), secret.value.clone()))
    .collect();
  assert_eq!(named_secrets, expected);

  // The password and code were never anywhere else; the PIN only before
  // the host named it.
  for event in streamed
    .iter()
    .filter(|event| !matches!(event, EventPayload::Secret { .. }))
  {
    let wire = serde_json::to_string(&ferridriver_session::ServerFrame::Event(ferridriver_session::Event {
      id: 1,
      payload: event.clone(),
    }))?;
    ensure!(!wire.contains("hunter2") && !wire.contains("123456"), "{wire}");
  }
  let kept = serde_json::to_string(&result.actions)? + &result.source;
  for literal in ["hunter2", "123456", "4321"] {
    ensure!(!kept.contains(literal), "{literal} in the result:\n{kept}");
  }
  assert_eq!(
    watched_steps(&streamed),
    result.actions,
    "the watcher's list is the recording"
  );
  for (index, name) in [(2, "PASSWORD"), (4, "CODE"), (6, "PIN"), (8, "PIN")] {
    assert_eq!(
      result.actions[index].action.secret(),
      Some(name),
      "{:#?}",
      result.actions
    );
    assert_eq!(
      serde_json::to_value(&result.actions[index])?["value"],
      serde_json::Value::Null
    );
  }
  let fills: Vec<&str> = result.source.lines().filter(|line| line.contains(".fill(")).collect();
  assert_eq!(
    fills,
    [
      "  await page.getByRole('textbox', { name: 'Password' }).fill(process.env.PASSWORD);",
      "  await page.locator('#code').fill(process.env.CODE);",
      "  await page.getByRole('textbox', { name: 'PIN', exact: true }).fill(process.env.PIN);",
      "  await page.getByRole('textbox', { name: 'Confirm PIN' }).fill(process.env.PIN);",
    ],
    "{}",
    result.source
  );

  replay_from_the_environment(scratch.path(), &result.source, &expected)?;

  drop(session);
  browser.close().await?;
  Ok(())
}

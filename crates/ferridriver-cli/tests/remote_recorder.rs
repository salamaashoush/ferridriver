//! Watching, driving and recording a headless browser over a session
//! endpoint, the way a web app relays a remote browser: frames out, input
//! in, recorded actions back, and a spec at the end that `ferridriver test`
//! passes.
//!
//! This speaks the session wire directly, which the JavaScript suites
//! cannot, so it lives with the binary it shells out to.

use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use ferridriver::codegen::Action;
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
    action: Action::Navigate { url },
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

fn action_kind(action: &Action) -> String {
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
  assert_eq!(
    kinds,
    ["navigate", "fill", "check", "click", "assertText", "assertUrl"],
    "{:#?}",
    result.actions
  );
  assert!(matches!(&result.actions[1], Action::Fill { value, .. } if value == "sashoush@example.com"));
  assert!(matches!(
    &result.actions[4],
    Action::AssertText { text, substring: true, .. } if text == "Welcome, sashoush"
  ));
  let recorded: Vec<&Action> = streamed
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
  assert_eq!(modes, ["assertingText", "recording"]);

  first_frame(&mut frames, watching).await?;

  // Locators come from the generator: role and accessible name, not CSS.
  let lines: Vec<&str> = result.source.lines().collect();
  assert_eq!(lines[0], "import { test, expect } from '@ferridriver/test';");
  assert_eq!(lines[2], "test('signs in', async ({ page }) => {");
  assert!(lines[3].starts_with("  await page.goto('data:text/html,"));
  assert_eq!(
    lines[4..8],
    [
      "  await page.getByRole('textbox', { name: 'Email' }).fill('sashoush@example.com');",
      "  await page.getByRole('checkbox', { name: 'Remember me' }).check();",
      "  await page.getByRole('button', { name: 'Sign in' }).click();",
      "  await expect(page.locator('#greeting')).toContainText('Welcome, sashoush');",
    ],
    "{}",
    result.source
  );
  assert!(lines[8].starts_with("  await expect(page).toHaveURL('data:text/html,"));
  assert_eq!(lines[9..], ["});"]);

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

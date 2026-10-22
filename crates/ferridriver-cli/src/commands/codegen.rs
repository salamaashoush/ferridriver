//! `ferridriver codegen` — record interactions and emit them as a test.

use std::io::Write as _;
use std::sync::Arc;

use anyhow::Context as _;
use ferridriver::codegen::emitter::{CodeEmitter, TypeScriptTestEmitter};
use ferridriver::codegen::recorder::{HeadedRecorder, RecorderOptions};
use ferridriver::codegen::{OutputLanguage, Scaffold};
use ferridriver_session::{
  Command, EventPayload, PageAssertion, RECORDER_ASSERT_VERB, RECORDER_MODE_VERB, RECORDER_START_VERB,
  RECORDER_STOP_VERB, RecorderAssertRequest, RecorderModeRequest, RecorderStartRequest, RecordingResult, Registry,
  SessionClient,
};

use crate::cli;
use crate::ui;

/// Launch the interactive recorder: open a headed browser, capture the user's
/// interactions, and emit a runnable script (TypeScript by default) to stdout
/// or `--output`. The emitted script runs standalone via `ferridriver run`
/// and replays on a live session via the MCP `run_script` tool.
pub async fn run(args: cli::CodegenArgs) -> anyhow::Result<()> {
  let scaffold = Scaffold::parse(&args.target).map_err(anyhow::Error::msg)?;
  if let Some(session) = args.attach.clone() {
    return attach(&args, &session, scaffold).await;
  }
  let url = args.url.clone().unwrap_or_else(|| "about:blank".to_string());
  let options = RecorderOptions {
    url: url.clone(),
    language: OutputLanguage::parse_cli(&args.language),
    scaffold,
    output_file: args.output.as_deref().map(|p| p.to_string_lossy().into_owned()),
    viewport: None,
  };

  if args.pick_locator {
    ui::note(&format!("picking from {}", ui::url(&url)));
    ui::note("click the element you want a selector for");
    let selector = HeadedRecorder::new(options)
      .pick()
      .await
      .map_err(|e| anyhow::anyhow!("codegen: {e}"))?;
    // The selector alone on stdout, so it pipes; everything a reader
    // needs to see is on stderr already.
    println!("{selector}");
    return Ok(());
  }

  ui::note(&format!("recording from {}", ui::url(&url)));
  ui::note("interact with the page, then close the browser to finish");
  HeadedRecorder::new(options)
    .start()
    .await
    .map_err(|e| anyhow::anyhow!("codegen: {e}"))?;
  if let Some(path) = &args.output {
    wrote(path, &args.target);
  }
  Ok(())
}

fn wrote(path: &std::path::Path, target: &str) {
  ui::say(&ui::success(&format!(
    "wrote {}",
    ui::path(&path.display().to_string())
  )));
  let command = if target == "test" { "test" } else { "run" };
  ui::next_steps(&[("run it", format!("ferridriver {command} {}", path.display()))]);
}

fn emitter(language: OutputLanguage, scaffold: Scaffold, title: Option<&str>) -> Arc<dyn CodeEmitter> {
  match (language, scaffold, title) {
    (OutputLanguage::TypeScript, Scaffold::Test, Some(title)) => Arc::new(TypeScriptTestEmitter::new(title)),
    _ => language.emitter_for(scaffold),
  }
}

/// Send one control command to the session on a connection of its own:
/// the recording holds the first one until it ends.
async fn control(
  session: &str,
  context: Option<&str>,
  verb: &str,
  args: serde_json::Value,
) -> anyhow::Result<ferridriver_session::Response> {
  let registry = Registry::open()?;
  let mut client = SessionClient::attach(&registry, session)
    .await
    .with_context(|| format!("attaching to session '{session}'"))?;
  let reply = client
    .call(Command::new(1, verb, args).with_context(context.map(str::to_string)))
    .await?;
  if !reply.ok {
    anyhow::bail!("{verb}: {}", reply.error.unwrap_or_default());
  }
  Ok(reply)
}

/// One line typed while recording: an assertion mode, a page assertion, or
/// the end of the recording.
async fn typed(session: &str, context: Option<&str>, line: &str) -> anyhow::Result<bool> {
  let mode = |mode: &str| serde_json::to_value(RecorderModeRequest { mode: mode.to_string() });
  let assert = |kind| serde_json::to_value(RecorderAssertRequest { kind });
  let (verb, args) = match line {
    "" => return Ok(false),
    "stop" => return Ok(true),
    "visible" => (RECORDER_MODE_VERB, mode("assertingVisibility")?),
    "text" => (RECORDER_MODE_VERB, mode("assertingText")?),
    "value" => (RECORDER_MODE_VERB, mode("assertingValue")?),
    "record" => (RECORDER_MODE_VERB, mode("recording")?),
    "pause" => (RECORDER_MODE_VERB, mode("none")?),
    "url" => (RECORDER_ASSERT_VERB, assert(PageAssertion::Url)?),
    "title" => (RECORDER_ASSERT_VERB, assert(PageAssertion::Title)?),
    other => {
      ui::say(&ui::warning(&format!(
        "unknown command '{other}': try visible, text, value, url, title, record, pause or stop"
      )));
      return Ok(false);
    },
  };
  if let Err(error) = control(session, context, verb, args).await {
    ui::say(&ui::warning(&format!("{error:#}")));
  }
  Ok(false)
}

/// Resolve when the recording should end: a typed `stop`, Ctrl+C or
/// SIGTERM. Other typed lines are carried out on the way. A closed stdin
/// only ends the typing, not the recording.
async fn until_stopped(session: &str, context: Option<&str>) {
  use tokio::io::AsyncBufReadExt as _;
  let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
  let mut typing = true;
  let signal = shutdown_signal();
  tokio::pin!(signal);
  loop {
    tokio::select! {
      () = &mut signal => return,
      line = lines.next_line(), if typing => match line {
        Ok(Some(line)) => {
          if matches!(typed(session, context, line.trim()).await, Ok(true)) {
            return;
          }
        },
        Ok(None) | Err(_) => typing = false,
      },
    }
  }
}

async fn shutdown_signal() {
  #[cfg(unix)]
  {
    use tokio::signal::unix::{SignalKind, signal};
    let Ok(mut term) = signal(SignalKind::terminate()) else {
      return std::future::pending().await;
    };
    tokio::select! {
      _ = term.recv() => {},
      _ = tokio::signal::ctrl_c() => {},
    }
  }
  #[cfg(not(unix))]
  {
    let _ = tokio::signal::ctrl_c().await;
  }
}

/// `codegen --attach`: record on a live session's page, printing each
/// action as it happens and writing the whole file when the recording ends.
async fn attach(args: &cli::CodegenArgs, session: &str, scaffold: Scaffold) -> anyhow::Result<()> {
  let language = OutputLanguage::parse_cli(&args.language);
  let context = args.context.as_deref();
  let live = emitter(language, scaffold, args.title.as_deref());
  let to_stdout = args.output.is_none();

  let registry = Registry::open()?;
  let mut client = SessionClient::attach(&registry, session)
    .await
    .with_context(|| format!("attaching to session '{session}'"))?;
  let request = RecorderStartRequest {
    language: Some(args.language.clone()),
    target: Some(args.target.clone()),
    title: args.title.clone(),
    url: args.url.clone(),
    mode: None,
  };
  let start =
    Command::new(1, RECORDER_START_VERB, serde_json::to_value(&request)?).with_context(context.map(str::to_string));

  ui::note(&format!("recording on session {}", ui::bold(session)));
  ui::note("type visible, text or value then click an element to assert on it; url or title asserts the page");
  ui::note("type stop or press Ctrl+C to finish");
  if to_stdout {
    print!("{}", live.header(""));
    let _ = std::io::stdout().flush();
  }

  let recording = client.call_with_events(start, |event| match event.payload {
    EventPayload::Recorded { action, .. } => {
      if to_stdout {
        print!("{}", live.action(&action));
        let _ = std::io::stdout().flush();
      } else {
        ui::note(&format!("recorded {}", action_name(&action)));
      }
    },
    EventPayload::RecorderMode { mode } => ui::note(&format!("mode: {mode}")),
    _ => {},
  });
  tokio::pin!(recording);
  let reply = tokio::select! {
    reply = &mut recording => reply,
    () = until_stopped(session, context) => {
      control(session, context, RECORDER_STOP_VERB, serde_json::json!({})).await?;
      recording.await
    },
  }
  .with_context(|| format!("recording on session '{session}'"))?;
  if !reply.ok {
    anyhow::bail!("recording on session '{session}': {}", reply.error.unwrap_or_default());
  }
  let result: RecordingResult = serde_json::from_str(&reply.text).context("reading the recording")?;

  if to_stdout {
    print!("{}", live.footer());
    let _ = std::io::stdout().flush();
  }
  if let Some(path) = &args.output {
    std::fs::write(path, &result.source).with_context(|| format!("writing {}", path.display()))?;
    ui::note(&format!("{} action(s) recorded", result.actions.len()));
    wrote(path, &args.target);
  }
  Ok(())
}

fn action_name(action: &ferridriver::codegen::Action) -> String {
  serde_json::to_value(action)
    .ok()
    .and_then(|value| {
      value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
    })
    .unwrap_or_default()
}

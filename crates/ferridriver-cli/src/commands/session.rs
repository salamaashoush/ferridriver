//! `ferridriver session` subcommand: open / host / attach / list / close /
//! close-all, plus the client half of `ferridriver run --session`.
//!
//! These drive ferridriver's named-session layer (`ferridriver-session`) from
//! the terminal — the token-efficient counterpart to the MCP server for
//! coding agents. `open` launches a browser and binds it under an id in a
//! detached host process; that host serves one thing, a script run, so
//! everything a client wants to do goes through
//! [`ferridriver_session::ScriptRequest`] rather than a verb table that would
//! forever lag behind the scripting API.

use anyhow::Context as _;
use ferridriver::backend::BackendKind;
use ferridriver::browser_type::BrowserType;
use ferridriver::options::{BrowserKind, LaunchOptions};
use ferridriver_config::FerridriverConfig;
use ferridriver_session::{BindOptions, Command, RUN_VERB, Registry, ScriptRequest, SessionClient, bind_in};

use crate::cli::{
  BrowserArgs, SessionArgs, SessionCommand, SessionHostArgs, SessionListArgs, SessionOpenArgs, SessionTargetArgs,
};
use crate::ui;

/// The script `session attach` runs to render a session's current state. It is
/// an ordinary script for the same reason everything else is: there is one
/// path into a session, and `attach` must not be a privileged exception to it.
const ATTACH_SNAPSHOT: &str = "return await page.snapshotForAI();";

/// How this invocation was configured, so `open` can hand the same stack to
/// the host it spawns.
///
/// The host discovers the layered config from its working directory on its
/// own, but an explicit `-c/--config` (and `--no-inherit`) exists only as
/// arguments to THIS process — without forwarding them, a session opened with
/// `-c` runs with a different configuration than the command that opened it,
/// silently.
#[derive(Clone, Copy)]
pub struct ConfigOrigin<'a> {
  pub explicit: Option<&'a std::path::Path>,
  pub inherit: bool,
}

pub async fn run(config: FerridriverConfig, origin: ConfigOrigin<'_>, args: SessionArgs) -> anyhow::Result<()> {
  match args.command {
    SessionCommand::Open(a) => open(a, origin).await,
    // Boxed: hosting carries the whole resolved scripting environment, which
    // would otherwise make this match arm's future the size of the enum.
    SessionCommand::Host(a) => Box::pin(host(config, a)).await,
    SessionCommand::Attach(a) => attach(a).await,
    SessionCommand::List(a) => list(&a),
    SessionCommand::Close(a) => close(&a),
    SessionCommand::CloseAll => close_all(),
  }
}

fn browser_kind_for(backend: BackendKind) -> BrowserKind {
  match backend {
    BackendKind::Bidi => BrowserKind::Firefox,
    BackendKind::WebKit => BrowserKind::WebKit,
    _ => BrowserKind::Chromium,
  }
}

/// Launch a browser for the given CLI browser args.
async fn launch_browser(browser: &BrowserArgs) -> anyhow::Result<ferridriver::Browser> {
  let backend = browser.backend_kind().unwrap_or(BackendKind::CdpPipe);
  let kind = browser_kind_for(backend);
  let factory = BrowserType::with_backend(kind, backend);
  let options = LaunchOptions {
    headless: Some(browser.headless),
    executable_path: browser.executable_path.clone(),
    ..Default::default()
  };
  factory
    .launch(options)
    .await
    .with_context(|| format!("launching {} browser", kind.name()))
}

/// `open`: spawn a detached `session host` process and wait until its
/// descriptor appears in the registry, then print the endpoint.
async fn open(args: SessionOpenArgs, origin: ConfigOrigin<'_>) -> anyhow::Result<()> {
  let registry = Registry::open()?;
  // If a session with this id is already live, refuse rather than clobber.
  if registry.get(&args.id)?.is_some() {
    anyhow::bail!(
      "session '{}' already exists. Close it first with `ferridriver session close {}`.",
      args.id,
      args.id
    );
  }

  let exe = std::env::current_exe().context("resolving the ferridriver executable")?;
  let mut cmd = std::process::Command::new(exe);
  // The config flags belong to the commands that read configuration, so they
  // go after the subcommand names rather than before them.
  cmd.arg("session").arg("host").arg(&args.id);
  if let Some(path) = origin.explicit {
    cmd.arg("--config").arg(path);
  }
  if !origin.inherit {
    cmd.arg("--no-inherit");
  }
  if let Some(url) = &args.url {
    cmd.arg(url);
  }
  cmd.arg("--backend").arg(backend_name(&args.browser));
  if args.browser.headless {
    cmd.arg("--headless");
  }
  if let Some(path) = &args.browser.executable_path {
    cmd.arg("--executable-path").arg(path);
  }
  for extension in &args.extensions {
    cmd.arg("--extension").arg(extension);
  }
  // The host resolves relative extension specs and the `fs` sandbox root
  // against ITS working directory, so it must start in the one the user
  // typed the command in.
  if let Ok(cwd) = std::env::current_dir() {
    cmd.current_dir(cwd);
  }
  // Detach: the host owns the browser and outlives this invocation.
  cmd.stdin(std::process::Stdio::null());
  cmd.stdout(std::process::Stdio::null());
  cmd.stderr(std::process::Stdio::null());
  let child = cmd.spawn().context("spawning session host process")?;

  // Wait for the host to publish its descriptor (bounded — the browser
  // launch dominates this).
  let descriptor = wait_for_descriptor(&registry, &args.id, std::time::Duration::from_mins(1)).await?;
  ui::say(&ui::success(&format!(
    "session {} open {}",
    ui::bold(&args.id),
    ui::dim(&format!("(pid {}) {}", child.id(), descriptor.endpoint))
  )));
  ui::next_steps(&[
    (
      "drive it",
      format!("ferridriver run -e \"await page.goto('…')\" --session {}", args.id),
    ),
    ("close it", format!("ferridriver session close {}", args.id)),
  ]);
  Ok(())
}

/// Poll the registry until `id` appears or the deadline elapses.
async fn wait_for_descriptor(
  registry: &Registry,
  id: &str,
  timeout: std::time::Duration,
) -> anyhow::Result<ferridriver_session::SessionDescriptor> {
  let deadline = std::time::Instant::now() + timeout;
  loop {
    if let Some(d) = registry.get(id)? {
      return Ok(d);
    }
    if std::time::Instant::now() >= deadline {
      anyhow::bail!("session '{id}' did not come up within {timeout:?}");
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
  }
}

/// `host`: the long-lived foreground process. Launch, bind, navigate, serve
/// until killed. `open` spawns this detached.
async fn host(config: FerridriverConfig, args: SessionHostArgs) -> anyhow::Result<()> {
  let browser = launch_browser(&args.browser).await?;
  // Open the first page (and navigate it if a url was given) so an attaching
  // client sees a ready page immediately.
  let page = browser.new_page().await.context("opening the session's first page")?;
  if let Some(url) = &args.url {
    page.goto(url).await.with_context(|| format!("navigating to {url}"))?;
  }

  let cwd = std::env::current_dir()?;
  let setup = crate::commands::script_setup::resolve(&config, &cwd, &args.extensions).await?;
  let script_host = std::sync::Arc::new(ferridriver_script::SessionScriptHost::new(
    std::sync::Arc::clone(browser.state()),
    &args.id,
    ferridriver_script::SessionScriptConfig {
      script_root: setup.script_root,
      artifacts: setup.artifacts,
      caps: setup.caps,
      extensions: setup.extensions,
      engine: setup.engine,
    },
  ));

  let registry = Registry::open()?;
  let session = bind_in(&registry, &browser, &args.id, BindOptions::default(), Some(script_host))
    .await
    .context("binding the session")?;
  tracing::info!(id = %args.id, endpoint = %session.endpoint(), "session host serving");

  // Serve until a shutdown signal arrives. Racing against the signal (rather
  // than letting SIGTERM default-kill the process) lets the BoundSession drop
  // run on the way out: it stops the server, prunes the descriptor, removes
  // the socket file, and closes the browser.
  let serve = session.server().serve();
  tokio::select! {
    res = serve => { res.context("serving the session")?; },
    () = shutdown_signal() => {
      tracing::info!(id = %args.id, "session host received shutdown signal");
    },
  }
  drop(session);
  browser.close().await.ok();
  Ok(())
}

/// Resolve when the process receives SIGTERM or SIGINT (Ctrl-C).
async fn shutdown_signal() {
  #[cfg(unix)]
  {
    use tokio::signal::unix::{SignalKind, signal};
    let Ok(mut term) = signal(SignalKind::terminate()) else {
      return std::future::pending().await;
    };
    let Ok(mut int) = signal(SignalKind::interrupt()) else {
      return std::future::pending().await;
    };
    tokio::select! {
      _ = term.recv() => {},
      _ = int.recv() => {},
    }
  }
  #[cfg(not(unix))]
  {
    let _ = tokio::signal::ctrl_c().await;
  }
}

/// `attach`: connect and print the session's current snapshot.
async fn attach(args: SessionTargetArgs) -> anyhow::Result<()> {
  let result = run_on_session(
    &args.id,
    None,
    ScriptRequest::source(ATTACH_SNAPSHOT),
    false,
    &RunSinks::default(),
  )
  .await?;
  match result.outcome {
    ferridriver_script::Outcome::Ok { success } => {
      match success.value {
        serde_json::Value::String(text) => println!("{text}"),
        other => println!("{other}"),
      }
      Ok(())
    },
    ferridriver_script::Outcome::Error { error } => {
      anyhow::bail!("snapshotting session '{}': {}", args.id, error.message)
    },
  }
}

/// Run one script against a live session, streaming its console to this
/// process's stdout/stderr as the host produces it.
///
/// `json` suppresses the streaming render and folds the streamed console into
/// the returned result instead, so `--json` still emits one document with
/// every line in it — the host always streams, the client decides how to show
/// it.
/// Where a session run's side channels land in this process.
pub struct RunSinks {
  /// Accumulates the generated source the host streamed.
  pub code: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
  /// Whether to also print each line as it arrives.
  pub echo_code: bool,
  /// Receives the page the host reported, when the request asked for it.
  pub page: std::sync::Arc<std::sync::Mutex<Option<ferridriver::response::PageState>>>,
}

impl Default for RunSinks {
  fn default() -> Self {
    Self {
      code: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
      echo_code: false,
      page: std::sync::Arc::new(std::sync::Mutex::new(None)),
    }
  }
}

pub async fn run_on_session(
  id: &str,
  context: Option<&str>,
  request: ScriptRequest,
  json: bool,
  sinks: &RunSinks,
) -> anyhow::Result<ferridriver_script::ScriptResult> {
  let registry = Registry::open()?;
  let mut client = SessionClient::attach(&registry, id)
    .await
    .with_context(|| format!("attaching to session '{id}'"))?;

  let command = Command::new(1, RUN_VERB, serde_json::to_value(&request)?).with_context(context.map(str::to_string));

  let mut streamed: Vec<ferridriver_script::ConsoleEntry> = Vec::new();
  let reply = client
    .call_with_events(command, |event| match event.payload {
      ferridriver_session::EventPayload::Console { level, message, ts_ms } => {
        let entry = ferridriver_script::ConsoleEntry {
          level: console_level(&level),
          message,
          ts_ms,
        };
        if json {
          streamed.push(entry);
        } else {
          crate::commands::run::console::print_entry(&entry);
        }
      },
      ferridriver_session::EventPayload::Code { line } => {
        if sinks.echo_code {
          eprintln!("{line}");
        }
        if let Ok(mut code) = sinks.code.lock() {
          code.push(line);
        }
      },
      ferridriver_session::EventPayload::Page {
        url,
        title,
        console_errors,
        console_warnings,
        page_errors,
      } => {
        if let Ok(mut page) = sinks.page.lock() {
          *page = Some(ferridriver::response::PageState {
            url,
            title,
            console_errors,
            console_warnings,
            page_errors,
          });
        }
      },
      // Action lines are a live view for a human, never part of the result
      // document — `--json` never asks for them in the first place.
      ferridriver_session::EventPayload::Action {
        phase,
        title,
        params,
        duration_ms,
        error,
        message,
        location,
        ..
      } => match phase {
        ferridriver_session::ActionPhase::Begin => {
          crate::commands::run::console::print_action_begin(
            &title,
            params.as_ref().unwrap_or(&serde_json::Value::Null),
            location.as_deref(),
          );
        },
        ferridriver_session::ActionPhase::Log => {
          crate::commands::run::console::print_action_log(message.as_deref().unwrap_or_default());
        },
        ferridriver_session::ActionPhase::End => {
          #[allow(clippy::cast_precision_loss)] // display only, and milliseconds never reach 2^53
          let ms = duration_ms.unwrap_or_default() as f64;
          crate::commands::run::console::print_action_end(&title, ms, error.as_deref());
        },
      },
    })
    .await?;

  if !reply.ok {
    anyhow::bail!("{}", reply.error.as_deref().unwrap_or("session run failed"));
  }
  let mut result: ferridriver_script::ScriptResult =
    serde_json::from_str(&reply.text).context("decoding the session's run result")?;
  result.console.extend(streamed);
  Ok(result)
}

/// Decode a wire console level. An unknown level means a newer host is talking
/// to an older client; render it rather than dropping the line.
fn console_level(level: &str) -> ferridriver_script::ConsoleLevel {
  serde_json::from_value(serde_json::Value::String(level.to_string())).unwrap_or(ferridriver_script::ConsoleLevel::Log)
}

/// `list`: read the registry and print live sessions.
fn list(_args: &SessionListArgs) -> anyhow::Result<()> {
  let registry = Registry::open()?;
  let sessions = registry.list()?;
  if ui::json() {
    return ui::print_json(&sessions);
  }
  if sessions.is_empty() {
    ui::say(&ui::info("no live sessions"));
    ui::next_steps(&[("open one", "ferridriver session open dev".to_string())]);
    return Ok(());
  }
  let mut table = ui::Table::new(&["ID", "BROWSER", "PID", "ENDPOINT"]).flex(3);
  for s in &sessions {
    table.row([
      ui::bold(&s.id),
      s.browser_name.clone(),
      ui::dim(&s.pid.to_string()),
      ui::dim(&s.endpoint),
    ]);
  }
  table.print(ui::width());
  Ok(())
}

/// `close`: stop the session. The browser is owned by the detached host
/// process, so signal that process to exit (its [`ferridriver_session::BoundSession`]
/// drop closes the browser and prunes the descriptor); then prune the
/// descriptor directly in case the host already died.
fn close(args: &SessionTargetArgs) -> anyhow::Result<()> {
  let registry = Registry::open()?;
  let descriptor = registry.get(&args.id)?;
  if let Some(d) = &descriptor {
    terminate_owner(d.pid);
  }
  ferridriver_session::unbind(&args.id)?;
  if descriptor.is_some() {
    ui::say(&ui::success(&format!("closed session {}", ui::bold(&args.id))));
  } else {
    ui::say(&ui::info(&format!("no session {}", ui::bold(&args.id))));
  }
  Ok(())
}

/// `close-all`: stop every session.
fn close_all() -> anyhow::Result<()> {
  let registry = Registry::open()?;
  let sessions = registry.list()?;
  for s in &sessions {
    terminate_owner(s.pid);
    ferridriver_session::unbind(&s.id)?;
  }
  ui::say(&ui::success(&format!("closed {} session(s)", sessions.len())));
  Ok(())
}

/// Ask the owning host process to exit. SIGTERM lets the host run its
/// `BoundSession` drop (close the browser, remove the socket) cleanly. A no-op
/// when the pid is this process (the rare same-process bind) or already gone.
#[cfg(unix)]
fn terminate_owner(pid: u32) {
  if pid == std::process::id() {
    return;
  }
  let Ok(pid) = libc::pid_t::try_from(pid) else {
    return;
  };
  // SAFETY: kill(2) with SIGTERM on a pid; failure (already dead, not ours)
  // is ignored. No memory is touched.
  #[allow(unsafe_code)]
  unsafe {
    libc::kill(pid, libc::SIGTERM);
  }
}

#[cfg(not(unix))]
fn terminate_owner(_pid: u32) {
  // On non-unix the host is reaped via the registry prune + its own exit;
  // a portable signal path can be added when a Windows host ships.
}

fn backend_name(browser: &BrowserArgs) -> &'static str {
  match browser.backend_kind().unwrap_or(BackendKind::CdpPipe) {
    BackendKind::CdpPipe => "cdp-pipe",
    BackendKind::CdpRaw => "cdp-raw",
    BackendKind::WebKit => "webkit",
    BackendKind::Bidi => "bidi",
  }
}

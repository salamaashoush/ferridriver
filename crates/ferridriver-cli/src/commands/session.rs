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
use ferridriver::options::BrowserKind;
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
    SessionCommand::Host(a) => {
      let diagnostics = a.startup_error.clone();
      let result = Box::pin(host(config, a)).await;
      if let (Some(path), Err(error)) = (diagnostics, &result)
        && let Err(write) = write_startup_error(&path, error)
      {
        tracing::warn!(%write, "Writing session startup error failed");
      }
      result
    },
    SessionCommand::Attach(a) => attach(a).await,
    SessionCommand::List(a) => list(&a),
    SessionCommand::Close(a) => close(&a).await,
    SessionCommand::CloseAll => close_all().await,
  }
}

fn write_startup_error(path: &std::path::Path, error: &anyhow::Error) -> std::io::Result<()> {
  use std::io::Write as _;
  let mut file = match std::fs::OpenOptions::new().write(true).truncate(true).open(path) {
    Ok(file) => file,
    // The opener removes this file once the descriptor is published.
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
    Err(error) => return Err(error),
  };
  let detail = format!("{error:#}");
  let end = detail.floor_char_boundary(16 * 1024);
  file.write_all(&detail.as_bytes()[..end])
}

fn configure_browser(
  config: &mut ferridriver_config::mcp::McpConfig,
  instance: &str,
  args: &BrowserArgs,
) -> anyhow::Result<Option<ferridriver::state::ConnectMode>> {
  let mut target = config.instance_settings(instance).map_err(anyhow::Error::msg)?;
  if args.browser.is_some() || args.backend.is_some() {
    target.browser = args.browser;
    target.backend = args.backend_kind();
  }
  if let Some(headless) = args.headless_override() {
    target.headless = Some(headless);
  }
  if let Some(path) = &args.executable_path {
    target.executable_path = Some(path.clone());
  }
  if let Some(path) = &args.user_data_dir {
    target.user_data_dir = Some(path.clone());
  }
  let connection = if let Some(endpoint) = &args.connect {
    target.device = None;
    target.connect_url = Some(endpoint.clone());
    None
  } else if let Some(channel) = &args.auto_connect {
    if args.browser.is_some_and(|browser| browser != BrowserKind::Chromium)
      || args
        .backend_kind()
        .is_some_and(|backend| !matches!(backend, BackendKind::CdpPipe | BackendKind::CdpWs))
    {
      anyhow::bail!("--auto-connect requires Chromium over CDP");
    }
    target.device = None;
    target.browser = Some(BrowserKind::Chromium);
    target.backend = args.backend_kind();
    target.connect_url = None;
    target.connect_options = None;
    Some(ferridriver::state::ConnectMode::AutoConnect {
      channel: channel.clone(),
      user_data_dir: target.user_data_dir.clone(),
    })
  } else {
    None
  };
  config.browser.instances.insert(instance.to_owned(), target);
  Ok(connection)
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
  if let Some(instance) = &args.instance {
    cmd.arg("--instance").arg(instance);
  }
  if let Some(backend) = args.browser.backend_kind() {
    cmd.arg("--backend").arg(backend.name());
  }
  if let Some(browser) = args.browser.browser {
    cmd.arg("--browser").arg(browser.name());
  }
  if args.browser.headless {
    cmd.arg("--headless");
  }
  if args.browser.headed {
    cmd.arg("--headed");
  }
  if let Some(endpoint) = &args.browser.connect {
    cmd.arg("--connect").arg(endpoint);
  }
  if let Some(channel) = &args.browser.auto_connect {
    cmd.arg("--auto-connect").arg(channel);
  }
  if let Some(profile) = &args.browser.user_data_dir {
    cmd.arg("--user-data-dir").arg(profile);
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
  #[cfg(unix)]
  {
    use std::os::unix::process::CommandExt as _;
    cmd.process_group(0);
  }
  cmd.stdin(std::process::Stdio::null());
  cmd.stdout(std::process::Stdio::null());
  let mut diagnostics = tempfile::NamedTempFile::new_in(registry.dir()).context("creating session startup result")?;
  cmd.arg("--startup-error").arg(diagnostics.path());
  cmd.stderr(std::process::Stdio::null());
  let mut child = cmd.spawn().context("spawning session host process")?;

  // The host owns the configured provisioning deadline, including SDK installation.
  let descriptor = match wait_for_descriptor(&registry, &args.id, &mut child).await {
    Ok(descriptor) => descriptor,
    Err(error) => {
      if child.try_wait()?.is_some() {
        use std::io::{Read as _, Seek as _};
        diagnostics.as_file_mut().rewind()?;
        let mut detail = String::new();
        diagnostics.as_file_mut().read_to_string(&mut detail)?;
        anyhow::bail!("{error:#}: {}", detail.trim());
      }
      return Err(error);
    },
  };
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

async fn wait_for_descriptor(
  registry: &Registry,
  id: &str,
  child: &mut std::process::Child,
) -> anyhow::Result<ferridriver_session::SessionDescriptor> {
  loop {
    if let Some(status) = child.try_wait()? {
      anyhow::bail!("session '{id}' host exited with {status}");
    }
    if let Some(d) = registry.get(id)? {
      if d.pid != child.id() {
        anyhow::bail!("session '{id}' was claimed by another host");
      }
      return Ok(d);
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
  }
}

/// `host`: the long-lived foreground process. Launch, bind, navigate, serve
/// until killed. `open` spawns this detached.
async fn host(mut config: FerridriverConfig, args: SessionHostArgs) -> anyhow::Result<()> {
  let instance = args.instance.as_deref().unwrap_or("default");
  let connection = configure_browser(&mut config.mcp, instance, &args.browser)?;
  let cwd = std::env::current_dir()?;
  let setup = crate::commands::script_setup::resolve(&config, &cwd, &args.extensions).await?;
  let provisioned = Box::pin(crate::commands::instance::provision_instance(
    config.mcp,
    instance,
    args.browser.headed,
    false,
    connection,
  ))
  .await?;
  let browser = provisioned.browser;
  let serving = async {
    if let Some(url) = &args.url {
      provisioned
        .page
        .goto(url)
        .await
        .with_context(|| format!("navigating to {url}"))?;
    }
    let script_host = std::sync::Arc::new(ferridriver_script::SessionScriptHost::new(
      std::sync::Arc::clone(&browser),
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
    let mut session = bind_in(&registry, &browser, &args.id, BindOptions::default(), Some(script_host))
      .await
      .context("binding the session")?;
    tracing::info!(id = %args.id, endpoint = %session.endpoint(), "session host serving");
    tokio::select! {
      result = session.wait() => result.context("serving the session")?,
      () = shutdown_signal() => tracing::info!(id = %args.id, "session host received shutdown signal"),
    }
    drop(session);
    Ok::<_, anyhow::Error>(())
  }
  .await;
  let cleanup = browser.close().await;
  match (serving, cleanup) {
    (Ok(()), Ok(())) => Ok(()),
    (Err(error), Ok(())) => Err(error),
    (Ok(()), Err(error)) => Err(error.into()),
    (Err(error), Err(cleanup)) => Err(anyhow::anyhow!("{error:#}; browser cleanup failed: {cleanup}")),
  }
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

async fn close_descriptor(
  registry: &Registry,
  descriptor: &ferridriver_session::SessionDescriptor,
) -> anyhow::Result<()> {
  let mut client = SessionClient::attach(registry, &descriptor.id).await?;
  let reply = client
    .call(Command::new(
      1,
      ferridriver_session::CLOSE_VERB,
      serde_json::json!({"endpoint": descriptor.endpoint, "generation": descriptor.generation}),
    ))
    .await?;
  if !reply.ok {
    anyhow::bail!("{}", reply.error.as_deref().unwrap_or("session cleanup failed"));
  }
  Ok(())
}

async fn close(args: &SessionTargetArgs) -> anyhow::Result<()> {
  let registry = Registry::open()?;
  if let Some(descriptor) = registry.get(&args.id)? {
    close_descriptor(&registry, &descriptor).await?;
    ui::say(&ui::success(&format!("closed session {}", ui::bold(&args.id))));
  } else {
    ui::say(&ui::info(&format!("no session {}", ui::bold(&args.id))));
  }
  Ok(())
}

async fn close_all() -> anyhow::Result<()> {
  let registry = Registry::open()?;
  let sessions = registry.list()?;
  let mut errors = Vec::new();
  for session in &sessions {
    if let Err(error) = close_descriptor(&registry, session).await {
      errors.push(format!("{}: {error:#}", session.id));
    }
  }
  if !errors.is_empty() {
    anyhow::bail!("{}", errors.join("; "));
  }
  ui::say(&ui::success(&format!("closed {} session(s)", sessions.len())));
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn session_readiness_follows_publication_and_host_exit() {
    let directory = tempfile::tempdir().unwrap();
    let registry = Registry::open_at(directory.path()).unwrap();
    let mut child = std::process::Command::new("sleep").arg("30").spawn().unwrap();
    let pid = child.id();
    let publisher = registry.clone();
    let publish = async move {
      tokio::time::sleep(std::time::Duration::from_millis(75)).await;
      publisher
        .put(&ferridriver_session::SessionDescriptor {
          id: "sashoush-startup".into(),
          endpoint: "owned-session.sock".into(),
          generation: "startup-test".into(),
          pid,
          browser_name: "chromium".into(),
          version: env!("CARGO_PKG_VERSION").into(),
          workspace_dir: None,
          metadata: None,
        })
        .unwrap();
    };
    let (ready, ()) = tokio::join!(wait_for_descriptor(&registry, "sashoush-startup", &mut child), publish);
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(ready.unwrap().pid, pid);
    let exited = wait_for_descriptor(&registry, "sashoush-startup", &mut child)
      .await
      .unwrap_err();
    assert!(exited.to_string().contains("host exited"), "{exited}");
  }

  #[test]
  fn startup_errors_are_bounded_and_do_not_recreate_the_openers_file() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let path = file.path().to_owned();
    write_startup_error(&path, &anyhow::anyhow!("{}", "é".repeat(20_000))).unwrap();
    let detail = std::fs::read_to_string(&path).unwrap();
    assert_eq!(detail.len(), 16 * 1024);
    file.close().unwrap();
    write_startup_error(&path, &anyhow::anyhow!("later host diagnostics")).unwrap();
    assert!(!path.exists());
  }
}

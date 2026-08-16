//! `ferridriver trace` — read a recorded trace.
//!
//! Three ways in, one model behind them:
//!
//! * `view` serves the embedded Playwright trace viewer and opens it, so a
//!   trace opens with no npm, no network, and no second tool installed;
//! * `show` prints the same trace as text, which is what is actually
//!   available over ssh, in a CI log, or to an agent reading a failure;
//! * `ls` says which traces a run produced and which of them failed.
//!
//! `view` reads its files through the viewer crate's `/trace/file?path=`
//! route, the same one live recordings are served from — a directory of
//! loose trace files opens exactly like a finished `trace.zip`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use ferridriver_config::FerridriverConfig;
use ferridriver_viewer::model::{TraceModel, TraceSource};
use ferridriver_viewer::{App, FileRoots, dump};

use crate::cli;

pub async fn run(config: &FerridriverConfig, args: cli::TraceArgs) -> anyhow::Result<()> {
  match args.command {
    cli::TraceCommand::View(view) => view_trace(config, view).await,
    cli::TraceCommand::Show(show) => show_trace(config, &show),
    cli::TraceCommand::Ls(ls) => list_traces(config, &ls),
  }
}

// ── view ────────────────────────────────────────────────────────────────

async fn view_trace(config: &FerridriverConfig, args: cli::TraceViewArgs) -> anyhow::Result<()> {
  let remote = args
    .trace
    .as_deref()
    .filter(|trace| trace.starts_with("http://") || trace.starts_with("https://"))
    .map(ToString::to_string);

  let (trace_param, roots) = if let Some(url) = remote {
    (url, FileRoots::new([std::env::current_dir()?]))
  } else {
    let path = match args.trace {
      Some(trace) => resolve_existing(Path::new(&trace))?,
      None => newest_trace(config)?,
    };
    let root = if path.is_dir() {
      path.clone()
    } else {
      path.parent().unwrap_or(Path::new(".")).to_path_buf()
    };
    // A directory is addressed through the marker entry, which the file
    // route answers with a listing of every trace inside it.
    let target = if path.is_dir() {
      path.join(ferridriver_viewer::TRACES_DIR_MARKER)
    } else {
      path
    };
    (ferridriver_viewer::trace_param(&target), FileRoots::new([root]))
  };

  let url = serve_viewer(&args.host, args.port, roots, "index.html", &[("trace", trace_param)]).await?;

  if args.no_open {
    println!("Serving the trace viewer on {url}");
    println!("Press Ctrl-C to stop.");
    tokio::signal::ctrl_c().await.ok();
    return Ok(());
  }

  println!("Opening {url}");
  open_app_window(&url).await
}

/// Bind a server that serves `app` plus the trace file route, and return the
/// URL of its entry point.
async fn serve_viewer(
  host: &str,
  port: Option<u16>,
  roots: FileRoots,
  web_app: &str,
  params: &[(&str, String)],
) -> anyhow::Result<String> {
  let ip: std::net::IpAddr = host.parse().with_context(|| format!("invalid --host {host}"))?;
  let addr = SocketAddr::new(ip, port.unwrap_or(0));
  let listener = tokio::net::TcpListener::bind(addr)
    .await
    .with_context(|| format!("bind trace viewer on {addr}"))?;
  let local = listener.local_addr()?;

  let base = format!("http://{}", displayable(local));
  let url = ferridriver_viewer::app_url(&base, web_app, params);
  let redirect = url.clone();
  let router = ferridriver_viewer::router(App::TraceViewer, roots).route(
    "/",
    axum::routing::get(move || {
      let redirect = redirect.clone();
      async move { axum::response::Redirect::temporary(&redirect) }
    }),
  );
  tokio::spawn(async move {
    let _ = axum::serve(listener, router).await;
  });
  Ok(url)
}

/// `0.0.0.0` is a bind address, not somewhere a browser can go.
fn displayable(addr: SocketAddr) -> String {
  if addr.ip().is_unspecified() {
    format!("127.0.0.1:{}", addr.port())
  } else {
    addr.to_string()
  }
}

/// Open `url` in a chromium app window and stay up until it is closed —
/// Playwright's `openTraceViewerApp`, which is why the viewer feels like an
/// application rather than a browser tab.
async fn open_app_window(url: &str) -> anyhow::Result<()> {
  use ferridriver::chromium;
  use ferridriver::options::LaunchOptions;

  let browser = chromium()
    .launch(LaunchOptions {
      headless: Some(false),
      args: vec![format!("--app={url}"), "--window-size=1280,800".to_string()],
      ..LaunchOptions::default()
    })
    .await;

  let browser = match browser {
    Ok(browser) => browser,
    Err(e) => {
      // No browser installed is a normal state for someone who only wanted
      // to read a trace, so say where it is instead of failing.
      eprintln!("could not open a browser window ({e})");
      println!("Open this URL yourself: {url}");
      tokio::signal::ctrl_c().await.ok();
      return Ok(());
    },
  };

  // `--app` already opened the window; `page()` adopts it rather than
  // opening a second one.
  let page = Box::pin(browser.page()).await?;
  tokio::select! {
    _ = page.wait_for_event("close", Some(86_400_000)) => {},
    _ = tokio::signal::ctrl_c() => {},
  }
  let _ = browser.close().await;
  Ok(())
}

// ── show ────────────────────────────────────────────────────────────────

fn show_trace(config: &FerridriverConfig, args: &cli::TraceShowArgs) -> anyhow::Result<()> {
  let path = match &args.trace {
    Some(trace) => resolve_existing(trace)?,
    None => newest_trace(config)?,
  };
  let model = load(&path)?;

  if args.json {
    println!("{}", serde_json::to_string_pretty(&dump::to_json(&model))?);
    return Ok(());
  }

  let options = dump::DumpOptions {
    scope: if args.errors {
      dump::Scope::Failures
    } else {
      dump::Scope::Everything
    },
    sections: dump::Sections {
      logs: !args.hide.contains(&cli::TraceSection::Logs),
      console: !args.hide.contains(&cli::TraceSection::Console),
      network: !args.hide.contains(&cli::TraceSection::Network),
    },
    limit: args.limit,
    color: use_color(&args.color),
  };
  print!("{}", dump::render(&model, &options));
  Ok(())
}

fn use_color(setting: &str) -> bool {
  match setting {
    "always" => true,
    "never" => false,
    // `auto`: a pipe gets plain text, a terminal gets color, and NO_COLOR
    // wins over both (https://no-color.org).
    _ => std::env::var_os("NO_COLOR").is_none() && std::io::IsTerminal::is_terminal(&std::io::stdout()),
  }
}

// ── ls ──────────────────────────────────────────────────────────────────

fn list_traces(config: &FerridriverConfig, args: &cli::TraceLsArgs) -> anyhow::Result<()> {
  let dir = args.dir.clone().unwrap_or_else(|| output_dir(config));
  if !dir.exists() {
    bail!("{} does not exist", dir.display());
  }
  let traces = collect_traces(&dir);
  if traces.is_empty() {
    if args.json {
      println!("[]");
    } else {
      println!("no traces under {}", dir.display());
    }
    return Ok(());
  }

  let mut rows = Vec::with_capacity(traces.len());
  for path in traces {
    let summary = load(&path).map(|model| dump::one_line_summary(&model));
    let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or_default();
    rows.push((path, size, summary));
  }

  if args.json {
    let entries: Vec<serde_json::Value> = rows
      .iter()
      .map(|(path, size, summary)| {
        serde_json::json!({
          "path": path.display().to_string(),
          "bytes": size,
          "summary": summary.as_ref().ok(),
          "error": summary.as_ref().err().map(ToString::to_string),
        })
      })
      .collect();
    println!("{}", serde_json::to_string_pretty(&entries)?);
    return Ok(());
  }

  for (path, size, summary) in rows {
    let detail = match summary {
      Ok(summary) => summary,
      Err(error) => format!("unreadable: {error}"),
    };
    println!("{}  {:>9}  {detail}", path.display(), human_bytes(size));
  }
  Ok(())
}

fn human_bytes(bytes: u64) -> String {
  #[allow(clippy::cast_precision_loss)]
  let bytes = bytes as f64;
  match bytes {
    b if b >= 1024.0 * 1024.0 => format!("{:.1}MB", b / (1024.0 * 1024.0)),
    b if b >= 1024.0 => format!("{:.0}KB", b / 1024.0),
    b => format!("{b:.0}B"),
  }
}

// ── shared ──────────────────────────────────────────────────────────────

fn load(path: &Path) -> anyhow::Result<TraceModel> {
  let source = TraceSource::open(path).map_err(|e| anyhow::anyhow!("{e}"))?;
  TraceModel::load(&source).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

fn resolve_existing(path: &Path) -> anyhow::Result<PathBuf> {
  if !path.exists() {
    bail!("{} does not exist", path.display());
  }
  Ok(std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

fn output_dir(config: &FerridriverConfig) -> PathBuf {
  let dir = config.test.output_dir.clone();
  if dir.is_absolute() {
    return dir;
  }
  config
    .source_dir
    .clone()
    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
    .join(dir)
}

/// The trace a person means when they do not name one: the last one this
/// project recorded.
fn newest_trace(config: &FerridriverConfig) -> anyhow::Result<PathBuf> {
  let dir = output_dir(config);
  let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
  for path in collect_traces(&dir) {
    let Ok(modified) = std::fs::metadata(&path).and_then(|meta| meta.modified()) else {
      continue;
    };
    if newest.as_ref().is_none_or(|(seen, _)| modified > *seen) {
      newest = Some((modified, path));
    }
  }
  match newest {
    Some((_, path)) => Ok(path),
    None => bail!(
      "no traces under {} — record one with `trace: 'on'` in ferridriver.toml, then pass its path",
      dir.display()
    ),
  }
}

/// Every trace archive under `dir`, recursively. Directories of loose trace
/// files count too — that is what an interrupted run leaves behind.
fn collect_traces(dir: &Path) -> Vec<PathBuf> {
  let mut found = Vec::new();
  let mut stack = vec![dir.to_path_buf()];
  while let Some(current) = stack.pop() {
    let Ok(entries) = std::fs::read_dir(&current) else {
      continue;
    };
    let mut has_loose_trace = false;
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_dir() {
        stack.push(path);
      } else if path.extension().is_some_and(|ext| ext == "zip")
        && path
          .file_name()
          .is_some_and(|name| name.to_string_lossy().contains("trace"))
      {
        found.push(path);
      } else if path.extension().is_some_and(|ext| ext == "trace") {
        has_loose_trace = true;
      }
    }
    if has_loose_trace {
      found.push(current);
    }
  }
  found.sort();
  found
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn collects_archives_and_live_directories() {
    let dir = tempfile::tempdir().expect("tempdir");
    let nested = dir.path().join("suite").join("test");
    std::fs::create_dir_all(&nested).expect("mkdir");
    std::fs::write(nested.join("t-attempt1.trace.zip"), b"PK").expect("write");
    std::fs::write(dir.path().join("unrelated.zip"), b"PK").expect("write");
    let live = dir.path().join(".artifacts").join("traces");
    std::fs::create_dir_all(&live).expect("mkdir");
    std::fs::write(live.join("abc.trace"), b"{}").expect("write");

    let traces = collect_traces(dir.path());
    assert!(traces.iter().any(|p| p.ends_with("t-attempt1.trace.zip")), "{traces:?}");
    assert!(traces.contains(&live), "loose recording missing: {traces:?}");
    assert!(
      !traces.iter().any(|p| p.ends_with("unrelated.zip")),
      "non-trace archive picked up: {traces:?}"
    );
  }

  #[test]
  fn color_setting_is_explicit_or_terminal_bound() {
    assert!(use_color("always"));
    assert!(!use_color("never"));
  }

  #[test]
  fn unspecified_bind_addresses_are_printed_as_loopback() {
    assert_eq!(displayable("0.0.0.0:9323".parse().expect("addr")), "127.0.0.1:9323");
    assert_eq!(displayable("127.0.0.1:9323".parse().expect("addr")), "127.0.0.1:9323");
  }

  #[test]
  fn byte_sizes_read_as_sizes() {
    assert_eq!(human_bytes(512), "512B");
    assert_eq!(human_bytes(2048), "2KB");
    assert_eq!(human_bytes(3 * 1024 * 1024), "3.0MB");
  }
}

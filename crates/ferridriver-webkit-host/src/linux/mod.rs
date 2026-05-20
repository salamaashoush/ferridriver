//! Linux host orchestrator. Owns the GTK4 main loop, the view registry,
//! and the writer half of the IPC socket. A dedicated reader thread
//! drains fd 3 with blocking [`frame_read`](ferridriver_webkit_wire::frame_read)
//! and posts dispatch closures onto the GTK main context — so all
//! webkit6 / gtk4 calls stay on the single thread that initialised them.

use ferridriver_webkit_wire::frame_read;
use std::cell::RefCell;
use std::io;
use std::os::unix::io::FromRawFd;
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

pub(crate) mod dispatch;
pub(crate) mod userscripts;
pub(crate) mod view;
pub(crate) mod writer;

/// Frame the main thread hands off to the dedicated writer thread.
/// Owned by [`WRITER_TX`]; the writer thread receives `Frame`s and
/// performs the actual `frame_write` synchronously off the GTK main
/// loop. Keeping writes off the main thread is critical: if the
/// socket buffer fills (parent's reader is slow), a synchronous
/// `write_all` would freeze the GTK main loop, which would then stop
/// processing incoming ops — the whole host wedges. With this
/// thread, the main loop only blocks on a cheap channel send.
pub(crate) struct Frame {
  pub rid: u32,
  pub rep: u8,
  pub payload: Vec<u8>,
}

thread_local! {
  /// Live views keyed by the `view_id` the host assigned at create-time.
  /// Owned solely by the main thread. Reader thread never touches it —
  /// it only posts closures through `MainContext::invoke` and we look it
  /// up from inside those closures.
  pub(crate) static REGISTRY: RefCell<view::ViewRegistry> = RefCell::new(view::ViewRegistry::new());

  /// Sender to the dedicated writer thread. Dispatch handlers and signal
  /// callbacks `send()` here; the writer thread (spawned in [`run`]) is
  /// the only owner of the actual socket. `std::sync::mpsc` is fine
  /// because there's only ever one producer (the main thread).
  pub(crate) static WRITER_TX: RefCell<Option<std::sync::mpsc::Sender<Frame>>> = const { RefCell::new(None) };

  /// `glib::MainLoop` clone parked here so the reader thread can ask
  /// the main thread to quit (via an `invoke` closure) when the parent
  /// closes the socket.
  pub(crate) static MAIN_LOOP: RefCell<Option<glib::MainLoop>> = const { RefCell::new(None) };
}

fn init_tracing() {
  use tracing_subscriber::EnvFilter;
  let filter = EnvFilter::try_from_env("FERRIDRIVER_WEBKIT_HOST_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
  let _ = tracing_subscriber::fmt()
    .with_env_filter(filter)
    .with_writer(std::io::stderr)
    .try_init();
}

/// Recover fd 3 — the parent's `IpcClient::spawn_host_process` dup2s
/// the child end of the socketpair into place before exec.
fn take_ipc_socket() -> UnixStream {
  // SAFETY: fd 3 is opened by the parent and dup'd into place before
  // exec; the host owns it exclusively from this point. The parent
  // also clears `FD_CLOEXEC` on it. No other code takes ownership.
  #[allow(unsafe_code)]
  unsafe {
    UnixStream::from_raw_fd(3)
  }
}

pub fn run() -> ExitCode {
  init_tracing();
  tracing::info!(
    webkit_gtk = webkit_version_string().as_str(),
    "ferridriver-webkit-host starting"
  );

  // Headless via xvfb-run. When `FERRIDRIVER_WEBKIT_HEADLESS=1` is set
  // AND we're not already running under a re-spawn (sentinel env
  // var prevents fork bombs), `exec` ourselves under `xvfb-run -a`.
  // The child inherits the IPC socket on fd 3 because xvfb-run does
  // not close fds it doesn't know about. If `xvfb-run` is not on
  // `PATH` we fall back to running directly against the caller's
  // `$DISPLAY` — the window will be briefly visible but tests still
  // work (Phase 5 / CI installs `xorg-server-xvfb` to avoid this).
  if std::env::var("FERRIDRIVER_WEBKIT_HEADLESS").as_deref() == Ok("1")
    && std::env::var("FERRIDRIVER_WEBKIT_HEADLESS_INNER").is_err()
  {
    if has_xvfb_run() {
      // Skip the GTK init below — `xvfb-run` runs us in a clean child.
      return spawn_under_xvfb();
    }
    tracing::warn!(
      "FERRIDRIVER_WEBKIT_HEADLESS=1 set but `xvfb-run` not on PATH; \
       falling back to direct display (window will be visible). \
       Install xorg-server-xvfb (Arch) / xvfb (Debian/Ubuntu) for true headless."
    );
  }

  if let Err(e) = gtk4::init() {
    tracing::error!("gtk4::init() failed: {e}");
    return ExitCode::from(70); // EX_SOFTWARE
  }

  let socket = take_ipc_socket();
  let reader_socket = match socket.try_clone() {
    Ok(s) => s,
    Err(e) => {
      tracing::error!("clone IPC socket: {e}");
      return ExitCode::from(70);
    },
  };

  // Spawn the writer thread first — `WRITER_TX` must be live before
  // the GTK main loop boots, so any signal handler triggered during
  // view construction can already send.
  let (writer_tx, writer_rx) = std::sync::mpsc::channel::<Frame>();
  WRITER_TX.with(|w| *w.borrow_mut() = Some(writer_tx));
  spawn_writer_thread(socket, writer_rx);

  let main_ctx = glib::MainContext::default();
  let main_loop = glib::MainLoop::new(Some(&main_ctx), false);
  MAIN_LOOP.with(|ml| *ml.borrow_mut() = Some(main_loop.clone()));

  spawn_reader_thread(reader_socket, main_ctx.clone());

  main_loop.run();
  tracing::info!("ferridriver-webkit-host main loop exited");
  ExitCode::SUCCESS
}

/// Spawn the background thread that owns the writer half of the IPC
/// socket. Loops on `recv()` from `writer_rx` and performs the
/// synchronous `frame_write` here so the GTK main thread is never
/// blocked on a slow parent reader.
fn spawn_writer_thread(mut socket: UnixStream, rx: std::sync::mpsc::Receiver<Frame>) {
  use ferridriver_webkit_wire::frame_write;
  std::thread::spawn(move || {
    while let Ok(frame) = rx.recv() {
      if let Err(e) = frame_write(&mut socket, frame.rid, frame.rep, &frame.payload) {
        // EPIPE = parent closed socket; main loop will quit via the
        // reader thread's UnexpectedEof path. Just drain.
        if e.kind() != std::io::ErrorKind::BrokenPipe {
          tracing::error!("writer thread: {e}");
        }
        return;
      }
    }
  });
}

/// Spawn the background thread that does the blocking `frame_read` and
/// hands each frame to the GTK main loop via `MainContext::invoke`.
fn spawn_reader_thread(mut socket: UnixStream, main_ctx: glib::MainContext) {
  std::thread::spawn(move || {
    loop {
      match frame_read(&mut socket) {
        Ok((req_id, op, payload)) => {
          main_ctx.invoke(move || dispatch::handle(req_id, op, &payload));
        },
        Err(e) => {
          if e.kind() == io::ErrorKind::UnexpectedEof {
            tracing::info!("parent closed IPC socket; quitting main loop");
          } else {
            tracing::error!("reader thread: {e}");
          }
          main_ctx.invoke(|| {
            MAIN_LOOP.with(|ml| {
              if let Some(ml) = ml.borrow().as_ref() {
                ml.quit();
              }
            });
          });
          return;
        },
      }
    }
  });
}

/// Return `true` when `xvfb-run` resolves on `$PATH`. The headless
/// re-exec at startup only kicks in when this returns `true`, so a
/// missing `xorg-server-xvfb` install on a dev box silently falls
/// back to direct-display mode instead of hard-erroring on every nav.
fn has_xvfb_run() -> bool {
  let Some(path) = std::env::var_os("PATH") else {
    return false;
  };
  std::env::split_paths(&path).any(|dir| dir.join("xvfb-run").is_file())
}

/// Re-exec self under `xvfb-run -a` so the `WebKit` window renders to a
/// virtual X server instead of the user's desktop. Used when the
/// parent passes `FERRIDRIVER_WEBKIT_HEADLESS=1`. The fd 3 IPC socket
/// inherits naturally (xvfb-run doesn't close unknown fds), and we
/// set a sentinel env var so the re-exec doesn't recurse.
fn spawn_under_xvfb() -> ExitCode {
  use std::os::unix::process::CommandExt;
  let exe = match std::env::current_exe() {
    Ok(p) => p,
    Err(e) => {
      tracing::error!("current_exe: {e}");
      return ExitCode::from(70);
    },
  };
  let mut cmd = std::process::Command::new("xvfb-run");
  cmd.arg("-a").arg("--server-args=-screen 0 1280x800x24").arg(&exe);
  cmd.env("FERRIDRIVER_WEBKIT_HEADLESS_INNER", "1");
  // `exec` replaces this process — the parent's IPC socket on fd 3
  // is preserved through the exec.
  let err = cmd.exec();
  tracing::error!("xvfb-run exec failed: {err}; is xorg-server-xvfb installed?");
  ExitCode::from(70)
}

/// `"WebKitGTK/<major>.<minor>.<micro>"` — same shape the macOS host
/// returns for `Op::GetWebKitVersion`, so `Browser::version()` surfaces
/// a real product version on Linux too.
pub(crate) fn webkit_version_string() -> String {
  format!(
    "WebKitGTK/{}.{}.{}",
    webkit6::functions::major_version(),
    webkit6::functions::minor_version(),
    webkit6::functions::micro_version()
  )
}

//! A server's life tied to whoever started it.

/// Resolves once standard input reaches its end: the process holding the
/// pipe's other end closed it or exited, however it exited, a kill included.
///
/// The read runs on a thread of its own rather than the runtime's blocking
/// pool: a read of stdin cannot be cancelled, and a pool thread still
/// blocked in one would hold the runtime open after the server stops for
/// some other reason.
pub fn stdin_closed() -> std::io::Result<impl std::future::Future<Output = ()>> {
  let (closed, wait) = tokio::sync::oneshot::channel::<()>();
  std::thread::Builder::new().name("stdin-watch".into()).spawn(move || {
    let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
    let _ = closed.send(());
  })?;
  Ok(async move {
    let _ = wait.await;
  })
}

/// A request to stop that is not a Ctrl-C: `SIGTERM` on Unix; on Windows,
/// which has no such signal, the console closing, the user logging off or
/// the system shutting down.
pub struct Termination {
  #[cfg(unix)]
  term: tokio::signal::unix::Signal,
  #[cfg(windows)]
  close: tokio::signal::windows::CtrlClose,
  #[cfg(windows)]
  logoff: tokio::signal::windows::CtrlLogoff,
  #[cfg(windows)]
  shutdown: tokio::signal::windows::CtrlShutdown,
}

impl Termination {
  /// # Errors
  /// Fails when the handlers cannot be installed.
  pub fn new() -> std::io::Result<Self> {
    Ok(Self {
      #[cfg(unix)]
      term: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
      #[cfg(windows)]
      close: tokio::signal::windows::ctrl_close()?,
      #[cfg(windows)]
      logoff: tokio::signal::windows::ctrl_logoff()?,
      #[cfg(windows)]
      shutdown: tokio::signal::windows::ctrl_shutdown()?,
    })
  }

  /// Resolves on the next request.
  pub async fn recv(&mut self) {
    #[cfg(unix)]
    self.term.recv().await;
    #[cfg(windows)]
    tokio::select! {
      _ = self.close.recv() => {},
      _ = self.logoff.recv() => {},
      _ = self.shutdown.recv() => {},
    }
  }
}

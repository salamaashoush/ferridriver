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

//! Playwright `WebKit` browser handle. Owns the spawned child process,
//! the [`super::Connection`], and the root [`super::BrowserSession`].
//!
//! Mirrors the launch flow of Playwright's `WKBrowser`:
//!
//! 1. Spawn `pw_run.sh --inspector-pipe [--headless] [--user-data-dir=...]`.
//! 2. Wire fd 3 (parent → child input) and fd 4 (child → parent output)
//!    through [`super::Transport`].
//! 3. Send `Playwright.enable` on the root session.
//! 4. Subscribe to `Playwright.pageProxyCreated` so subsequent
//!    `Playwright.createPage` calls return a usable page handle.

use super::connection::{BrowserSession, Connection, ConnectionError, PageProxySession};
use super::launcher::{LaunchConfig, LaunchError};
use super::protocol::{self, CreateContextParams, CreateContextResult, CreatePageParams, CreatePageResult, Envelope};
use super::transport::Transport;
use serde_json::{Value, json};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::process::Child;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::broadcast;

#[derive(Debug, Error)]
pub enum BrowserError {
  #[error("launch: {0}")]
  Launch(#[from] LaunchError),
  #[error("connection: {0}")]
  Connection(#[from] ConnectionError),
  #[error("io: {0}")]
  Io(#[from] std::io::Error),
  #[error("json: {0}")]
  Json(#[from] serde_json::Error),
  #[error("protocol: {0}")]
  Protocol(String),
}

pub struct Browser {
  child: Child,
  /// Parent's halves of the IPC socketpair. Held so the descriptors
  /// stay open for the lifetime of the connection — Rust's
  /// `UnixStream` Drop closes them. [`Self::close`] consumes them
  /// explicitly so EOF reaches the child before we reap it.
  ipc: (UnixStream, UnixStream),
  conn: Arc<Connection>,
  root: BrowserSession,
}

impl Browser {
  /// Spawn a Playwright `WebKit` child + complete the
  /// `Playwright.enable` handshake.
  pub async fn launch(config: &LaunchConfig) -> Result<Self, BrowserError> {
    // Two socketpairs:
    //
    //   pair A — child reads from fd 3 ← parent writes to its end of A.
    //   pair B — child writes to fd 4 → parent reads from its end of B.
    //
    // The naming convention here is "what the OWNER does":
    //   parent_write   — parent writes to this socket (pair A, parent side).
    //   child_read     — child reads from this socket (pair A, child side; dup → fd 3).
    //   child_write    — child writes to this socket (pair B, child side; dup → fd 4).
    //   parent_read    — parent reads from this socket (pair B, parent side).
    //
    // Swapping these two pairs (e.g. handing the child its own read end on
    // fd 4) leaves both processes blocked on `read()` forever, so be
    // precise about which half lives on which side.
    let (parent_write, child_read) = UnixStream::pair()?;
    let (parent_read, child_write) = UnixStream::pair()?;
    let child_read_fd = child_read.as_raw_fd();
    let child_write_fd = child_write.as_raw_fd();
    // launcher::spawn signature: (config, fd_that_becomes_child_fd_4_write_end,
    //                             fd_that_becomes_child_fd_3_read_end).
    // PW's `--inspector-pipe`: child READS from fd 3, WRITES to fd 4.
    let child = super::launcher::spawn(config, child_write_fd, child_read_fd)?;
    // Drop the child halves — they're already dup'd to fd 3/4 in the
    // child. Keeping them open in the parent would prevent EOF
    // propagation when the child exits.
    drop(child_read);
    drop(child_write);

    parent_read.set_nonblocking(false)?;
    parent_write.set_nonblocking(false)?;
    let read_clone = parent_read.try_clone()?;
    let write_clone = parent_write.try_clone()?;
    let transport = Transport::new(read_clone, write_clone);
    let conn = Connection::spawn(transport);
    let browser = conn.browser();
    browser.send(protocol::PLAYWRIGHT_ENABLE, json!({})).await?;

    Ok(Browser {
      child,
      ipc: (parent_read, parent_write),
      conn,
      root: browser,
    })
  }

  #[must_use]
  pub fn root(&self) -> &BrowserSession {
    &self.root
  }

  #[must_use]
  pub fn connection(&self) -> &Arc<Connection> {
    &self.conn
  }

  /// Create an ephemeral browser context. Returns the
  /// `browserContextId` Playwright assigned.
  pub async fn create_context(&self, params: CreateContextParams) -> Result<String, BrowserError> {
    let resp = self
      .root
      .send(protocol::PLAYWRIGHT_CREATE_CONTEXT, serde_json::to_value(&params)?)
      .await?;
    let parsed: CreateContextResult =
      serde_json::from_value(resp).map_err(|e| BrowserError::Protocol(e.to_string()))?;
    Ok(parsed.browser_context_id)
  }

  /// Delete a context previously returned by [`Self::create_context`].
  pub async fn delete_context(&self, browser_context_id: &str) -> Result<(), BrowserError> {
    self
      .root
      .send(
        protocol::PLAYWRIGHT_DELETE_CONTEXT,
        json!({ "browserContextId": browser_context_id }),
      )
      .await?;
    Ok(())
  }

  /// Create a new page in `browser_context_id` (or the default
  /// persistent context when `None`). Returns the `pageProxyId`
  /// + a registered [`PageProxySession`] handle.
  pub async fn create_page(&self, browser_context_id: Option<&str>) -> Result<PageProxySession, BrowserError> {
    let params = CreatePageParams {
      browser_context_id: browser_context_id.map(str::to_string),
    };
    let resp = self
      .root
      .send(protocol::PLAYWRIGHT_CREATE_PAGE, serde_json::to_value(&params)?)
      .await?;
    let parsed: CreatePageResult = serde_json::from_value(resp).map_err(|e| BrowserError::Protocol(e.to_string()))?;
    Ok(self.conn.open_page_proxy(parsed.page_proxy_id))
  }

  /// Subscribe to events on the root browser session. Useful for
  /// `Playwright.pageProxyCreated/Destroyed`, download events, etc.
  #[must_use]
  pub fn events(&self) -> broadcast::Receiver<Envelope> {
    self.root.events()
  }

  /// Issue `Playwright.close` and wait for the child process to exit.
  pub async fn close(mut self) -> Result<(), BrowserError> {
    // `Playwright.close` does not always send a response — Playwright
    // assigns a sentinel id (`kBrowserCloseMessageId = -9999`) the
    // dispatcher silently drops. We mirror that by firing the
    // request without awaiting a callback so we don't deadlock on a
    // reply the child will never deliver. Best-effort: ignore write
    // errors (the child may already have torn down its read end).
    let close_envelope = json!({ "id": -9999, "method": protocol::PLAYWRIGHT_CLOSE, "params": {} });
    let _ = self.conn.send_raw(&close_envelope);
    // Close our pipe fds so the child's reads return EOF and it can
    // tear down cleanly. The `ipc` tuple owns the parent halves of
    // both socketpairs; dropping it closes them.
    drop(self.ipc);
    // Reap the child without blocking forever — if `Playwright.close`
    // didn't make it exit, escalate to SIGKILL after a short timeout
    // so test runs don't wedge.
    for _ in 0..30 {
      if let Ok(Some(_)) = self.child.try_wait() {
        return Ok(());
      }
      tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let _ = self.child.kill();
    let _ = self.child.wait();
    Ok(())
  }

  /// Returns the result of `Playwright.getInfo` (currently
  /// `{ os: "macOS" | "Linux" | "Windows" }`).
  pub async fn info(&self) -> Result<Value, BrowserError> {
    Ok(self.root.send(protocol::PLAYWRIGHT_GET_INFO, json!({})).await?)
  }
}

//! `AndroidWebView`, `AndroidInput`, and `AndroidSocket` handles.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::super::driver::{key_code, key_codes};
use super::{AndroidDevice, DeviceInner};
use crate::error::{FerriError, Result};
use crate::options::Point;

pub(super) struct WebViewInner {
  pub(super) device: std::sync::Weak<DeviceInner>,
  pub(super) pid: i64,
  pub(super) pkg: String,
  pub(super) socket_name: String,
  pub(super) page: tokio::sync::OnceCell<Arc<crate::Page>>,
  /// Set once the device no longer lists the view, or closes.
  pub(super) closed: tokio::sync::watch::Sender<bool>,
}

/// Playwright: `AndroidWebView`, a web view an application on the device
/// shows.
#[derive(Clone)]
pub struct AndroidWebView {
  pub(super) inner: Arc<WebViewInner>,
}

impl PartialEq for AndroidWebView {
  fn eq(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.inner, &other.inner)
  }
}

impl std::fmt::Debug for AndroidWebView {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("AndroidWebView")
      .field("pid", &self.inner.pid)
      .field("pkg", &self.inner.pkg)
      .field("socket_name", &self.inner.socket_name)
      .finish_non_exhaustive()
  }
}

impl AndroidWebView {
  /// Playwright: `androidWebView.pid(): number`. `-1` when the socket's
  /// name carries none.
  #[must_use]
  pub fn pid(&self) -> i64 {
    self.inner.pid
  }

  /// Playwright: `androidWebView.pkg(): string`. Empty when the process
  /// could not be named.
  #[must_use]
  pub fn pkg(&self) -> &str {
    &self.inner.pkg
  }

  /// The name of the view's `DevTools` socket.
  #[must_use]
  pub fn socket_name(&self) -> &str {
    &self.inner.socket_name
  }

  /// Whether the view is gone: Playwright's `close` event has fired.
  #[must_use]
  pub fn is_closed(&self) -> bool {
    *self.inner.closed.borrow()
  }

  /// Resolves when the view is gone, at once if it already is.
  /// Playwright's `close` event of `AndroidWebView`.
  pub async fn closed(&self) {
    let mut closed = self.inner.closed.subscribe();
    let _ = closed.wait_for(|closed| *closed).await;
  }

  /// Playwright: `androidWebView.page(): Promise<Page>`. The page the
  /// view shows, connected to on first use.
  ///
  /// # Errors
  ///
  /// Returns an error if the view or its device is gone, or its
  /// `DevTools` socket cannot be reached.
  pub async fn page(&self) -> Result<Arc<crate::Page>> {
    let page = self
      .inner
      .page
      .get_or_try_init(|| async {
        if self.is_closed() {
          return Err(FerriError::target_closed(Some("WebView has been closed".into())));
        }
        let inner = self
          .inner
          .device
          .upgrade()
          .ok_or_else(|| FerriError::target_closed(Some("Device is closed".into())))?;
        let device = AndroidDevice { inner };
        device.ensure_open()?;
        let server = &device.inner.server;
        let serial = device.inner.serial.as_str();
        let port = server
          .forward(serial, &format!("localabstract:{}", self.inner.socket_name))
          .await?;
        let context = crate::BrowserType::connect_persistent_over_cdp(
          &format!("http://127.0.0.1:{port}"),
          crate::options::BrowserContextOptions {
            viewport: crate::options::ViewportOption::Null,
            ..Default::default()
          },
          None,
        )
        .await;
        let context = match context {
          Ok(context) => context,
          Err(error) => {
            let _ = server.kill_forward(serial, port).await;
            return Err(error);
          },
        };
        device.inner.browsers.lock().await.push((context.clone(), port));
        context
          .pages()
          .into_iter()
          .next()
          .ok_or_else(|| FerriError::backend("the web view has no page"))
      })
      .await?;
    Ok(Arc::clone(page))
  }
}

/// Playwright: `AndroidInput`, reached as `androidDevice.input`.
#[derive(Clone)]
pub struct AndroidInput {
  pub(super) device: AndroidDevice,
}

impl AndroidInput {
  async fn send(&self, method: &str, params: serde_json::Value) -> Result<()> {
    let driver = self.device.driver().await?;
    // Upstream gives the input calls no limit of their own.
    driver.send(method, params, 0).await.map(|_| ())
  }

  /// Playwright: `androidInput.type(text: string)`. A key press per
  /// character.
  ///
  /// # Errors
  ///
  /// Returns an error for a character no `AndroidKey` stands for, and
  /// if the device is closed or the driver cannot start.
  pub async fn type_text(&self, text: &str) -> Result<()> {
    for code in key_codes(text)? {
      self.send("inputPress", serde_json::json!({ "keyCode": code })).await?;
    }
    Ok(())
  }

  /// Playwright: `androidInput.press(key: AndroidKey)`.
  ///
  /// # Errors
  ///
  /// Returns an error for a key that is not an `AndroidKey`, and if the
  /// device is closed or the driver cannot start.
  pub async fn press(&self, key: &str) -> Result<()> {
    let code = key_code(key)?;
    self.send("inputPress", serde_json::json!({ "keyCode": code })).await
  }

  /// Playwright: `androidInput.tap(point: { x, y })`.
  ///
  /// # Errors
  ///
  /// Returns an error if the device is closed or the driver cannot start.
  pub async fn tap(&self, point: Point) -> Result<()> {
    self
      .send(
        "inputClick",
        serde_json::json!({ "point": { "x": point.x, "y": point.y } }),
      )
      .await
  }

  /// Playwright: `androidInput.swipe(from, segments, steps)`. From
  /// `from` through each of `segments`, `steps` moves per segment.
  ///
  /// # Errors
  ///
  /// Returns an error if the device is closed or the driver cannot start.
  pub async fn swipe(&self, from: Point, segments: &[Point], steps: u32) -> Result<()> {
    let path: Vec<serde_json::Value> = std::iter::once(&from)
      .chain(segments)
      .map(|point| serde_json::json!({ "x": point.x, "y": point.y }))
      .collect();
    self
      .send("inputSwipe", serde_json::json!({ "segments": path, "steps": steps }))
      .await
  }

  /// Playwright: `androidInput.drag(from, to, steps)`.
  ///
  /// # Errors
  ///
  /// Returns an error if the device is closed or the driver cannot start.
  pub async fn drag(&self, from: Point, to: Point, steps: u32) -> Result<()> {
    self
      .send(
        "inputDrag",
        serde_json::json!({
          "from": { "x": from.x, "y": from.y },
          "to": { "x": to.x, "y": to.y },
          "steps": steps,
        }),
      )
      .await
  }
}

/// Playwright: `AndroidSocket`, a connection `androidDevice.open` made.
pub struct AndroidSocket {
  pub(super) writer: tokio::sync::Mutex<Option<tokio::net::tcp::OwnedWriteHalf>>,
  pub(super) data: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>>,
  pub(super) reader: tokio::task::AbortHandle,
}

impl AndroidSocket {
  pub(super) fn new(stream: tokio::net::TcpStream) -> Self {
    let (mut read, write) = stream.into_split();
    let (sender, data) = tokio::sync::mpsc::unbounded_channel();
    let reader = tokio::spawn(async move {
      let mut buffer = vec![0u8; 64 * 1024];
      loop {
        match read.read(&mut buffer).await {
          Ok(0) | Err(_) => break,
          Ok(length) => {
            if sender.send(buffer[..length].to_vec()).is_err() {
              break;
            }
          },
        }
      }
    })
    .abort_handle();
    Self {
      writer: tokio::sync::Mutex::new(Some(write)),
      data: tokio::sync::Mutex::new(data),
      reader,
    }
  }

  /// Playwright: `androidSocket.write(data: Buffer): Promise<void>`.
  ///
  /// # Errors
  ///
  /// Returns an error if the socket is closed.
  pub async fn write(&self, data: &[u8]) -> Result<()> {
    let mut writer = self.writer.lock().await;
    let Some(writer) = writer.as_mut() else {
      return Err(FerriError::target_closed(Some("Socket is closed".into())));
    };
    writer.write_all(data).await?;
    Ok(())
  }

  /// The next bytes the device sent: Playwright's `data` event, one
  /// chunk at a time. `None` once the connection has closed, which is
  /// its `close` event.
  pub async fn next_data(&self) -> Option<Vec<u8>> {
    self.data.lock().await.recv().await
  }

  /// Playwright: `androidSocket.close(): Promise<void>`.
  pub async fn close(&self) {
    self.writer.lock().await.take();
    self.reader.abort();
    self.data.lock().await.close();
  }
}

impl Drop for AndroidSocket {
  fn drop(&mut self) {
    self.reader.abort();
  }
}

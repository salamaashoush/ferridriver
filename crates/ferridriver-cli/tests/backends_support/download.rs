//! Rule-9 integration tests for `Download` as a first-class event
//! handle accessible via `page.waitForEvent('download')`.
//!
//! Per-backend expectations:
//! * cdp-pipe / cdp-raw — full round-trip through
//!   `Browser.downloadWillBegin` + `Browser.downloadProgress`, CDP
//!   writes the file to our per-page temp dir, `saveAs` copies the
//!   bytes byte-for-byte, `cancel` surfaces as `failure() === 'canceled'`.
//! * bidi — full round-trip through the BiDi download events
//!   (`browsingContext.downloadWillBegin`, `browsingContext.downloadEnd`).
//!   Firefox reports the absolute `filepath` on completion so
//!   `path()` resolves to the real file. `cancel()` returns typed
//!   `Unsupported` — Firefox's BiDi has no cancel command and
//!   Playwright's own BiDi backend leaves `cancelDownload` as a no-op.
//! * webkit — stock `WKWebView` routes downloads through
//!   `WKDownloadDelegate` in the host's Obj-C subprocess and our IPC
//!   does not yet carry those events. `waitForEvent('download')` times
//!   out, matching the documented backend gap.

#![allow(
  clippy::too_many_lines,
  clippy::doc_markdown,
  clippy::uninlined_format_args,
  clippy::unwrap_used,
  clippy::expect_used,
  clippy::needless_pass_by_value
)]

use super::client::McpClient;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

// ── Stub HTTP server serving a Content-Disposition: attachment payload ──
//
// Browsers treat a response with `Content-Disposition: attachment` as
// the canonical trigger for a user-visible download. `<a download>` on
// a `data:` URL also works, but behaviour varies per backend (Chrome
// auto-names it `download`; Firefox respects the `download` attribute
// differently across versions). Using an explicit HTTP response keeps
// the test stable across CDP and BiDi.

/// Bring up a stub HTTP server that serves a single path, hand control
/// to `body`, and tear the server down afterwards.
///
/// `GET /file.bin` → 200 with `Content-Disposition: attachment;
/// filename="greeting.txt"` and the exact bytes of `payload`.
/// Every other path → 200 `text/html` serving an anchor that
/// triggers the download — clicking `#dl` navigates to `/file.bin`
/// which then becomes the download.
fn with_download_server<F: FnOnce(&str, &[u8])>(payload: &[u8], body: F) {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
  let addr = listener.local_addr().expect("addr");
  let base = format!("http://{addr}");
  let stop = Arc::new(AtomicBool::new(false));
  let stop_clone = stop.clone();
  let payload_for_thread = payload.to_vec();

  let handle = thread::spawn(move || {
    listener.set_nonblocking(true).expect("listener nonblocking");
    while !stop_clone.load(Ordering::Acquire) {
      match listener.accept() {
        Ok((stream, _)) => {
          let bytes = payload_for_thread.clone();
          thread::spawn(move || handle_download_conn(stream, &bytes));
        },
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
          thread::sleep(std::time::Duration::from_millis(10));
        },
        Err(_) => break,
      }
    }
  });

  body(&base, payload);

  stop.store(true, Ordering::Release);
  let _ = TcpStream::connect(addr);
  let _ = handle.join();
}

fn handle_download_conn(mut stream: TcpStream, payload: &[u8]) {
  let mut buf = [0u8; 4096];
  let Ok(n) = stream.read(&mut buf) else { return };
  let request = String::from_utf8_lossy(&buf[..n]);
  let mut lines = request.lines();
  let request_line = lines.next().unwrap_or("");
  let mut parts = request_line.split_whitespace();
  let _method = parts.next().unwrap_or("GET");
  let path = parts.next().unwrap_or("/");

  let response = if path.starts_with("/file.bin") {
    // Attachment response — triggers download in Chrome and Firefox.
    let mut out = format!(
      "HTTP/1.1 200 OK\r\n\
       Content-Type: application/octet-stream\r\n\
       Content-Disposition: attachment; filename=\"greeting.txt\"\r\n\
       Content-Length: {}\r\n\
       Connection: close\r\n\r\n",
      payload.len()
    )
    .into_bytes();
    out.extend_from_slice(payload);
    out
  } else if path.starts_with("/hang.bin") {
    // Attachment that never reaches Content-Length: send headers, then
    // dribble bytes until the client tears the socket down (which
    // Chrome does on `download.cancel()`). Keeps the download
    // deterministically in-flight so the cancel-vs-complete race always
    // resolves as `canceled` regardless of runner speed.
    let headers = "HTTP/1.1 200 OK\r\n\
       Content-Type: application/octet-stream\r\n\
       Content-Disposition: attachment; filename=\"greeting.txt\"\r\n\
       Content-Length: 1048576\r\n\
       Connection: close\r\n\r\n";
    if stream.write_all(headers.as_bytes()).is_err() {
      return;
    }
    let _ = stream.flush();
    let chunk = [0u8; 1024];
    // ~30 s safety cap; the test cancels within milliseconds, so the
    // loop exits via the write error long before this.
    for _ in 0..600 {
      if stream.write_all(&chunk).is_err() {
        return;
      }
      let _ = stream.flush();
      thread::sleep(std::time::Duration::from_millis(50));
    }
    return;
  } else {
    // Landing page with anchors that navigate to the download paths.
    // Clicking an anchor causes a new top-level navigation whose
    // response is the attachment — Chrome and Firefox both treat the
    // navigation as a download and fire the protocol download-begin
    // event. `#dl` completes immediately; `#dlhang` stays in-flight.
    let html = "<!doctype html><html><body>\
      <a id=\"dl\" href=\"/file.bin\">download</a>\
      <a id=\"dlhang\" href=\"/hang.bin\">download-hang</a>\
      </body></html>";
    let mut out = format!(
      "HTTP/1.1 200 OK\r\n\
       Content-Type: text/html\r\n\
       Content-Length: {}\r\n\
       Connection: close\r\n\r\n",
      html.len()
    )
    .into_bytes();
    out.extend_from_slice(html.as_bytes());
    out
  };
  let _ = stream.write_all(&response);
  let _ = stream.flush();
}

// ── Tests ──────────────────────────────────────────────────────────────

/// `waitForEvent('download')` on WebKit must surface a timeout — the
/// stock `WKWebView` download delegate runs in the Obj-C host and no
/// event flows through our IPC. Matches Rule 4 honesty.
pub fn test_download_webkit_unsupported(c: &mut McpClient) {
  if c.backend != "webkit" {
    return;
  }
  with_download_server(b"hello-webkit", |base, _| {
    c.nav_url(base);
    let script = r##"
      const started = Date.now();
      let threw = false;
      let message = "";
      try {
        const p = page.waitForEvent("download", 800);
        const clickPromise = page.click("#dl").catch(() => {});
        await Promise.all([p, clickPromise]);
      } catch (e) {
        threw = true;
        message = String(e && e.message || e);
      }
      return { threw, message, elapsed_ms: Date.now() - started };
    "##;
    let v = c.script_value(script);
    assert_eq!(
      v["threw"].as_bool(),
      Some(true),
      "webkit should surface a timeout for downloads: {v}"
    );
    let msg = v["message"].as_str().unwrap_or("");
    assert!(
      msg.contains("Timeout") || msg.contains("timeout") || msg.contains("unsupported"),
      "webkit download error should mention timeout/unsupported, got: {msg}"
    );
  });
}

/// Trigger a download, capture via `waitForEvent('download')`, call
/// `saveAs(tmpPath)`, read the saved file, assert the bytes match the
/// payload byte-for-byte. Runs on cdp-pipe, cdp-raw, bidi.
pub fn test_download_save_as_roundtrip(c: &mut McpClient) {
  if c.backend == "webkit" {
    return;
  }
  let payload = b"hello download world";
  with_download_server(payload, |base, _| {
    c.nav_url(base);
    let save_path = std::env::temp_dir().join(format!(
      "ferridriver-dl-save-{}-{}.bin",
      std::process::id(),
      backend_suffix(&c.backend),
    ));
    // Remove any leftover from a prior run.
    let _ = std::fs::remove_file(&save_path);
    let save_str = save_path.display().to_string();
    let script = format!(
      r##"
      const p = page.waitForEvent("download", 15000);
      await page.click("#dl");
      const dl = await p;
      const url = dl.url();
      const suggested = dl.suggestedFilename();
      await dl.saveAs({save_str});
      return {{ url, suggested }};
    "##,
      save_str = serde_json::to_string(&save_str).unwrap(),
    );
    let v = c.script_value(&script);
    let url = v["url"].as_str().unwrap_or_default();
    assert!(
      url.contains("/file.bin"),
      "download.url() should expose the download URL: {v}"
    );
    assert_eq!(
      v["suggested"].as_str(),
      Some("greeting.txt"),
      "suggestedFilename should reflect Content-Disposition filename: {v}"
    );
    let saved_bytes = std::fs::read(&save_path).expect("read saved file");
    assert_eq!(
      saved_bytes.as_slice(),
      payload,
      "saveAs bytes must match the served payload byte-for-byte (len saved={}, expected={})",
      saved_bytes.len(),
      payload.len(),
    );
    let _ = std::fs::remove_file(&save_path);
  });
}

/// `download.path()` resolves to the backend-written file and its
/// contents match the payload. Exercises the `wait_finished` +
/// `report_finished` watch transition end-to-end.
pub fn test_download_path_contents(c: &mut McpClient) {
  if c.backend == "webkit" {
    return;
  }
  let payload = b"payload-for-path";
  with_download_server(payload, |base, _| {
    c.nav_url(base);
    let script = r##"
      const p = page.waitForEvent("download", 15000);
      await page.click("#dl");
      const dl = await p;
      const path = await dl.path();
      return { path };
    "##;
    let v = c.script_value(script);
    let path_str = v["path"].as_str().unwrap_or_default();
    assert!(
      !path_str.is_empty(),
      "download.path() should resolve to the written file: {v}"
    );
    let path = std::path::PathBuf::from(path_str);
    let disk_bytes = std::fs::read(&path).expect("read download path");
    assert_eq!(
      disk_bytes.as_slice(),
      payload,
      "file at download.path() must contain the served bytes (len={}, expected={})",
      disk_bytes.len(),
      payload.len(),
    );
  });
}

/// `cancel()` on CDP backends surfaces as `failure() === 'canceled'`
/// (matches Playwright's `crBrowser.ts::cancelDownload` ->
/// `downloadFinished(guid, 'canceled')` path byte-for-byte). CDP only
/// — BiDi's cancel path is exercised by
/// [`test_download_cancel_bidi_unsupported`] because Firefox's BiDi
/// has no cancel primitive (Playwright's own BiDi backend leaves
/// `cancelDownload` as a no-op). We can't conflate the two tests
/// because `await failure()` on BiDi would block indefinitely — the
/// download never reaches a terminal state without a working cancel.
pub fn test_download_cancel_surfaces_failure(c: &mut McpClient) {
  if c.backend != "cdp_pipe" && c.backend != "cdp_raw" && c.backend != "cdp-pipe" && c.backend != "cdp-raw" {
    return;
  }
  let payload = b"bytes-that-may-get-truncated";
  with_download_server(payload, |base, _| {
    c.nav_url(base);
    // `#dlhang` serves an attachment that never finishes, so the
    // download is deterministically still in-flight when cancel()
    // fires — no cancel-vs-complete race on slow CI runners.
    let script = r##"
      const p = page.waitForEvent("download", 15000);
      await page.click("#dlhang");
      const dl = await p;
      await dl.cancel();
      const failure = await dl.failure();
      return { failure };
    "##;
    let v = c.script_value(script);
    assert_eq!(
      v["failure"].as_str(),
      Some("canceled"),
      "CDP download.failure() after cancel should equal 'canceled': {v}"
    );
  });
}

/// BiDi cancel is typed `Unsupported` (Rule 4). Playwright's own BiDi
/// backend at `bidiBrowser.ts::cancelDownload` is an empty async — we
/// surface the gap via a typed error instead of a silent no-op so
/// callers can dispatch on `error.name === 'FerriError'`.
///
/// We DON'T call `failure()` here because the download never reaches a
/// terminal state on BiDi without a working cancel and the await
/// would block past the test timeout. The cancel throw is the entire
/// observable surface of the gap.
pub fn test_download_cancel_bidi_unsupported(c: &mut McpClient) {
  if c.backend != "bidi" {
    return;
  }
  let payload = b"bidi-bytes";
  with_download_server(payload, |base, _| {
    c.nav_url(base);
    let script = r##"
      const p = page.waitForEvent("download", 15000);
      await page.click("#dl");
      const dl = await p;
      let cancelThrew = false;
      let cancelMessage = "";
      try {
        await dl.cancel();
      } catch (e) {
        cancelThrew = true;
        cancelMessage = String(e && e.message || e);
      }
      return { cancelThrew, cancelMessage };
    "##;
    let v = c.script_value(script);
    assert_eq!(
      v["cancelThrew"].as_bool(),
      Some(true),
      "bidi cancel should surface typed Unsupported: {v}"
    );
    let msg = v["cancelMessage"].as_str().unwrap_or("");
    assert!(
      msg.contains("unsupported") || msg.contains("Unsupported") || msg.contains("BiDi"),
      "bidi cancel error should mention Unsupported/BiDi, got: {msg}"
    );
  });
}

/// Per-backend session cleanup isolator: make the temp file name
/// distinct so concurrent test threads don't fight over disk state.
fn backend_suffix(backend: &str) -> &str {
  match backend {
    "cdp_pipe" | "cdp-pipe" => "cdppipe",
    "cdp_raw" | "cdp-raw" => "cdpraw",
    "bidi" => "bidi",
    "webkit" => "webkit",
    _ => "other",
  }
}

//! An ADB server with one device and no Android behind it, for the tests
//! of the device API: it speaks enough of the wire protocol to list the
//! device, run a shell command, take a push and install a package.
//!
//! The shell is scripted. `getprop ro.product.model` names the model,
//! `screencap -p` answers a PNG signature, `cat <path>` returns what was
//! pushed there with its mode, and anything else is echoed back. A
//! package whose bytes start with `BAD` is refused.
//!
//! It also plays Playwright's Android driver on the abstract socket
//! `playwright_android_driver_socket`: every call is recorded and
//! answered, `cat driver-calls` returns the record as a JSON array, a
//! selector whose text is `^Missing$` is not found, and `info` describes
//! one button.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const SERIAL: &str = "emulator-5554";
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

type Files = Arc<Mutex<HashMap<String, (u32, Vec<u8>)>>>;

/// Where the driver's calls are recorded, among the pushed files.
const DRIVER_CALLS: &str = "driver-calls";

/// Playwright's driver: four bytes of big-endian length and a JSON
/// `{ id, method, params }`, answered `{ id, result }` or `{ id, error }`.
async fn driver(stream: &mut TcpStream, files: &Files) -> Result<()> {
  loop {
    let mut length = [0u8; 4];
    if stream.read_exact(&mut length).await.is_err() {
      return Ok(());
    }
    let mut message = vec![0u8; u32::from_be_bytes(length) as usize];
    stream.read_exact(&mut message).await?;
    let call: serde_json::Value = serde_json::from_slice(&message)?;
    {
      let mut files = files.lock().map_err(|error| anyhow::anyhow!(error.to_string()))?;
      let (_, recorded) = files.entry(DRIVER_CALLS.into()).or_insert_with(|| (0, b"[]".to_vec()));
      let mut calls: Vec<serde_json::Value> = serde_json::from_slice(recorded)?;
      calls.push(serde_json::json!({ "method": call["method"], "params": call["params"] }));
      *recorded = serde_json::to_vec(&calls)?;
    }
    let answer = if call["params"]["selector"]["text"] == "^Missing$" {
      serde_json::json!({ "id": call["id"], "error": "Timeout waiting for selector" })
    } else if call["method"] == "info" {
      serde_json::json!({ "id": call["id"], "result": {
        "clazz": "android.widget.Button", "desc": null, "res": "com.example:id/save", "pkg": "com.example",
        "text": "Save", "bounds": { "x": 10, "y": 20, "width": 300, "height": 120 },
        "checkable": false, "checked": false, "clickable": true, "enabled": true, "focusable": true,
        "focused": false, "longClickable": false, "scrollable": false, "selected": false,
      } })
    } else {
      serde_json::json!({ "id": call["id"], "result": null })
    };
    let body = serde_json::to_vec(&answer)?;
    stream.write_all(&u32::try_from(body.len())?.to_be_bytes()).await?;
    stream.write_all(&body).await?;
  }
}

async fn request(stream: &mut TcpStream) -> Result<String> {
  let mut digits = [0u8; 4];
  stream.read_exact(&mut digits).await?;
  let length = usize::from_str_radix(std::str::from_utf8(&digits)?, 16)?;
  let mut message = vec![0u8; length];
  stream.read_exact(&mut message).await?;
  Ok(String::from_utf8(message)?)
}

async fn fail(stream: &mut TcpStream, reason: &str) -> Result<()> {
  stream
    .write_all(format!("FAIL{:04x}{reason}", reason.len()).as_bytes())
    .await?;
  Ok(())
}

async fn sync(stream: &mut TcpStream, files: &Files) -> Result<()> {
  let mut target = String::new();
  let mut content = Vec::new();
  loop {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header).await?;
    let value = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    match &header[..4] {
      b"SEND" => {
        let mut name = vec![0u8; value as usize];
        stream.read_exact(&mut name).await?;
        target = String::from_utf8(name)?;
      },
      b"DATA" => {
        let mut chunk = vec![0u8; value as usize];
        stream.read_exact(&mut chunk).await?;
        content.extend(chunk);
      },
      b"DONE" => break,
      other => anyhow::bail!("unexpected sync id {other:?}"),
    }
  }
  let (path, mode) = target.rsplit_once(',').context("SEND names a path and a mode")?;
  if path.starts_with("/system/") {
    let reason = b"Read-only file system";
    stream.write_all(b"FAIL").await?;
    stream.write_all(&u32::try_from(reason.len())?.to_le_bytes()).await?;
    stream.write_all(reason).await?;
    return Ok(());
  }
  files
    .lock()
    .map_err(|error| anyhow::anyhow!(error.to_string()))?
    .insert(path.to_owned(), (mode.parse()?, content));
  stream.write_all(b"OKAY\0\0\0\0").await?;
  Ok(())
}

async fn shell(stream: &mut TcpStream, command: &str, files: &Files) -> Result<()> {
  if command == "getprop ro.product.model" {
    stream.write_all(b"Fixture Phone\n").await?;
  } else if command == "screencap -p" {
    stream.write_all(PNG).await?;
  } else if command.starts_with("am instrument -w ") {
    // The driver's shell stays open for as long as the driver runs.
    let mut closed = [0u8; 1];
    let _ = stream.read(&mut closed).await;
  } else if command == format!("cat {DRIVER_CALLS}") {
    let recorded = files
      .lock()
      .map_err(|error| anyhow::anyhow!(error.to_string()))?
      .get(DRIVER_CALLS)
      .map_or_else(|| b"[]".to_vec(), |(_, calls)| calls.clone());
    stream.write_all(&recorded).await?;
  } else if let Some(path) = command.strip_prefix("cat ") {
    let stored = files
      .lock()
      .map_err(|error| anyhow::anyhow!(error.to_string()))?
      .get(path)
      .cloned();
    match stored {
      Some((mode, content)) => {
        stream.write_all(format!("{mode:o}:").as_bytes()).await?;
        stream.write_all(&content).await?;
      },
      None => {
        stream
          .write_all(format!("cat: {path}: No such file or directory\n").as_bytes())
          .await?;
      },
    }
  } else if let Some(arguments) = command.strip_prefix("cmd package install ") {
    let (arguments, length) = arguments.rsplit_once(' ').context("install names a length")?;
    let mut package = vec![0u8; length.parse()?];
    stream.read_exact(&mut package).await?;
    if package.starts_with(b"BAD") {
      stream.write_all(b"Failure [INSTALL_PARSE_FAILED_NOT_APK]\n").await?;
    } else {
      files
        .lock()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?
        .insert(
          "installed".into(),
          (0, format!("{arguments}:{}", package.len()).into_bytes()),
        );
      stream.write_all(b"Success\n").await?;
    }
  } else {
    stream.write_all(command.as_bytes()).await?;
  }
  Ok(())
}

async fn serve(mut stream: TcpStream, files: Files) -> Result<()> {
  let first = request(&mut stream).await?;
  if first == "host:devices" {
    let listing = format!("{SERIAL}\tdevice\nR58M123ABC\tunauthorized\n");
    stream
      .write_all(format!("OKAY{:04x}{listing}", listing.len()).as_bytes())
      .await?;
    return Ok(());
  }
  let Some(serial) = first.strip_prefix("host:transport:") else {
    return fail(&mut stream, "unknown host service").await;
  };
  if serial != SERIAL {
    return fail(&mut stream, &format!("device '{serial}' not found")).await;
  }
  stream.write_all(b"OKAY").await?;
  let service = request(&mut stream).await?;
  stream.write_all(b"OKAY").await?;
  if service == "sync:" {
    sync(&mut stream, &files).await
  } else if let Some(command) = service.strip_prefix("shell:") {
    shell(&mut stream, command, &files).await
  } else if service == "localabstract:playwright_android_driver_socket" {
    driver(&mut stream, &files).await
  } else {
    Ok(())
  }
}

#[tokio::main]
async fn main() -> Result<()> {
  let listener = TcpListener::bind("127.0.0.1:0").await?;
  let mut output = tokio::io::stdout();
  output
    .write_all(format!("listening {}\n", listener.local_addr()?.port()).as_bytes())
    .await?;
  output.flush().await?;
  let files = Files::default();
  loop {
    let (stream, _) = listener.accept().await?;
    let files = Arc::clone(&files);
    tokio::spawn(async move {
      if let Err(error) = serve(stream, files).await {
        eprintln!("adb fixture: {error}");
      }
    });
  }
}

//! An ADB server with one device and no Android behind it, for the tests
//! of the device API: it speaks enough of the wire protocol to list the
//! device, run a shell command, take a push and install a package.
//!
//! The shell is scripted. `getprop ro.product.model` names the model,
//! `screencap -p` answers a PNG signature, `cat <path>` returns what was
//! pushed there with its mode, and anything else is echoed back. A
//! package whose bytes start with `BAD` is refused.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const SERIAL: &str = "emulator-5554";
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

type Files = Arc<Mutex<HashMap<String, (u32, Vec<u8>)>>>;

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

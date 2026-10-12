//! The ADB server's wire protocol, spoken directly rather than through
//! the `adb` binary: Playwright's `server/android/backendAdb.ts`.
//!
//! A request is four hex digits of length and the message. The server
//! answers `OKAY` or `FAIL`, a `FAIL` carrying a length-prefixed reason.
//! A request for a device is preceded on the same connection by
//! `host:transport:<serial>`, after which the connection belongs to the
//! service asked for: a shell's output until it closes, the binary
//! `sync:` file protocol, or a socket on the device.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::{FerriError, Result};

/// The largest `DATA` chunk the `sync:` service accepts.
const SYNC_CHUNK: usize = 65_535;

/// Where an ADB server listens. Playwright's `android.devices({ host, port })`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AdbServer {
  pub host: String,
  pub port: u16,
}

impl Default for AdbServer {
  fn default() -> Self {
    Self {
      host: "127.0.0.1".into(),
      port: 5037,
    }
  }
}

/// One line of `host:devices`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceEntry {
  pub serial: String,
  /// `device` when it is usable; `offline`, `unauthorized` and the rest
  /// when it is not.
  pub state: String,
}

fn failure(context: &str, reason: impl std::fmt::Display) -> FerriError {
  FerriError::protocol("ADB", format!("{context}: {reason}"))
}

impl AdbServer {
  async fn dial(&self) -> Result<TcpStream> {
    TcpStream::connect((self.host.as_str(), self.port))
      .await
      .map_err(|error| {
        FerriError::backend(format!(
          "no ADB server at {}:{} ({error}); start one with `adb start-server`",
          self.host, self.port
        ))
      })
  }

  /// Send one request and read its status.
  async fn request(stream: &mut TcpStream, message: &str) -> Result<()> {
    let length = u16::try_from(message.len()).map_err(|_| failure(message, "request longer than 65535 bytes"))?;
    stream.write_all(format!("{length:04x}").as_bytes()).await?;
    stream.write_all(message.as_bytes()).await?;
    let mut status = [0u8; 4];
    stream.read_exact(&mut status).await?;
    match &status {
      b"OKAY" => Ok(()),
      b"FAIL" => {
        let reason = Self::read_block(stream).await?;
        Err(failure(message, String::from_utf8_lossy(&reason)))
      },
      other => Err(failure(
        message,
        format!("unexpected status {:?}", String::from_utf8_lossy(other)),
      )),
    }
  }

  /// Read four hex digits of length and that many bytes.
  async fn read_block(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut digits = [0u8; 4];
    stream.read_exact(&mut digits).await?;
    let length = std::str::from_utf8(&digits)
      .ok()
      .and_then(|digits| usize::from_str_radix(digits, 16).ok())
      .ok_or_else(|| failure("reply", "length is not four hex digits"))?;
    let mut block = vec![0u8; length];
    stream.read_exact(&mut block).await?;
    Ok(block)
  }

  /// A connection handed to `service` on the device `serial`.
  pub async fn open(&self, serial: &str, service: &str) -> Result<TcpStream> {
    let mut stream = self.dial().await?;
    Self::request(&mut stream, &format!("host:transport:{serial}")).await?;
    Self::request(&mut stream, service).await?;
    Ok(stream)
  }

  /// The devices the server knows, usable or not.
  pub async fn devices(&self) -> Result<Vec<DeviceEntry>> {
    let mut stream = self.dial().await?;
    Self::request(&mut stream, "host:devices").await?;
    let listing = Self::read_block(&mut stream).await?;
    Ok(
      String::from_utf8_lossy(&listing)
        .lines()
        .filter_map(|line| {
          let (serial, state) = line.trim().split_once('\t')?;
          Some(DeviceEntry {
            serial: serial.to_owned(),
            state: state.trim().to_owned(),
          })
        })
        .collect(),
    )
  }

  /// Run `command` in the device's shell and return everything it wrote,
  /// byte for byte: a screenshot comes back through here.
  pub async fn shell(&self, serial: &str, command: &str) -> Result<Vec<u8>> {
    let mut stream = self.open(serial, &format!("shell:{command}")).await?;
    let mut output = Vec::new();
    stream.read_to_end(&mut output).await?;
    Ok(output)
  }

  /// Write `content` to `path` on the device with `mode`, through the
  /// `sync:` service.
  pub async fn push(&self, serial: &str, content: &[u8], path: &str, mode: u32) -> Result<()> {
    async fn send(stream: &mut TcpStream, id: &[u8; 4], data: &[u8]) -> Result<()> {
      let length = u32::try_from(data.len()).map_err(|_| failure("push", "chunk longer than 4 GiB"))?;
      stream.write_all(id).await?;
      stream.write_all(&length.to_le_bytes()).await?;
      stream.write_all(data).await?;
      Ok(())
    }

    let mut stream = self.open(serial, "sync:").await?;
    send(&mut stream, b"SEND", format!("{path},{mode}").as_bytes()).await?;
    for chunk in content.chunks(SYNC_CHUNK) {
      send(&mut stream, b"DATA", chunk).await?;
    }
    let modified = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map_or(0, |since| u32::try_from(since.as_secs()).unwrap_or(u32::MAX));
    stream.write_all(b"DONE").await?;
    stream.write_all(&modified.to_le_bytes()).await?;

    let mut reply = [0u8; 8];
    stream.read_exact(&mut reply).await?;
    if &reply[..4] == b"OKAY" {
      return Ok(());
    }
    let length = u32::from_le_bytes([reply[4], reply[5], reply[6], reply[7]]) as usize;
    let mut reason = vec![0u8; length];
    stream.read_exact(&mut reason).await?;
    Err(failure(&format!("push to {path}"), String::from_utf8_lossy(&reason)))
  }

  /// Install an APK by streaming it to the package manager, with the
  /// arguments Playwright's `installApk` passes when given none.
  pub async fn install(&self, serial: &str, apk: &[u8], args: &[String]) -> Result<()> {
    let args = if args.is_empty() {
      "-r -t -S".to_owned()
    } else {
      args.join(" ")
    };
    let mut stream = self
      .open(serial, &format!("shell:cmd package install {args} {}", apk.len()))
      .await?;
    stream.write_all(apk).await?;
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).await?;
    let answer = String::from_utf8_lossy(&answer);
    if answer.trim_start().starts_with("Success") {
      Ok(())
    } else {
      Err(failure("install", answer.trim()))
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use tokio::net::TcpListener;

  /// A server that plays one connection: it expects each request in
  /// turn, answers it, then hands the connection to `service`.
  async fn server<F, Fut>(
    requests: Vec<(&'static str, &'static [u8])>,
    service: F,
  ) -> (AdbServer, tokio::task::JoinHandle<Vec<u8>>)
  where
    F: FnOnce(TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = Vec<u8>> + Send,
  {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let played = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      for (expected, answer) in requests {
        let mut digits = [0u8; 4];
        stream.read_exact(&mut digits).await.unwrap();
        let length = usize::from_str_radix(std::str::from_utf8(&digits).unwrap(), 16).unwrap();
        let mut message = vec![0u8; length];
        stream.read_exact(&mut message).await.unwrap();
        assert_eq!(String::from_utf8(message).unwrap(), expected);
        stream.write_all(answer).await.unwrap();
      }
      service(stream).await
    });
    (
      AdbServer {
        host: "127.0.0.1".into(),
        port,
      },
      played,
    )
  }

  async fn nothing(_stream: TcpStream) -> Vec<u8> {
    Vec::new()
  }

  #[tokio::test]
  async fn devices_lists_every_serial_with_its_state() {
    let (adb, played) = server(
      vec![("host:devices", b"OKAY0027emulator-5554\tdevice\nR58M123ABC\toffline\n")],
      nothing,
    )
    .await;
    assert_eq!(
      adb.devices().await.unwrap(),
      [
        DeviceEntry {
          serial: "emulator-5554".into(),
          state: "device".into()
        },
        DeviceEntry {
          serial: "R58M123ABC".into(),
          state: "offline".into()
        },
      ]
    );
    played.await.unwrap();
  }

  #[tokio::test]
  async fn shell_returns_the_output_byte_for_byte() {
    let (adb, played) = server(
      vec![
        ("host:transport:emulator-5554", b"OKAY"),
        ("shell:screencap -p", b"OKAY"),
      ],
      |mut stream| async move {
        stream
          .write_all(&[0x89, b'P', b'N', b'G', 0, 255, b'\r', b'\n'])
          .await
          .unwrap();
        Vec::new()
      },
    )
    .await;
    assert_eq!(
      adb.shell("emulator-5554", "screencap -p").await.unwrap(),
      [0x89, b'P', b'N', b'G', 0, 255, b'\r', b'\n']
    );
    played.await.unwrap();
  }

  #[tokio::test]
  async fn a_refused_request_carries_the_servers_reason() {
    let (adb, played) = server(
      vec![("host:transport:gone", b"FAIL0017device 'gone' not found")],
      nothing,
    )
    .await;
    let error = adb.shell("gone", "true").await.unwrap_err().to_string();
    assert!(error.contains("device 'gone' not found"), "{error}");
    played.await.unwrap();
  }

  #[tokio::test]
  async fn a_missing_server_says_where_it_looked() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let adb = AdbServer {
      host: "127.0.0.1".into(),
      port,
    };
    let error = adb.devices().await.unwrap_err().to_string();
    assert!(error.contains(&format!("no ADB server at 127.0.0.1:{port}")), "{error}");
  }

  #[tokio::test]
  async fn push_sends_the_file_in_chunks_the_sync_service_accepts() {
    let content: Vec<u8> = (0..70_000u32).map(|byte| (byte % 251) as u8).collect();
    let expected = content.clone();
    let (adb, played) = server(
      vec![("host:transport:emulator-5554", b"OKAY"), ("sync:", b"OKAY")],
      |mut stream| async move {
        let mut received = Vec::new();
        let mut sizes = Vec::new();
        loop {
          let mut header = [0u8; 8];
          stream.read_exact(&mut header).await.unwrap();
          let value = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
          match &header[..4] {
            b"SEND" => {
              let mut target = vec![0u8; value as usize];
              stream.read_exact(&mut target).await.unwrap();
              assert_eq!(String::from_utf8(target).unwrap(), "/data/local/tmp/note.bin,420");
            },
            b"DATA" => {
              let mut chunk = vec![0u8; value as usize];
              stream.read_exact(&mut chunk).await.unwrap();
              sizes.push(chunk.len());
              received.extend(chunk);
            },
            b"DONE" => {
              assert!(value > 1_600_000_000, "DONE carries a modification time, got {value}");
              break;
            },
            other => panic!("unexpected sync id {other:?}"),
          }
        }
        assert_eq!(sizes, [65_535, 4_465]);
        stream.write_all(b"OKAY\0\0\0\0").await.unwrap();
        received
      },
    )
    .await;
    adb
      .push("emulator-5554", &content, "/data/local/tmp/note.bin", 0o644)
      .await
      .unwrap();
    assert_eq!(played.await.unwrap(), expected);
  }

  #[tokio::test]
  async fn a_refused_push_carries_the_devices_reason() {
    let (adb, played) = server(
      vec![("host:transport:emulator-5554", b"OKAY"), ("sync:", b"OKAY")],
      |mut stream| async move {
        let mut sent = vec![0u8; 8 + "/system/x,420".len() + 8 + 1 + 8];
        stream.read_exact(&mut sent).await.unwrap();
        stream.write_all(b"FAIL\x11\0\0\0Permission denied").await.unwrap();
        Vec::new()
      },
    )
    .await;
    let error = adb
      .push("emulator-5554", b"x", "/system/x", 0o644)
      .await
      .unwrap_err()
      .to_string();
    assert!(error.contains("push to /system/x: Permission denied"), "{error}");
    played.await.unwrap();
  }

  #[tokio::test]
  async fn install_streams_the_apk_and_reads_the_verdict() {
    for (verdict, installed) in [
      ("Success\n", true),
      ("Failure [INSTALL_FAILED_INVALID_APK: no manifest]\n", false),
    ] {
      let (adb, played) = server(
        vec![
          ("host:transport:emulator-5554", b"OKAY"),
          ("shell:cmd package install -r -t -S 5", b"OKAY"),
        ],
        move |mut stream| async move {
          let mut apk = [0u8; 5];
          stream.read_exact(&mut apk).await.unwrap();
          stream.write_all(verdict.as_bytes()).await.unwrap();
          apk.to_vec()
        },
      )
      .await;
      let result = adb.install("emulator-5554", b"APK\0\x01", &[]).await;
      assert_eq!(played.await.unwrap(), b"APK\0\x01");
      match result {
        Ok(()) => assert!(installed),
        Err(error) => {
          assert!(!installed, "{error}");
          assert!(error.to_string().contains("INSTALL_FAILED_INVALID_APK"), "{error}");
        },
      }
    }
  }

  #[tokio::test]
  async fn install_passes_the_callers_arguments_in_place_of_the_defaults() {
    let (adb, played) = server(
      vec![
        ("host:transport:emulator-5554", b"OKAY"),
        ("shell:cmd package install -g --user 0 1", b"OKAY"),
      ],
      |mut stream| async move {
        let mut apk = [0u8; 1];
        stream.read_exact(&mut apk).await.unwrap();
        stream.write_all(b"Success\n").await.unwrap();
        Vec::new()
      },
    )
    .await;
    adb
      .install("emulator-5554", b"A", &["-g".into(), "--user".into(), "0".into()])
      .await
      .unwrap();
    played.await.unwrap();
  }
}

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;

use super::AndroidSdk;
use crate::backend::process::{ChildGroup, drain_child_stderr, output};
use crate::error::{FerriError, Result};

pub(crate) struct AdbEndpoint {
  pub port: u16,
  pub home: PathBuf,
}

pub(super) async fn start_server(
  sdk: &mut AndroidSdk,
  user_home: &Path,
  root: &Path,
  owner: &super::AndroidOwner,
) -> Result<()> {
  #[cfg(not(unix))]
  {
    let _ = (sdk, user_home, root, owner);
    return Err(FerriError::unsupported("managed Android requires a Unix host"));
  }
  #[cfg(unix)]
  start_unix_server(sdk, user_home, root, owner).await
}

#[cfg(unix)]
async fn start_unix_server(
  sdk: &mut AndroidSdk,
  user_home: &Path,
  root: &Path,
  owner: &super::AndroidOwner,
) -> Result<()> {
  let binary = sdk.root.join("platform-tools/adb");
  let auth_home = user_home.join(".android");
  tokio::fs::create_dir_all(&auth_home).await?;
  let keys = auth_home.join("adbkey");
  let mut keygen = sdk.command(&binary);
  keygen.env("HOME", user_home).arg("keygen").arg(&keys);
  output(keygen, None, Some(Duration::from_secs(10))).await?;

  let reservation = TcpListener::bind("127.0.0.1:0")?;
  let port = reservation.local_addr()?.port();
  let mut command = sdk.command(Path::new("/bin/sh"));
  command
    .args(["-c", "exec 3>&1; exec 1>/dev/null; exec \"$@\"", "ferridriver-adb"])
    .arg(&binary)
    .args(["-L", "acceptfd:0", "--reply-fd", "3", "server", "nodaemon"])
    // ADB reads auth and pairing state from HOME, ignoring ANDROID_USER_HOME.
    .env("HOME", user_home)
    .env("ANDROID_USER_HOME", user_home)
    .env("ADB_VENDOR_KEYS", &keys)
    .env("ADB_USB", "0")
    .env("ADB_EMU", "0")
    .env("ADB_MDNS_AUTO_CONNECT", "0")
    .stdin(Stdio::from(std::os::fd::OwnedFd::from(reservation)))
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
  #[cfg(unix)]
  command.process_group(0);
  let mut child = command.spawn()?;
  let stdout = child
    .stdout
    .take()
    .ok_or_else(|| FerriError::backend("missing ADB readiness pipe"))?;
  let stderr = drain_child_stderr(&mut child);
  let mut group = ChildGroup::new(child);
  group.own_dir(root);
  *owner.adb_group.lock().await = Some(group);
  let mut reader = tokio::io::BufReader::new(stdout);
  let mut acknowledgement = String::new();
  tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut acknowledgement))
    .await
    .map_err(|_| FerriError::timeout("starting private ADB server", 10_000))??;
  if acknowledgement != "OK\n"
    || owner
      .adb_group
      .lock()
      .await
      .as_mut()
      .is_none_or(|group| !group.is_running())
  {
    return Err(FerriError::backend(format!(
      "private ADB server failed to initialize: {:?}",
      stderr.lines()
    )));
  }
  sdk.adb = Some(AdbEndpoint {
    port,
    home: user_home.to_owned(),
  });
  Ok(())
}

pub(super) fn reserve_emulator_ports() -> Result<(TcpListener, TcpListener)> {
  let scan_limit = std::env::var("ADB_LOCAL_TRANSPORT_MAX_PORT")
    .ok()
    .map(|value| value.parse::<u16>())
    .transpose()
    .map_err(|_| FerriError::invalid_argument("ADB_LOCAL_TRANSPORT_MAX_PORT", "expected a TCP port"))?
    .unwrap_or(5585);
  for _ in 0..64 {
    let reservation = TcpListener::bind("127.0.0.1:0")?;
    let port = reservation.local_addr()?.port();
    let console_port = port & !1;
    if console_port <= scan_limit {
      continue;
    }
    let peer = if port.is_multiple_of(2) { port + 1 } else { port - 1 };
    if let Ok(adjacent) = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, peer)) {
      return Ok(if port.is_multiple_of(2) {
        (reservation, adjacent)
      } else {
        (adjacent, reservation)
      });
    }
  }
  Err(FerriError::backend(
    "cannot reserve emulator ports outside the shared ADB scan range",
  ))
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;

  #[test]
  fn reserves_adjacent_console_and_device_ports() {
    let (console, device) = reserve_emulator_ports().unwrap();
    let console_address = console.local_addr().unwrap();
    let device_address = device.local_addr().unwrap();
    assert!(console_address.port().is_multiple_of(2));
    assert_eq!(device_address.port(), console_address.port() + 1);
    assert!(TcpListener::bind(console_address).is_err());
    assert!(TcpListener::bind(device_address).is_err());
  }

  fn sdk(root: &Path) -> std::io::Result<AndroidSdk> {
    std::fs::create_dir(root.join("platform-tools"))?;
    std::os::unix::fs::symlink(
      Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/android/private-adb.py"),
      root.join("platform-tools/adb"),
    )?;
    Ok(AndroidSdk {
      root: root.to_owned(),
      tools: root.to_owned(),
      java_home: root.to_owned(),
      adb: None,
    })
  }

  #[tokio::test]
  async fn private_clients_do_not_restart_a_closed_owned_server() {
    let root = tempfile::tempdir().unwrap();
    let mut sdk = sdk(root.path()).unwrap();
    let home = root.path().join("user");
    std::fs::create_dir(&home).unwrap();
    let profile = tempfile::tempdir().unwrap();
    let profile_path = profile.path().to_owned();
    let owner = super::super::AndroidOwner::new(profile);
    start_server(&mut sdk, &home, &profile_path, &owner).await.unwrap();
    assert_eq!(sdk.adb("emulator-sashoush", &["get-state"]).await.unwrap(), "device");
    owner.close().await.unwrap();
    assert!(sdk.root.join("platform-tools/adb").exists());
    assert!(sdk.adb("emulator-sashoush", &["get-state"]).await.is_err());
    assert!(!home.join(".android/unowned-replacement").exists());
  }

  #[tokio::test]
  async fn cancelled_readiness_keeps_the_server_owned_until_explicit_close() {
    let root = tempfile::tempdir().unwrap();
    let mut sdk = sdk(root.path()).unwrap();
    let home = root.path().join("user");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("stall"), "").unwrap();
    let started = home.join("pid");
    let profile = tempfile::tempdir().unwrap();
    let profile_path = profile.path().to_owned();
    let owner = super::super::AndroidOwner::new(profile);
    let retained = std::sync::Arc::clone(&owner);
    let task = tokio::spawn(async move { start_server(&mut sdk, &home, &profile_path, &retained).await });
    tokio::time::timeout(Duration::from_secs(2), async {
      while !started.exists() {
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(owner.adb_group.lock().await.as_mut().unwrap().is_running());
    assert!(!owner.cleanup_complete());
    owner.close().await.unwrap();
    assert!(owner.cleanup_complete());
    assert!(owner.adb_group.lock().await.is_none());
  }

  #[tokio::test]
  async fn cancelling_startup_reaps_the_private_server() {
    let root = tempfile::tempdir().unwrap();
    let mut sdk = sdk(root.path()).unwrap();
    let home = root.path().join("user");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("stall"), "").unwrap();
    let started = home.join("pid");
    let profile = tempfile::tempdir().unwrap();
    let profile_path = profile.path().to_owned();
    let owner = super::super::AndroidOwner::new(profile);
    let task = tokio::spawn(async move { start_server(&mut sdk, &home, &profile_path, &owner).await });
    let pid = tokio::time::timeout(Duration::from_secs(2), async {
      loop {
        if let Ok(pid) = tokio::fs::read_to_string(&started).await {
          break pid;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .unwrap();
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    tokio::time::timeout(Duration::from_secs(2), async {
      loop {
        let status = tokio::process::Command::new("ps")
          .args(["-p", &pid, "-o", "pid="])
          .stdout(Stdio::null())
          .stderr(Stdio::null())
          .status()
          .await
          .unwrap();
        if !status.success() {
          break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
      }
    })
    .await
    .unwrap();
  }
}

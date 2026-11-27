//! Unit tests for the Android ADB API.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::device::{process_package, web_view_sockets};
use super::*;
use tokio::net::TcpListener;

/// An ADB server with the devices `listing` names, whose every shell
/// answers with the command it was given.
async fn adb(listing: &'static str) -> AndroidDevicesOptions {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    loop {
      let Ok((mut stream, _)) = listener.accept().await else {
        return;
      };
      tokio::spawn(async move {
        let mut digits = [0u8; 4];
        stream.read_exact(&mut digits).await.unwrap();
        let length = usize::from_str_radix(std::str::from_utf8(&digits).unwrap(), 16).unwrap();
        let mut message = vec![0u8; length];
        stream.read_exact(&mut message).await.unwrap();
        let message = String::from_utf8(message).unwrap();
        if message == "host:devices" {
          stream
            .write_all(format!("OKAY{:04x}{listing}", listing.len()).as_bytes())
            .await
            .unwrap();
          return;
        }
        assert!(message.starts_with("host:transport:"), "{message}");
        stream.write_all(b"OKAY").await.unwrap();
        let mut digits = [0u8; 4];
        stream.read_exact(&mut digits).await.unwrap();
        let length = usize::from_str_radix(std::str::from_utf8(&digits).unwrap(), 16).unwrap();
        let mut service = vec![0u8; length];
        stream.read_exact(&mut service).await.unwrap();
        let service = String::from_utf8(service).unwrap();
        stream.write_all(b"OKAY").await.unwrap();
        match service.as_str() {
          "shell:getprop ro.product.model" => stream.write_all(b"Pixel 7\n").await.unwrap(),
          "shell:echo-back" => {
            let mut heard = [0u8; 4];
            stream.read_exact(&mut heard).await.unwrap();
            stream.write_all(&heard).await.unwrap();
            stream.write_all(b"!").await.unwrap();
          },
          other => stream.write_all(other.as_bytes()).await.unwrap(),
        }
      });
    }
  });
  AndroidDevicesOptions {
    host: None,
    port: Some(port),
    omit_driver_install: None,
  }
}

#[test]
fn web_view_sockets_are_read_with_the_process_their_name_carries() {
  let listing = "0000000000000000: 00000002 00000000 00010000 0001 01 5841881 @webview_devtools_remote_32327\n\
    0000000000000000: 00000002 00000000 00010000 0001 01 5841882 @webview_devtools_remote_zeus\n\
    0000000000000000: 00000002 00000000 00010000 0001 01 5841883 @webview_devtools_remote_4242_zeus\n\
    0000000000000000: 00000002 00000000 00010000 0001 01 5841884 @chrome_devtools_remote\n\
    0000000000000000: 00000002 00000000 00010000 0001 01 5841881 @webview_devtools_remote_32327\n\
    shell:cat /proc/net/unix | grep webview_devtools_remote\n";
  assert_eq!(
    web_view_sockets(listing),
    [
      ("webview_devtools_remote_32327".to_owned(), Some(32327)),
      ("webview_devtools_remote_zeus".to_owned(), None),
      ("webview_devtools_remote_4242_zeus".to_owned(), Some(4242)),
    ]
  );
}

#[test]
fn a_process_is_named_by_the_line_with_its_pid() {
  let listing = "u0_a120      14242   901 14500000 90000 0 0 S com.example.other\n\
    u0_a145       4242   901 15200000 120000 0 0 S com.example.app\n";
  assert_eq!(process_package(listing, 4242), "com.example.app");
  assert_eq!(process_package(listing, 7), "");
}

/// An ADB server with one device whose web views are what `views`
/// holds: a line of `/proc/net/unix` each.
async fn adb_with_views(views: Arc<std::sync::Mutex<String>>) -> AndroidDevicesOptions {
  let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
  let port = listener.local_addr().unwrap().port();
  tokio::spawn(async move {
    loop {
      let Ok((mut stream, _)) = listener.accept().await else {
        return;
      };
      let views = Arc::clone(&views);
      tokio::spawn(async move {
        let request = async |stream: &mut tokio::net::TcpStream| {
          let mut digits = [0u8; 4];
          stream.read_exact(&mut digits).await.ok()?;
          let length = usize::from_str_radix(std::str::from_utf8(&digits).ok()?, 16).ok()?;
          let mut message = vec![0u8; length];
          stream.read_exact(&mut message).await.ok()?;
          String::from_utf8(message).ok()
        };
        let Some(first) = request(&mut stream).await else {
          return;
        };
        if first == "host:devices" {
          let _ = stream.write_all(b"OKAY0015emulator-5554\tdevice\n").await;
          return;
        }
        let _ = stream.write_all(b"OKAY").await;
        let Some(service) = request(&mut stream).await else {
          return;
        };
        let _ = stream.write_all(b"OKAY").await;
        let answer = match service.as_str() {
          "shell:getprop ro.product.model" => "Pixel 7\n".to_owned(),
          "shell:cat /proc/net/unix | grep webview_devtools_remote" => views.lock().unwrap().clone(),
          "shell:ps -A | grep 4242" => "u0_a145       4242   901 15200000 120000 0 0 S com.example.app\n".to_owned(),
          other => other.to_owned(),
        };
        let _ = stream.write_all(answer.as_bytes()).await;
      });
    }
  });
  AndroidDevicesOptions {
    host: None,
    port: Some(port),
    omit_driver_install: None,
  }
}

#[tokio::test]
async fn web_views_follow_the_device_and_announce_themselves() {
  const SHOWN: &str = "0000000000000000: 00000002 00000000 00010000 0001 01 5841881 @webview_devtools_remote_4242\n";
  let views = Arc::new(std::sync::Mutex::new(SHOWN.to_owned()));
  let android = Android::new();
  let device = android
    .devices(adb_with_views(Arc::clone(&views)).await)
    .await
    .unwrap()
    .remove(0);

  let listed = device.web_views();
  assert_eq!(listed.len(), 1);
  assert_eq!((listed[0].pid(), listed[0].pkg()), (4242, "com.example.app"));
  assert_eq!(listed[0].socket_name(), "webview_devtools_remote_4242");
  let found = device
    .web_view(
      &AndroidWebViewSelector {
        pkg: Some("com.example.app".into()),
        socket_name: None,
      },
      None,
    )
    .await
    .unwrap();
  assert!(found == listed[0], "the listed view is the one found");

  // The view goes away: it closes and leaves the list.
  views.lock().unwrap().clear();
  tokio::time::timeout(std::time::Duration::from_secs(5), listed[0].closed())
    .await
    .expect("the view closed");
  assert!(listed[0].is_closed());
  assert_eq!(device.web_views().len(), 0);
  let gone = listed[0].page().await.unwrap_err().to_string();
  assert!(gone.contains("WebView has been closed"), "{gone}");

  // One that appears later is announced, and found by a waiting call.
  let mut events = device.subscribe();
  let by_socket = AndroidWebViewSelector {
    pkg: None,
    socket_name: Some("webview_devtools_remote_4242".into()),
  };
  let waiting = device.web_view(&by_socket, Some(5_000));
  *views.lock().unwrap() = SHOWN.to_owned();
  let appeared = waiting.await.unwrap();
  assert_eq!(appeared.pkg(), "com.example.app");
  assert!(appeared != listed[0], "a view that comes back is a new one");
  assert!(matches!(events.recv().await, Ok(AndroidDeviceEvent::WebView(view)) if view == appeared));

  let missing = device
    .web_view(
      &AndroidWebViewSelector {
        pkg: Some("com.example.absent".into()),
        socket_name: None,
      },
      Some(700),
    )
    .await
    .unwrap_err()
    .to_string();
  assert!(missing.contains("700ms") && missing.contains("webview"), "{missing}");

  device.close().await;
  assert!(appeared.is_closed());
  let mut closed = false;
  while let Ok(event) = events.try_recv() {
    closed |= matches!(event, AndroidDeviceEvent::Close);
  }
  assert!(closed, "close is announced");
}

#[tokio::test]
async fn devices_are_the_usable_ones_and_keep_their_identity() {
  let options = adb("emulator-5554\tdevice\nR58M123ABC\tunauthorized\n").await;
  let android = Android::new();
  let devices = android.devices(options.clone()).await.unwrap();
  assert_eq!(devices.len(), 1);
  assert_eq!(devices[0].serial(), "emulator-5554");
  assert_eq!(devices[0].model(), "Pixel 7");

  let again = android.devices(options.clone()).await.unwrap();
  assert!(devices[0] == again[0], "a listed device is the same object");

  devices[0].close().await;
  let after = android.devices(options).await.unwrap();
  assert!(devices[0] != after[0], "a closed device is listed anew");
  assert!(devices[0].is_closed() && !after[0].is_closed());
}

#[tokio::test]
async fn shell_and_screenshot_return_what_the_device_wrote() {
  let android = Android::new();
  let device = android
    .devices(adb("emulator-5554\tdevice\n").await)
    .await
    .unwrap()
    .remove(0);
  assert_eq!(device.shell("id -u").await.unwrap(), b"shell:id -u");

  let directory = tempfile::tempdir().unwrap();
  let path = directory.path().join("screen.png");
  let image = device.screenshot(Some(&path)).await.unwrap();
  assert_eq!(image, b"shell:screencap -p");
  assert_eq!(std::fs::read(&path).unwrap(), image);
}

#[tokio::test]
async fn a_closed_device_refuses_every_call() {
  let android = Android::new();
  let device = android
    .devices(adb("emulator-5554\tdevice\n").await)
    .await
    .unwrap()
    .remove(0);
  device.close().await;
  for error in [
    device.shell("true").await.unwrap_err(),
    device.screenshot(None).await.unwrap_err(),
    device
      .push(AndroidFile::Bytes(vec![1]), "/data/local/tmp/x", None)
      .await
      .unwrap_err(),
    device.install_apk(AndroidFile::Bytes(vec![1]), None).await.unwrap_err(),
    device.open("shell:true").await.err().unwrap(),
  ] {
    assert!(error.to_string().contains("Device is closed"), "{error}");
  }
}

#[tokio::test]
async fn a_file_that_cannot_be_read_names_itself() {
  let android = Android::new();
  let device = android
    .devices(adb("emulator-5554\tdevice\n").await)
    .await
    .unwrap()
    .remove(0);
  let missing = std::env::temp_dir().join("ferridriver-no-such-file-sashoush.apk");
  let error = device
    .install_apk(AndroidFile::Path(missing.clone()), None)
    .await
    .unwrap_err()
    .to_string();
  assert!(error.contains(&missing.display().to_string()), "{error}");
}

#[tokio::test]
async fn a_socket_carries_bytes_both_ways_until_it_closes() {
  let android = Android::new();
  let device = android
    .devices(adb("emulator-5554\tdevice\n").await)
    .await
    .unwrap()
    .remove(0);
  let socket = device.open("shell:echo-back").await.unwrap();
  socket.write(b"ping").await.unwrap();
  let mut heard = Vec::new();
  while let Some(chunk) = socket.next_data().await {
    heard.extend(chunk);
  }
  assert_eq!(heard, b"ping!");
  socket.close().await;
  assert!(
    socket
      .write(b"late")
      .await
      .unwrap_err()
      .to_string()
      .contains("Socket is closed")
  );
}

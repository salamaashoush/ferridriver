//! Playwright's Android driver: the instrumentation APK that finds and
//! acts on native views through UI Automator, and the calls `_android`
//! makes to it. `server/android/android.ts` for the transport,
//! `server/dispatchers/androidDispatcher.ts` for what each call sends.
//!
//! The driver listens on the abstract socket
//! `playwright_android_driver_socket`. A message is four bytes of
//! big-endian length and a JSON object: `{ id, method, params }` one way,
//! `{ id, result }` or `{ id, error }` back.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rustc_hash::FxHashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::wire::AdbServer;
use crate::error::{FerriError, Result};

const DRIVER_PACKAGE: &str = "com.microsoft.playwright.androiddriver";
const DRIVER_SOCKET: &str = "localabstract:playwright_android_driver_socket";
const DRIVER_RUNNER: &str = "com.microsoft.playwright.androiddriver.test/androidx.test.runner.AndroidJUnitRunner";

/// A text condition of an [`AndroidSelector`]: Playwright's
/// `string | RegExp`. A string is the whole value; a pattern is the
/// regular expression's source, which the driver also matches against
/// the whole value, so `Network` does not find "Network & internet" and
/// `Network.*` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AndroidText {
  Exact(String),
  Pattern(String),
}

impl AndroidText {
  /// What the driver matches with: a pattern as it is, a string escaped
  /// and anchored, as `client/android.ts::toSelectorChannel` does.
  fn to_wire(&self) -> String {
    match self {
      Self::Pattern(source) => source.clone(),
      Self::Exact(text) => {
        let mut escaped = String::with_capacity(text.len() + 2);
        escaped.push('^');
        for character in text.chars() {
          match character {
            '|' | '\\' | '{' | '}' | '(' | ')' | '[' | ']' | '^' | '$' | '+' | '*' | '?' | '.' => {
              escaped.push('\\');
              escaped.push(character);
            },
            '-' => escaped.push_str("\\x2d"),
            other => escaped.push(other),
          }
        }
        escaped.push('$');
        escaped
      },
    }
  }
}

impl From<&str> for AndroidText {
  fn from(text: &str) -> Self {
    Self::Exact(text.to_owned())
  }
}

/// Playwright: `AndroidSelector`. Every field that is set must hold for
/// a view to match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AndroidSelector {
  pub checkable: Option<bool>,
  pub checked: Option<bool>,
  pub clazz: Option<AndroidText>,
  pub clickable: Option<bool>,
  pub depth: Option<u32>,
  pub desc: Option<AndroidText>,
  pub enabled: Option<bool>,
  pub focusable: Option<bool>,
  pub focused: Option<bool>,
  /// `hasChild: { selector }`.
  pub has_child: Option<Box<AndroidSelector>>,
  /// `hasDescendant: { selector, maxDepth? }`.
  pub has_descendant: Option<(Box<AndroidSelector>, Option<u32>)>,
  pub long_clickable: Option<bool>,
  pub pkg: Option<AndroidText>,
  pub res: Option<AndroidText>,
  pub scrollable: Option<bool>,
  pub selected: Option<bool>,
  pub text: Option<AndroidText>,
}

impl AndroidSelector {
  pub(crate) fn to_wire(&self) -> serde_json::Value {
    let mut wire = serde_json::Map::new();
    let mut flag = |name: &str, value: Option<bool>| {
      if let Some(value) = value {
        wire.insert(name.to_owned(), value.into());
      }
    };
    flag("checkable", self.checkable);
    flag("checked", self.checked);
    flag("clickable", self.clickable);
    flag("enabled", self.enabled);
    flag("focusable", self.focusable);
    flag("focused", self.focused);
    flag("longClickable", self.long_clickable);
    flag("scrollable", self.scrollable);
    flag("selected", self.selected);
    for (name, text) in [
      ("clazz", &self.clazz),
      ("pkg", &self.pkg),
      ("desc", &self.desc),
      ("res", &self.res),
      ("text", &self.text),
    ] {
      if let Some(text) = text {
        wire.insert(name.to_owned(), text.to_wire().into());
      }
    }
    if let Some(depth) = self.depth {
      wire.insert("depth".to_owned(), depth.into());
    }
    if let Some(child) = &self.has_child {
      wire.insert(
        "hasChild".to_owned(),
        serde_json::json!({ "selector": child.to_wire() }),
      );
    }
    if let Some((descendant, max_depth)) = &self.has_descendant {
      let mut has = serde_json::Map::new();
      has.insert("selector".to_owned(), descendant.to_wire());
      if let Some(max_depth) = max_depth {
        has.insert("maxDepth".to_owned(), (*max_depth).into());
      }
      wire.insert("hasDescendant".to_owned(), has.into());
    }
    wire.into()
  }
}

/// Playwright: the `'down' | 'up' | 'left' | 'right'` of `fling`,
/// `scroll` and `swipe`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AndroidDirection {
  Down,
  Up,
  Left,
  Right,
}

impl AndroidDirection {
  pub(crate) fn as_str(self) -> &'static str {
    match self {
      Self::Down => "down",
      Self::Up => "up",
      Self::Left => "left",
      Self::Right => "right",
    }
  }

  /// Parse Playwright's spelling.
  ///
  /// # Errors
  ///
  /// Returns an error for anything but the four directions.
  pub fn parse(direction: &str) -> Result<Self> {
    match direction {
      "down" => Ok(Self::Down),
      "up" => Ok(Self::Up),
      "left" => Ok(Self::Left),
      "right" => Ok(Self::Right),
      other => Err(FerriError::invalid_argument(
        "direction",
        format!("expected 'down', 'up', 'left' or 'right', got '{other}'"),
      )),
    }
  }
}

/// The bounds of a view on the device's screen.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AndroidBounds {
  pub x: f64,
  pub y: f64,
  pub width: f64,
  pub height: f64,
}

fn text_or_empty<'de, D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<String, D::Error> {
  Ok(<Option<String> as serde::Deserialize>::deserialize(deserializer)?.unwrap_or_default())
}

/// Playwright: `AndroidElementInfo`. UI Automator leaves a text it has
/// no value for null; it is the empty string here, as upstream.
// Nine independent properties of a view, as UI Automator reports them and as
// Playwright's type has them: not states of one thing an enum could hold.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AndroidElementInfo {
  #[serde(default, deserialize_with = "text_or_empty")]
  pub clazz: String,
  #[serde(default, deserialize_with = "text_or_empty")]
  pub desc: String,
  #[serde(default, deserialize_with = "text_or_empty")]
  pub res: String,
  #[serde(default, deserialize_with = "text_or_empty")]
  pub pkg: String,
  #[serde(default, deserialize_with = "text_or_empty")]
  pub text: String,
  pub bounds: AndroidBounds,
  pub checkable: bool,
  pub checked: bool,
  pub clickable: bool,
  pub enabled: bool,
  pub focusable: bool,
  pub focused: bool,
  pub long_clickable: bool,
  pub scrollable: bool,
  pub selected: bool,
}

/// Playwright's `AndroidKey` names and the Android key codes they send.
const KEYS: &[(&str, u32)] = &[
  ("Unknown", 0),
  ("SoftLeft", 1),
  ("SoftRight", 2),
  ("Home", 3),
  ("Back", 4),
  ("Call", 5),
  ("EndCall", 6),
  ("0", 7),
  ("1", 8),
  ("2", 9),
  ("3", 10),
  ("4", 11),
  ("5", 12),
  ("6", 13),
  ("7", 14),
  ("8", 15),
  ("9", 16),
  ("Star", 17),
  ("*", 17),
  ("Pound", 18),
  ("#", 18),
  ("DialUp", 19),
  ("DialDown", 20),
  ("DialLeft", 21),
  ("DialRight", 22),
  ("DialCenter", 23),
  ("VolumeUp", 24),
  ("VolumeDown", 25),
  ("Power", 26),
  ("Camera", 27),
  ("Clear", 28),
  ("A", 29),
  ("B", 30),
  ("C", 31),
  ("D", 32),
  ("E", 33),
  ("F", 34),
  ("G", 35),
  ("H", 36),
  ("I", 37),
  ("J", 38),
  ("K", 39),
  ("L", 40),
  ("M", 41),
  ("N", 42),
  ("O", 43),
  ("P", 44),
  ("Q", 45),
  ("R", 46),
  ("S", 47),
  ("T", 48),
  ("U", 49),
  ("V", 50),
  ("W", 51),
  ("X", 52),
  ("Y", 53),
  ("Z", 54),
  ("Comma", 55),
  (",", 55),
  ("Period", 56),
  (".", 56),
  ("AltLeft", 57),
  ("AltRight", 58),
  ("ShiftLeft", 59),
  ("ShiftRight", 60),
  ("Tab", 61),
  ("\\t", 61),
  ("Space", 62),
  (" ", 62),
  ("Sym", 63),
  ("Explorer", 64),
  ("Envelop", 65),
  ("Enter", 66),
  ("Del", 67),
  ("Grave", 68),
  ("Minus", 69),
  ("-", 69),
  ("Equals", 70),
  ("=", 70),
  ("LeftBracket", 71),
  ("(", 71),
  ("RightBracket", 72),
  (")", 72),
  ("Backslash", 73),
  ("\\", 73),
  ("Semicolon", 74),
  (";", 74),
  ("Apostrophe", 75),
  ("`", 75),
  ("Slash", 76),
  ("/", 76),
  ("At", 77),
  ("@", 77),
  ("Num", 78),
  ("HeadsetHook", 79),
  ("Focus", 80),
  ("Plus", 81),
  ("Menu", 82),
  ("Notification", 83),
  ("Search", 84),
  ("MediaPlayPause", 85),
  ("MediaStop", 86),
  ("MediaNext", 87),
  ("MediaPrevious", 88),
  ("MediaRewind", 89),
  ("MediaFastForward", 90),
  ("MediaPlay", 126),
  ("MediaPause", 127),
  ("MediaClose", 128),
  ("MediaEject", 129),
  ("MediaRecord", 130),
  ("ChannelUp", 166),
  ("ChannelDown", 167),
  ("AppSwitch", 187),
  ("Assist", 219),
  ("MediaAudioTrack", 222),
  ("MediaTopMenu", 226),
  ("MediaSkipForward", 272),
  ("MediaSkipBackward", 273),
  ("MediaStepForward", 274),
  ("MediaStepBackward", 275),
  ("Cut", 277),
  ("Copy", 278),
  ("Paste", 279),
];

/// The key code of a Playwright `AndroidKey`.
pub(crate) fn key_code(key: &str) -> Result<u32> {
  KEYS
    .iter()
    .find(|(name, _)| *name == key)
    .map(|(_, code)| *code)
    .ok_or_else(|| FerriError::invalid_argument("key", format!("Unknown key: {key}")))
}

/// The key codes that type `text`, a character at a time, as
/// `androidDispatcher.ts::inputType` maps them: by the upper-case key
/// name.
pub(crate) fn key_codes(text: &str) -> Result<Vec<u32>> {
  text
    .chars()
    .map(|character| {
      let name: String = character.to_uppercase().collect();
      KEYS
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, code)| *code)
        .ok_or_else(|| FerriError::invalid_argument("text", format!("No mapping for {character} found")))
    })
    .collect()
}

type Pending =
  Arc<std::sync::Mutex<FxHashMap<u64, tokio::sync::oneshot::Sender<std::result::Result<serde_json::Value, String>>>>>;

/// A connection to the driver running on a device.
pub(crate) struct Driver {
  writer: tokio::sync::Mutex<tokio::net::tcp::OwnedWriteHalf>,
  pending: Pending,
  next_id: AtomicU64,
  reader: tokio::task::AbortHandle,
  /// The `am instrument` shell the driver runs in: closing it stops the
  /// driver.
  _instrumentation: tokio::net::TcpStream,
}

impl Driver {
  /// Install the driver unless told not to, start it, and connect.
  ///
  /// `apks` is where the two APKs are on this machine. It is not read
  /// when `omit_install` says the device has them already.
  pub(crate) async fn start(
    server: &AdbServer,
    serial: &str,
    omit_install: bool,
    apks: Option<&std::path::Path>,
    timeout_ms: u64,
  ) -> Result<Self> {
    server.shell(serial, &format!("am force-stop {DRIVER_PACKAGE}")).await?;
    if !omit_install {
      let directory = apks.ok_or_else(|| {
        FerriError::backend("the Android driver is not installed; run `ferridriver install android-driver`")
      })?;
      server
        .shell(serial, &format!("cmd package uninstall {DRIVER_PACKAGE}"))
        .await?;
      server
        .shell(serial, &format!("cmd package uninstall {DRIVER_PACKAGE}.test"))
        .await?;
      for apk in crate::install::ANDROID_DRIVER_APKS {
        let path = directory.join(apk);
        let content = tokio::fs::read(&path)
          .await
          .map_err(|error| FerriError::backend(format!("{}: {error}", path.display())))?;
        server.install(serial, &content, &[]).await?;
      }
    }
    let instrumentation = server
      .open(serial, &format!("shell:am instrument -w {DRIVER_RUNNER}"))
      .await?;

    let connect = async {
      loop {
        if let Ok(stream) = server.open(serial, DRIVER_SOCKET).await {
          return stream;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
      }
    };
    let stream = if timeout_ms == 0 {
      connect.await
    } else {
      tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), connect)
        .await
        .map_err(|_| FerriError::timeout("waiting for the Android driver to start", timeout_ms))?
    };
    Ok(Self::over(stream, instrumentation))
  }

  fn over(stream: tokio::net::TcpStream, instrumentation: tokio::net::TcpStream) -> Self {
    let (mut read, write) = stream.into_split();
    let pending: Pending = Arc::default();
    let answered = Arc::clone(&pending);
    let reader = tokio::spawn(async move {
      loop {
        let mut length = [0u8; 4];
        if read.read_exact(&mut length).await.is_err() {
          break;
        }
        let mut message = vec![0u8; u32::from_be_bytes(length) as usize];
        if read.read_exact(&mut message).await.is_err() {
          break;
        }
        let Ok(response) = serde_json::from_slice::<serde_json::Value>(&message) else {
          continue;
        };
        let Some(id) = response.get("id").and_then(serde_json::Value::as_u64) else {
          continue;
        };
        let waiting = answered
          .lock()
          .unwrap_or_else(std::sync::PoisonError::into_inner)
          .remove(&id);
        if let Some(waiting) = waiting {
          let outcome = match response.get("error").and_then(serde_json::Value::as_str) {
            Some(error) => Err(error.to_owned()),
            None => Ok(response.get("result").cloned().unwrap_or(serde_json::Value::Null)),
          };
          let _ = waiting.send(outcome);
        }
      }
      // The driver is gone: nothing still waiting will be answered.
      answered
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
    })
    .abort_handle();
    Self {
      writer: tokio::sync::Mutex::new(write),
      pending,
      next_id: AtomicU64::new(0),
      reader,
      _instrumentation: instrumentation,
    }
  }

  /// One call to the driver. `timeout_ms` is how long the driver itself
  /// waits for a selector to match, sent with the call as upstream does;
  /// the driver reads zero as "look once".
  pub(crate) async fn send(
    &self,
    method: &str,
    mut params: serde_json::Value,
    timeout_ms: u64,
  ) -> Result<serde_json::Value> {
    if let Some(bag) = params.as_object_mut() {
      bag.insert("timeout".to_owned(), timeout_ms.into());
    }
    let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
    let (answer, answered) = tokio::sync::oneshot::channel();
    self
      .pending
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(id, answer);
    let message = serde_json::to_vec(&serde_json::json!({ "id": id, "method": method, "params": params }))
      .map_err(|error| FerriError::backend(error.to_string()))?;
    let length = u32::try_from(message.len()).map_err(|_| FerriError::backend("driver message longer than 4 GiB"))?;
    {
      let mut writer = self.writer.lock().await;
      writer.write_all(&length.to_be_bytes()).await?;
      writer.write_all(&message).await?;
    }
    match answered.await {
      Ok(Ok(result)) => Ok(result),
      Ok(Err(error)) => Err(FerriError::backend(error)),
      Err(_) => Err(FerriError::target_closed(Some("Device closed".into()))),
    }
  }
}

impl Drop for Driver {
  fn drop(&mut self) {
    self.reader.abort();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_string_is_the_whole_value_and_a_pattern_is_its_source() {
    assert_eq!(AndroidText::from("Sign in (beta)").to_wire(), r"^Sign in \(beta\)$");
    assert_eq!(AndroidText::from("a-b.c").to_wire(), r"^a\x2db\.c$");
    assert_eq!(AndroidText::Pattern("^Sign".into()).to_wire(), "^Sign");
  }

  #[test]
  fn a_selector_sends_only_what_it_sets() {
    let selector = AndroidSelector {
      text: Some("Save".into()),
      clickable: Some(true),
      has_descendant: Some((
        Box::new(AndroidSelector {
          res: Some(AndroidText::Pattern("id/label$".into())),
          ..Default::default()
        }),
        Some(3),
      )),
      has_child: Some(Box::new(AndroidSelector {
        checked: Some(false),
        ..Default::default()
      })),
      ..Default::default()
    };
    assert_eq!(
      selector.to_wire(),
      serde_json::json!({
        "text": "^Save$",
        "clickable": true,
        "hasDescendant": { "selector": { "res": "id/label$" }, "maxDepth": 3 },
        "hasChild": { "selector": { "checked": false } },
      })
    );
    assert_eq!(AndroidSelector::default().to_wire(), serde_json::json!({}));
  }

  #[test]
  fn keys_are_upstreams_names_and_typing_goes_by_upper_case_name() {
    assert_eq!(key_code("Back").unwrap(), 4);
    assert_eq!(key_code("Enter").unwrap(), 66);
    assert!(
      key_code("NoSuchKey")
        .unwrap_err()
        .to_string()
        .contains("Unknown key: NoSuchKey")
    );
    assert_eq!(
      key_codes("a1").unwrap(),
      [key_code("A").unwrap(), key_code("1").unwrap()]
    );
    assert!(
      key_codes("é")
        .unwrap_err()
        .to_string()
        .contains("No mapping for é found")
    );
  }

  #[test]
  fn element_info_reads_null_texts_as_empty() {
    let info: AndroidElementInfo = serde_json::from_value(serde_json::json!({
      "clazz": "android.widget.Button", "desc": null, "res": null, "pkg": "com.example", "text": "Save",
      "bounds": { "x": 1, "y": 2, "width": 30, "height": 40 },
      "checkable": false, "checked": false, "clickable": true, "enabled": true, "focusable": true,
      "focused": false, "longClickable": false, "scrollable": false, "selected": false,
    }))
    .unwrap();
    assert_eq!(
      (info.desc.as_str(), info.res.as_str(), info.text.as_str()),
      ("", "", "Save")
    );
    assert_eq!(info.bounds.width, 30.0);
    assert!(info.clickable && !info.long_clickable);
  }

  #[tokio::test]
  async fn a_call_is_framed_big_endian_and_matched_to_its_answer_by_id() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let played = tokio::spawn(async move {
      let (mut stream, _) = listener.accept().await.unwrap();
      let mut seen = Vec::new();
      for _ in 0..2 {
        let mut length = [0u8; 4];
        stream.read_exact(&mut length).await.unwrap();
        let mut message = vec![0u8; u32::from_be_bytes(length) as usize];
        stream.read_exact(&mut message).await.unwrap();
        seen.push(serde_json::from_slice::<serde_json::Value>(&message).unwrap());
      }
      // Answered out of order: the second call first.
      for answer in [
        serde_json::json!({ "id": 2, "error": "Selector not found" }),
        serde_json::json!({ "id": 1, "result": { "ok": true } }),
      ] {
        let body = serde_json::to_vec(&answer).unwrap();
        stream
          .write_all(&u32::try_from(body.len()).unwrap().to_be_bytes())
          .await
          .unwrap();
        stream.write_all(&body).await.unwrap();
      }
      seen
    });
    let driver = Driver::over(
      tokio::net::TcpStream::connect(address).await.unwrap(),
      tokio::net::TcpStream::connect(address).await.unwrap(),
    );
    let (first, second) = tokio::join!(
      driver.send("click", serde_json::json!({ "selector": { "text": "^Save$" } }), 30_000),
      driver.send("wait", serde_json::json!({ "selector": {} }), 0),
    );
    assert_eq!(first.unwrap(), serde_json::json!({ "ok": true }));
    assert!(second.unwrap_err().to_string().contains("Selector not found"));
    let seen = played.await.unwrap();
    assert_eq!(
      seen[0],
      serde_json::json!({ "id": 1, "method": "click", "params": { "selector": { "text": "^Save$" }, "timeout": 30000 } })
    );
    assert_eq!(seen[1]["params"]["timeout"], 0);
  }
}

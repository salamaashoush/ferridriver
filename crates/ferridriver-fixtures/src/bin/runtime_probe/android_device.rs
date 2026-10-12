use anyhow::{Context, Result};
use base64::Engine as _;
use ferridriver::android::{Android, AndroidDevicesOptions, AndroidFile, AndroidLaunchBrowserOptions};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum Action {
  Shell {
    command: String,
  },
  Screenshot,
  Push {
    content: String,
    path: String,
    mode: Option<u32>,
  },
  Socket {
    command: String,
    write: Option<String>,
  },
  LaunchBrowser {
    pkg: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    url: String,
    expression: String,
  },
  Close,
}

/// Drive the first device an ADB server lists. The first result is the
/// listing itself; each action adds one.
pub async fn run(host: Option<String>, port: Option<u16>, actions: Vec<Action>) -> Result<Value> {
  let android = Android::new();
  let devices = android
    .devices(AndroidDevicesOptions {
      host,
      port,
      omit_driver_install: None,
    })
    .await?;
  let device = devices
    .first()
    .context("the ADB server lists no usable device")?
    .clone();
  let mut results = vec![json!({
    "serials": devices.iter().map(ferridriver::android::AndroidDevice::serial).collect::<Vec<_>>(),
    "model": device.model(),
  })];
  for action in actions {
    results.push(match action {
      Action::Shell { command } => json!(String::from_utf8_lossy(&device.shell(&command).await?)),
      Action::Screenshot => {
        let image = device.screenshot(None).await?;
        json!({ "bytes": image.len(), "head": base64::engine::general_purpose::STANDARD.encode(&image[..image.len().min(8)]) })
      },
      Action::Push { content, path, mode } => {
        device.push(AndroidFile::Bytes(content.into_bytes()), &path, mode).await?;
        Value::Null
      },
      Action::Socket { command, write } => {
        let socket = device.open(&command).await?;
        if let Some(text) = write {
          socket.write(text.as_bytes()).await?;
        }
        let mut heard = Vec::new();
        while let Some(chunk) = socket.next_data().await {
          heard.extend(chunk);
        }
        socket.close().await;
        json!(String::from_utf8_lossy(&heard))
      },
      Action::LaunchBrowser {
        pkg,
        args,
        url,
        expression,
      } => {
        let context = device
          .launch_browser(AndroidLaunchBrowserOptions {
            pkg,
            args,
            ..Default::default()
          })
          .await?;
        let pages = context.pages();
        let page = pages.first().context("the launched browser has no page")?.clone();
        page.goto(&url).await?;
        let value = page.inner().evaluate(&expression).await?;
        let listed = context.pages().len();
        context.close().await?;
        json!({ "pages": listed, "value": value, "closed": context.is_closed() })
      },
      Action::Close => {
        device.close().await;
        Value::Null
      },
    });
  }
  Ok(json!(results))
}

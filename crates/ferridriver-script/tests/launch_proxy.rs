#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `chromium().launch({ proxy })` from inside a script.
//!
//! Scripts get the same `chromium()` / `firefox()` / `webkit()` globals the
//! Rust and NAPI surfaces expose, so a `proxy` declared there has to reach the
//! browser process too — the option being parsed but dropped in the JS-to-Rust
//! lowering is exactly the kind of failure nothing else would catch.

use std::sync::{Arc, Mutex};

use ferridriver_script::{InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngineConfig, Session};

/// A proxy that answers nothing and records the request lines it is sent.
fn spawn_recording_proxy() -> (u16, Arc<Mutex<Vec<String>>>) {
  use std::io::{BufRead as _, BufReader};

  let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind proxy");
  let port = listener.local_addr().expect("addr").port();
  let seen = Arc::new(Mutex::new(Vec::new()));
  let recorder = Arc::clone(&seen);

  std::thread::spawn(move || {
    while let Ok((stream, _)) = listener.accept() {
      let recorder = Arc::clone(&recorder);
      std::thread::spawn(move || {
        let mut line = String::new();
        if BufReader::new(stream).read_line(&mut line).is_ok()
          && let Ok(mut seen) = recorder.lock()
        {
          seen.push(line.trim().to_string());
        }
      });
    }
  });

  (port, seen)
}

#[tokio::test]
async fn a_script_launching_with_a_proxy_routes_through_it() {
  let (port, seen) = spawn_recording_proxy();
  let tmp = tempfile::tempdir().expect("tempdir");

  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(tmp.path()).expect("sandbox")),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    // The script launches its own browser: that launch is what is under test.
    browser: None,
    extensions: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
  };

  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");

  // Nothing resolves this host, so a request reaching the recorder can only
  // have come through the proxy the script asked for.
  let source = format!(
    r"
      const browser = await chromium().launch({{
        headless: true,
        proxy: {{ server: 'http://127.0.0.1:{port}', bypass: '127.0.0.1,localhost' }},
      }});
      try {{
        const page = await browser.newPage();
        try {{ await page.goto('https://proxy-probe.invalid/', {{ timeout: 5000 }}); }} catch (e) {{}}
      }} finally {{
        await browser.close();
      }}
      return true;
    "
  );

  let run = session.execute(&source, &[], RunOptions::default(), &ctx).await;
  assert!(!run.poisoned, "the VM must not be poisoned by a valid script");
  match run.result.outcome {
    Outcome::Ok { .. } => {},
    Outcome::Error { error } => panic!("script error: {error:?}"),
  }

  let requests = seen.lock().expect("recorded requests").clone();
  assert!(
    requests.iter().any(|line| line.contains("proxy-probe.invalid")),
    "a script's launch({{ proxy }}) did not route through the proxy; saw: {requests:?}"
  );
}

//! Browser instances end to end: a bare session key selects a
//! configured instance, and that instance's launch settings reach the
//! real browser process.
//!
//! Both halves were broken in ways config-level tests cannot catch:
//! - the server extracted the instance name with its own `split(':')`,
//!   so every bare key went to `default` regardless of configuration;
//! - `[mcp.browser]` had no `userDataDir` at all, so every launch got a
//!   throwaway profile and lost its cookies on restart (and an external
//!   browser manager could never find the process again).

use serde_json::json;

use super::client::McpClient;

/// Config with one real instance carrying a persistent profile and an
/// extra argument, plus a second instance to prove the "unknown
/// instance" path.
fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
  let dir = tempfile::tempdir().expect("tempdir");
  let profiles = dir.path().join("profiles");
  let config = dir.path().join("ferridriver.toml");
  std::fs::write(
    &config,
    format!(
      "[mcp.browser]\n\
       headless = true\n\
       chromeArgs = [\"--base-flag\"]\n\
       \n\
       [mcp.browser.instances.staging]\n\
       userDataDir = {}\n\
       args = [\"--window-size=900,700\"]\n\
       \n\
       [mcp.browser.instances.other]\n\
       args = []\n",
      serde_json::to_string(&profiles.join("${INSTANCE}").display().to_string()).expect("json path")
    ),
  )
  .expect("write config");
  (dir, config)
}

pub fn run() {
  let (dir, config) = fixture();
  let mut c = McpClient::with_config("cdp-pipe", &config);

  bare_session_key_selects_the_instance(&mut c, &dir.path().join("profiles"));
  unknown_instance_fails_loudly(&mut c);
  contexts_share_one_instance(&mut c);
}

/// `session: "staging"` must drive the `staging` INSTANCE, and its
/// `userDataDir` must be the profile Chrome actually launches with.
fn bare_session_key_selects_the_instance(c: &mut McpClient, profiles: &std::path::Path) {
  let res = c.call_tool("navigate", json!({"url": "about:blank", "session": "staging"}));
  assert_ne!(
    res["result"]["isError"], true,
    "bare key naming a configured instance must launch it: {res}"
  );

  let staging_profile = profiles.join("staging");
  assert!(
    staging_profile.join("Default").is_dir(),
    "`${{INSTANCE}}` userDataDir must reach the browser: {} missing (found: {:?})",
    staging_profile.display(),
    std::fs::read_dir(profiles).map(|d| d.flatten().map(|e| e.path()).collect::<Vec<_>>())
  );

  // The instance is live and usable, not merely launched.
  let res = c.call_tool("evaluate", json!({"expression": "1 + 1", "session": "staging"}));
  assert_ne!(res["result"]["isError"], true, "evaluate on the instance: {res}");
}

/// A session key naming no configured instance must fail with the list
/// of real ones instead of launching an unconfigured browser.
fn unknown_instance_fails_loudly(c: &mut McpClient) {
  let res = c.call_tool("navigate", json!({"url": "about:blank", "session": "typo-env:admin"}));
  assert_eq!(res["result"]["isError"], true, "unknown instance must fail: {res}");
  let text = res["result"]["content"][0]["text"].as_str().unwrap_or_default();
  assert!(text.contains("typo-env"), "names the bad instance: {text}");
  assert!(text.contains("staging"), "lists the configured instances: {text}");
}

/// Two contexts on one instance share the browser process: the profile
/// directory is per instance, so a second context must not need a
/// second launch.
fn contexts_share_one_instance(c: &mut McpClient) {
  for context in ["admin", "tester"] {
    let session = format!("staging:{context}");
    let res = c.call_tool("navigate", json!({"url": "about:blank", "session": session}));
    assert_ne!(res["result"]["isError"], true, "{session} navigate: {res}");
  }
}

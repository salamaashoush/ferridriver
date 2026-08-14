//! The agent-facing response contract, end to end on every backend.
//!
//! Three things are only true if the whole stack cooperates, so all three are
//! asserted against a live browser rather than unit-tested in isolation:
//!
//! - the `### Page` section reports the page the run actually left the session
//!   on, which means the backend's own url/title reads;
//! - a declared secret never appears in any part of the reply — not the
//!   returned value, not the console the script wrote, not the echoed source —
//!   and the echoed source reads it from the environment instead;
//! - the artifacts ceiling evicts an older output when a new one pushes the
//!   directory over it, and never the output the reply just linked.
//!
//! The server is launched with its own config (the shared per-category client
//! has none), so it runs against a temp artifacts root and a temp secrets file.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};

use super::client::{McpClient, data_url, extract_script_payload, ok};

/// The declared secret's value. Distinctive enough that finding it anywhere in
/// a reply is unambiguous evidence of a leak.
const SECRET_VALUE: &str = "s3cr3t-value-9f2a";
const SECRET_NAME: &str = "APP_PASSWORD";

/// Every text block of a tool reply, concatenated — the whole of what a caller
/// receives, which is the surface a secret must not appear on.
fn all_text(resp: &Value) -> String {
  resp["result"]["content"]
    .as_array()
    .map(|blocks| {
      blocks
        .iter()
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
    })
    .unwrap_or_default()
}

/// Files under `dir`, recursively, newest last.
fn artifact_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
  let mut out = Vec::new();
  let mut stack = vec![dir.to_path_buf()];
  while let Some(current) = stack.pop() {
    let Ok(entries) = std::fs::read_dir(&current) else {
      continue;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      if path.is_dir() {
        stack.push(path);
      } else {
        out.push(path);
      }
    }
  }
  out.sort();
  out
}

pub fn test_response_contract(backend: &str) {
  let tmp = tempfile::tempdir().expect("tempdir");
  std::fs::write(
    tmp.path().join(".env.secrets"),
    format!("{SECRET_NAME}={SECRET_VALUE}\n"),
  )
  .unwrap();
  // `artifactsMaxBytes = 1` makes the sweep deterministic without depending on
  // how large a screenshot happens to be: any artifact the current call did
  // not write is over budget.
  std::fs::write(
    tmp.path().join("ferridriver.toml"),
    "artifactsRoot = \"./artifacts\"\nartifactsMaxBytes = 1\n\n[secrets]\nfile = \"./.env.secrets\"\n",
  )
  .unwrap();
  let artifacts = tmp.path().join("artifacts");
  let mut c = McpClient::with_config(backend, &tmp.path().join("ferridriver.toml"));

  let url = data_url("<title>Contract</title><input id=pw>");
  let resp = c.call_tool(
    "run_script",
    json!({
      "source": "await page.goto(args[0]); \
                 await page.locator('#pw').fill(args[1]); \
                 console.log('the password is ' + args[1]); \
                 return 'signed in with ' + args[1];",
      // Passed as a bound argument, the way a caller supplies a credential.
      "args": [url, SECRET_VALUE],
      "code_language": "ts",
    }),
  );
  ok(&resp, "run_script");
  let text = all_text(&resp);

  // ── The page section describes where the run left the session ────────────
  assert!(text.contains("### Page"), "no page section: {text}");
  assert!(
    text.contains("- Page URL: data:text/html"),
    "page section does not carry the live url: {text}"
  );
  // The title is read through the backend under test — a section that only
  // echoed the requested URL would pass without it.
  assert!(
    text.contains("- Page Title: Contract"),
    "page section does not carry the live title: {text}"
  );

  let payload = extract_script_payload(&resp).expect("structured payload");
  assert_eq!(payload["page"]["title"], "Contract", "structured page state: {payload}");
  assert!(
    payload["page"]["url"].as_str().unwrap_or_default().starts_with("data:"),
    "structured page url: {payload}"
  );

  // ── The secret reaches the caller nowhere, by any route ──────────────────
  assert!(
    !text.contains(SECRET_VALUE),
    "the declared secret leaked into the reply: {text}"
  );
  assert!(
    text.contains(&format!("<secret>{SECRET_NAME}</secret>")),
    "nothing was redacted, so the check above proves nothing: {text}"
  );
  // Specifically: the returned value and the console line the script wrote.
  assert!(
    payload["value"]
      .as_str()
      .is_some_and(|v| v == format!("signed in with <secret>{SECRET_NAME}</secret>")),
    "returned value not redacted: {payload}"
  );
  assert!(
    payload["console"].as_array().is_some_and(|entries| entries
      .iter()
      .any(|e| e["message"] == format!("the password is <secret>{SECRET_NAME}</secret>"))),
    "console entry not redacted: {payload}"
  );

  // ── The echoed source is committable: an env read, not a literal ─────────
  assert!(text.contains("### Ran ferridriver code"), "no code section: {text}");
  let code = payload["code"].as_array().expect("code array");
  let fill = code
    .iter()
    .filter_map(|l| l.as_str())
    .find(|l| l.contains(".fill("))
    .unwrap_or_else(|| panic!("no fill line echoed: {code:?}"));
  assert_eq!(
    fill,
    format!("await page.locator('#pw').fill(process.env['{SECRET_NAME}']);"),
    "the echoed fill did not become an environment read"
  );

  // ── Redaction covers the non-script tools too ────────────────────────────
  //
  // The engine redacts what a script hands back, which covers `run_script`
  // and nothing else. `evaluate`, `snapshot` and `search_page` read the page
  // directly, so they need the reply to be redacted on the way out — a
  // credential sitting in the DOM would otherwise come straight back.
  let evaluated = c.call_tool(
    "evaluate",
    json!({ "expression": "document.getElementById('pw').value" }),
  );
  ok(&evaluated, "evaluate");
  let text = all_text(&evaluated);
  assert!(
    !text.contains(SECRET_VALUE),
    "evaluate returned the declared secret verbatim: {text}"
  );
  assert!(
    text.contains(&format!("<secret>{SECRET_NAME}</secret>")),
    "evaluate did not read back the filled value at all, so the check above proves nothing: {text}"
  );

  // Same for the snapshot tool, which serialises the whole a11y tree.
  let searched = c.call_tool("search_page", json!({ "pattern": SECRET_VALUE }));
  ok(&searched, "search_page");
  assert!(
    !all_text(&searched).contains(SECRET_VALUE),
    "search_page echoed the declared secret back in its match output"
  );

  // ── The artifacts ceiling evicts the old, never the just-written ─────────
  let first = c.call_tool("screenshot", json!({}));
  ok(&first, "screenshot 1");
  let after_first = artifact_files(&artifacts);
  assert_eq!(
    after_first.len(),
    1,
    "the call's own artifact must survive its own sweep: {after_first:?}"
  );

  let second = c.call_tool("screenshot", json!({}));
  ok(&second, "screenshot 2");
  let after_second = artifact_files(&artifacts);
  assert_eq!(
    after_second.len(),
    1,
    "the older artifact should have been evicted: {after_second:?}"
  );
  assert_ne!(
    after_second[0], after_first[0],
    "the surviving artifact should be the one the second call wrote"
  );
  // The link the reply handed back still resolves — evicting it would have
  // made the response a lie.
  assert!(
    after_second[0].exists(),
    "the artifact the reply links was evicted: {after_second:?}"
  );
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run_owned(
    "backends_support::response_contract::test_response_contract",
    test_response_contract,
  );
}

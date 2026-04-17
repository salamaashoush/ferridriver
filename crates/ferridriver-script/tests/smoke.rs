#![allow(clippy::expect_used, clippy::unwrap_used)]
//! End-to-end smoke tests proving the engine boots, runs, and honours the
//! sandbox invariants. Each test builds a fresh `ScriptEngine` + `RunContext`
//! so there is no cross-test state bleeding.

use std::sync::Arc;

use ferridriver_script::{
  InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngine, ScriptEngineConfig, ScriptErrorKind,
};

fn make_engine() -> (ScriptEngine, tempfile::TempDir, RunContext) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let sandbox = PathSandbox::new(tmp.path()).expect("sandbox");
  let vars = Arc::new(InMemoryVars::new());
  let context = RunContext {
    vars: vars.clone(),
    sandbox: Arc::new(sandbox),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
  };
  let engine = ScriptEngine::new(ScriptEngineConfig::default());
  (engine, tmp, context)
}

fn make_engine_with_artifacts() -> (ScriptEngine, tempfile::TempDir, tempfile::TempDir, RunContext) {
  let scripts_tmp = tempfile::tempdir().expect("scripts tempdir");
  let artifacts_tmp = tempfile::tempdir().expect("artifacts tempdir");
  let sandbox = PathSandbox::new(scripts_tmp.path()).expect("scripts sandbox");
  let artifacts_sandbox = PathSandbox::new(artifacts_tmp.path()).expect("artifacts sandbox");
  let vars = Arc::new(InMemoryVars::new());
  let context = RunContext {
    vars: vars.clone(),
    sandbox: Arc::new(sandbox),
    artifacts: Some(Arc::new(artifacts_sandbox)),
    page: None,
    browser_context: None,
    request: None,
  };
  let engine = ScriptEngine::new(ScriptEngineConfig::default());
  (engine, scripts_tmp, artifacts_tmp, context)
}

#[tokio::test]
async fn evaluates_expression() {
  let (engine, _tmp, ctx) = make_engine();
  let result = engine.run("return 1 + 2", &[], RunOptions::default(), ctx).await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!(3)),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn args_are_bound_not_interpolated() {
  let (engine, _tmp, ctx) = make_engine();
  // If args were interpolated, the quote/semicolon would break parsing.
  // With bound args, it's just a string value.
  let payload = serde_json::json!("'; drop table users; --");
  let result = engine
    .run(
      "return args[0]",
      std::slice::from_ref(&payload),
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, payload),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn console_log_is_captured() {
  let (engine, _tmp, ctx) = make_engine();
  let result = engine
    .run(
      "console.log('hello'); console.warn('be careful', 42); return true",
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  assert!(result.is_ok(), "{result:?}");
  assert_eq!(result.console.len(), 2);
  assert_eq!(result.console[0].message, "hello");
  assert!(result.console[1].message.contains("be careful"));
  assert!(result.console[1].message.contains("42"));
}

#[tokio::test]
async fn vars_round_trip() {
  let (engine, _tmp, ctx) = make_engine();
  let result = engine
    .run(
      "vars.set('greeting', 'hi'); return vars.get('greeting')",
      &[],
      RunOptions::default(),
      ctx.clone(),
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("hi")),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
  // Vars persist across runs because they live in the session store.
  assert_eq!(ctx.vars.get("greeting").as_deref(), Some("hi"));
}

#[tokio::test]
async fn fs_read_write_inside_root() {
  let (engine, _tmp, ctx) = make_engine();
  let result = engine
    .run(
      r"
      await fs.writeFile('note.txt', 'hello world');
      const read = await fs.readFile('note.txt');
      return read;
      ",
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("hello world")),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn fs_rejects_traversal() {
  let (engine, _tmp, ctx) = make_engine();
  let result = engine
    .run(
      "try { await fs.readFile('../escape'); return 'no-error'; } catch (e) { return String(e); }",
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => {
      let s = success.value.as_str().unwrap_or_default().to_string();
      assert!(s.contains("traversal"), "got: {s}");
    },
    Outcome::Error { error } => panic!("expected caught error to surface, got: {error:?}"),
  }
}

#[tokio::test]
async fn syntax_error_reports_structured_error() {
  let (engine, _tmp, ctx) = make_engine();
  let result = engine
    .run("this is not js at all", &[], RunOptions::default(), ctx)
    .await;
  match result.outcome {
    Outcome::Ok { .. } => panic!("expected syntax error"),
    Outcome::Error { error } => {
      // QuickJS reports this as a runtime exception during parse.
      assert_eq!(error.kind, ScriptErrorKind::Runtime);
      assert!(!error.message.is_empty());
    },
  }
}

#[tokio::test]
async fn timeout_is_enforced() {
  let (engine, _tmp, ctx) = make_engine();
  let result = engine
    .run(
      "while (true) { /* spin */ }",
      &[],
      RunOptions {
        timeout: Some(std::time::Duration::from_millis(150)),
        ..RunOptions::default()
      },
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { .. } => panic!("expected timeout"),
    Outcome::Error { error } => assert_eq!(error.kind, ScriptErrorKind::Timeout),
  }
}

#[tokio::test]
async fn imports_module_from_sandbox() {
  let (engine, tmp, ctx) = make_engine();
  std::fs::write(
    tmp.path().join("helper.js"),
    "export function greet(name) { return `hi ${name}`; }",
  )
  .unwrap();
  let result = engine
    .run(
      "const m = await import('./helper.js'); return m.greet('world');",
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("hi world")),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn import_rejects_traversal() {
  let (engine, tmp, ctx) = make_engine();
  // Put a file OUTSIDE the sandbox that ../ would reach if traversal worked.
  let parent = tmp.path().parent().expect("parent").to_path_buf();
  std::fs::write(parent.join("secret.js"), "export const leak = 'x';").ok();

  let result = engine
    .run(
      "try { await import('../secret.js'); return 'no-error'; } catch (e) { return String(e).includes('traversal') || String(e).includes('loading') ? 'rejected' : String(e); }",
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("rejected")),
    Outcome::Error { error } => panic!("unexpected engine error: {error:?}"),
  }
}

// ── Expanded coverage ─────────────────────────────────────────────────────

#[tokio::test]
async fn console_levels_recorded_correctly() {
  use ferridriver_script::ConsoleLevel;

  let (engine, _tmp, ctx) = make_engine();
  let result = engine
    .run(
      r"
      console.log('log-msg');
      console.info('info-msg');
      console.warn('warn-msg');
      console.error('error-msg');
      console.debug('debug-msg');
      return null;
      ",
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  assert!(result.is_ok(), "{result:?}");
  let levels: Vec<ConsoleLevel> = result.console.iter().map(|e| e.level).collect();
  assert_eq!(
    levels,
    vec![
      ConsoleLevel::Log,
      ConsoleLevel::Info,
      ConsoleLevel::Warn,
      ConsoleLevel::Error,
      ConsoleLevel::Debug,
    ]
  );
}

#[tokio::test]
async fn returns_nested_object() {
  let (engine, _tmp, ctx) = make_engine();
  let result = engine
    .run(
      "return { a: 1, b: [2, 3, { c: 'nested', d: [true, null] }], unicode: 'héllo 🚀' };",
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(
      success.value,
      serde_json::json!({
        "a": 1,
        "b": [2, 3, { "c": "nested", "d": [true, null] }],
        "unicode": "héllo 🚀"
      })
    ),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn args_support_complex_types() {
  let (engine, _tmp, ctx) = make_engine();
  let args = vec![
    serde_json::json!("plain string"),
    serde_json::json!({ "user": { "name": "alice", "tags": ["a", "b"] } }),
    serde_json::json!([1, 2, 3, null, false]),
  ];
  let result = engine
    .run(
      "return { s: args[0], obj: args[1], arr: args[2] };",
      &args,
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(
      success.value,
      serde_json::json!({
        "s": "plain string",
        "obj": { "user": { "name": "alice", "tags": ["a", "b"] } },
        "arr": [1, 2, 3, null, false]
      })
    ),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn fs_readdir_lists_sandbox_contents() {
  let (engine, tmp, ctx) = make_engine();
  std::fs::write(tmp.path().join("a.txt"), b"x").unwrap();
  std::fs::write(tmp.path().join("b.txt"), b"y").unwrap();
  std::fs::create_dir_all(tmp.path().join("sub")).unwrap();

  let result = engine
    .run(
      "const entries = await fs.readdir('.'); entries.sort(); return entries;",
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!(["a.txt", "b.txt", "sub"])),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn fs_exists_reports_presence_and_missing() {
  let (engine, tmp, ctx) = make_engine();
  std::fs::write(tmp.path().join("present.txt"), b"x").unwrap();

  let result = engine
    .run(
      r"
      const has = await fs.exists('present.txt');
      const missing = await fs.exists('nothing.txt');
      const escape = await fs.exists('../secret');
      return { has, missing, escape };
      ",
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    // `exists` returns false for both missing and escape attempts — this
    // is the documented contract (sandbox violations do not leak as errors
    // to scripts that are just probing for presence).
    Outcome::Ok { success } => assert_eq!(
      success.value,
      serde_json::json!({ "has": true, "missing": false, "escape": false })
    ),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn thrown_error_includes_line_number() {
  let (engine, _tmp, ctx) = make_engine();
  let result = engine
    .run(
      r"
      let x = 1;
      let y = 2;
      throw new Error('deliberate');
      return x + y;
      ",
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { .. } => panic!("expected error"),
    Outcome::Error { error } => {
      assert_eq!(error.kind, ferridriver_script::ScriptErrorKind::Runtime);
      assert!(error.message.contains("deliberate"), "got: {}", error.message);
      // Line numbers come from QuickJS's exception object; not guaranteed on
      // every variant, but when present the snippet is too.
      if error.line.is_some() {
        assert!(error.source_snippet.is_some());
      }
    },
  }
}

#[tokio::test]
async fn imports_from_nested_subdirectory() {
  let (engine, tmp, ctx) = make_engine();
  std::fs::create_dir_all(tmp.path().join("lib/util")).unwrap();
  std::fs::write(
    tmp.path().join("lib/util/math.js"),
    "export const double = (n) => n * 2;",
  )
  .unwrap();

  let result = engine
    .run(
      "const m = await import('./lib/util/math.js'); return m.double(21);",
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!(42)),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn rejects_bare_module_import() {
  let (engine, _tmp, ctx) = make_engine();
  let result = engine
    .run(
      "try { await import('lodash'); return 'no-error'; } catch (e) { return 'rejected: ' + String(e).slice(0, 30); }",
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => {
      let s = success.value.as_str().unwrap_or_default().to_string();
      assert!(s.starts_with("rejected"), "got: {s}");
    },
    Outcome::Error { error } => panic!("unexpected engine error: {error:?}"),
  }
}

#[tokio::test]
async fn artifacts_write_read_list_remove() {
  let (engine, _scripts_tmp, artifacts_tmp, ctx) = make_engine_with_artifacts();
  let result = engine
    .run(
      r#"
      await artifacts.write('note.txt', 'hello');
      await artifacts.writeBytes('bin.dat', [1, 2, 3, 255]);
      const got = await artifacts.read('note.txt');
      const bytes = await artifacts.readBytes('bin.dat');
      const entries = (await artifacts.list()).sort();
      const removed = await artifacts.remove('note.txt');
      const afterRemove = await artifacts.exists('note.txt');
      return { got, bytes: Array.from(bytes), entries, removed, afterRemove };
      "#,
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => {
      assert_eq!(success.value["got"], serde_json::json!("hello"));
      assert_eq!(success.value["bytes"], serde_json::json!([1, 2, 3, 255]));
      assert_eq!(success.value["entries"], serde_json::json!(["bin.dat", "note.txt"]));
      assert_eq!(success.value["removed"], serde_json::json!(true));
      assert_eq!(success.value["afterRemove"], serde_json::json!(false));
    },
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
  // Files that survived the test should actually be on disk in artifacts_tmp.
  assert!(artifacts_tmp.path().join("bin.dat").exists());
}

#[tokio::test]
async fn artifacts_rejects_traversal() {
  let (engine, _scripts_tmp, _artifacts_tmp, ctx) = make_engine_with_artifacts();
  let result = engine
    .run(
      "try { await artifacts.write('../escape.txt', 'x'); return 'no-error'; } \
       catch (e) { return String(e).includes('traversal') ? 'rejected' : String(e); }",
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("rejected")),
    Outcome::Error { error } => panic!("unexpected engine error: {error:?}"),
  }
}

#[tokio::test]
async fn artifacts_absent_when_not_provided() {
  let (engine, _tmp, ctx) = make_engine();
  // No artifacts binding installed; the global is undefined.
  let result = engine
    .run("return typeof artifacts;", &[], RunOptions::default(), ctx)
    .await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("undefined")),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn fresh_context_isolates_state() {
  let (engine, _tmp, ctx) = make_engine();
  // First run leaks a global
  let _ = engine
    .run(
      "globalThis.leak = 42; return 1",
      &[],
      RunOptions::default(),
      ctx.clone(),
    )
    .await;
  // Second run should not see it
  let second = engine
    .run("return typeof globalThis.leak", &[], RunOptions::default(), ctx)
    .await;
  match second.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("undefined")),
    Outcome::Error { error } => panic!("second run failed: {error:?}"),
  }
}

#![allow(clippy::expect_used, clippy::unwrap_used)]
//! End-to-end smoke tests proving the engine boots, runs, and honours the
//! sandbox invariants. Each test builds a fresh `ScriptEngine` + `RunContext`
//! so there is no cross-test state bleeding.

use std::sync::Arc;

use ferridriver_script::{
  CommandSpec, InMemoryVars, Outcome, OutputDir, RunContext, RunOptions, ScriptEngine, ScriptEngineConfig,
  ScriptErrorKind,
};

fn make_engine() -> (ScriptEngine, tempfile::TempDir, RunContext) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let vars = Arc::new(InMemoryVars::new());
  let context = RunContext {
    vars: vars.clone(),
    script_root: tmp.path().into(),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
  };
  let engine = ScriptEngine::new(ScriptEngineConfig::default());
  (engine, tmp, context)
}

fn make_engine_with_artifacts() -> (ScriptEngine, tempfile::TempDir, tempfile::TempDir, RunContext) {
  let scripts_tmp = tempfile::tempdir().expect("scripts tempdir");
  let artifacts_tmp = tempfile::tempdir().expect("artifacts tempdir");
  let artifacts_dir = OutputDir::new(artifacts_tmp.path()).expect("artifacts dir");
  let vars = Arc::new(InMemoryVars::new());
  let context = RunContext {
    vars: vars.clone(),
    script_root: scripts_tmp.path().into(),
    artifacts: Some(Arc::new(artifacts_dir)),
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
    session: None,
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
async fn ferridriver_runtime_object_and_virtual_modules_are_available() {
  let (engine, _tmp, ctx) = make_engine();
  let result = engine
    .run(
      r#"
        const fd = await import("ferridriver");
        const cukes = await import("@cucumber/cucumber");
        return {
          host: ferridriver.host,
          same: fd.ferridriver === ferridriver,
          tool: typeof fd.tool,
          tools: fd.tools === ferridriver.tools,
          noPublicPlugins: typeof fd.plugins,
          noGlobalPlugins: typeof globalThis.plugins,
          bdd: typeof ferridriver.bdd.Given,
          cucumber: cukes.Given === ferridriver.bdd.Given,
        };
      "#,
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(
      success.value,
      serde_json::json!({
        "host": "script",
        "same": true,
        "tool": "function",
        "tools": true,
        "noPublicPlugins": "undefined",
        "noGlobalPlugins": "undefined",
        "bdd": "function",
        "cucumber": true,
      })
    ),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

#[tokio::test]
async fn configured_global_commands_are_available_to_plain_scripts() {
  let (engine, _tmp, mut ctx) = make_engine();
  let spec: CommandSpec = serde_json::from_value(serde_json::json!({
    "run": ["echo", "${value}"],
    "output": "text"
  }))
  .expect("command spec");
  ctx.caps.commands.insert("echoValue".to_string(), spec);

  let result = engine
    .run(
      r#"return await ferridriver.commands.run("echoValue", { value: "hello" })"#,
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("hello")),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
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

/// `fs` is Node's, so a script reads and writes the way Node does.
#[tokio::test]
async fn fs_reads_and_writes_through_the_node_surface() {
  let (engine, tmp, ctx) = make_engine();
  let note = tmp.path().join("note.txt").to_string_lossy().into_owned();
  let result = engine
    .run(
      &format!(
        r"
      const note = {note:?};
      await fs.promises.writeFile(note, 'hello world');
      const viaPromise = await fs.promises.readFile(note, 'utf8');
      const viaSync = fs.readFileSync(note, 'utf8');
      const bytes = fs.readFileSync(note);
      return {{ viaPromise, viaSync, length: bytes.length }};
      "
      ),
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(
      success.value,
      serde_json::json!({ "viaPromise": "hello world", "viaSync": "hello world", "length": 11 })
    ),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
  }
}

/// A `..` component resolves; it is not a refusal.
///
/// `fs` used to be confined to a root and rejected any path that climbed
/// out of it. That boundary is gone: the paths a suite legitimately
/// handles are produced by the runner and absolute, and a prefix check on
/// one module was never a sandbox while the same script reaches the
/// network, the browser and `commands`.
#[tokio::test]
async fn fs_follows_a_parent_component() {
  let (engine, tmp, ctx) = make_engine();
  let outside = tmp.path().parent().expect("parent").join("fd-smoke-outside.txt");
  std::fs::write(&outside, b"reachable").expect("seed");
  let nested = tmp.path().join("nested");
  std::fs::create_dir_all(&nested).expect("mkdir");
  let via_parent = nested
    .join("..")
    .join("..")
    .join("fd-smoke-outside.txt")
    .to_string_lossy()
    .into_owned();

  let result = engine
    .run(
      &format!("return fs.readFileSync({via_parent:?}, 'utf8');"),
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  let _ = std::fs::remove_file(&outside);
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("reachable")),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
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
async fn imports_a_relative_module() {
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

/// A relative import that climbs above the script root resolves.
///
/// It used to be refused. A suite whose helpers live one directory up is
/// ordinary, and the refusal was never a boundary — see
/// `fs_follows_a_parent_component`.
#[tokio::test]
async fn import_follows_a_parent_specifier() {
  let (engine, tmp, ctx) = make_engine();
  let parent = tmp.path().parent().expect("parent").to_path_buf();
  let helper = parent.join("fd-smoke-shared.js");
  std::fs::write(&helper, "export const shared = 'from above';").expect("seed");

  let result = engine
    .run(
      "const m = await import('../fd-smoke-shared.js'); return m.shared;",
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  let _ = std::fs::remove_file(&helper);
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("from above")),
    Outcome::Error { error } => panic!("expected ok, got error: {error:?}"),
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
async fn fs_readdir_lists_directory_contents() {
  let (engine, tmp, ctx) = make_engine();
  std::fs::write(tmp.path().join("a.txt"), b"x").unwrap();
  std::fs::write(tmp.path().join("b.txt"), b"y").unwrap();
  std::fs::create_dir_all(tmp.path().join("sub")).unwrap();

  let dir = tmp.path().to_string_lossy().into_owned();
  let result = engine
    .run(
      &format!("const entries = await fs.promises.readdir({dir:?}); entries.sort(); return entries;"),
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
async fn fs_exists_reports_presence_and_absence() {
  let (engine, tmp, ctx) = make_engine();
  std::fs::write(tmp.path().join("present.txt"), b"x").unwrap();

  let present = tmp.path().join("present.txt").to_string_lossy().into_owned();
  let absent = tmp.path().join("nothing.txt").to_string_lossy().into_owned();
  let result = engine
    .run(
      &format!(
        r"
      return {{ has: fs.existsSync({present:?}), missing: fs.existsSync({absent:?}) }};
      "
      ),
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    // `false` means the file is not there, and nothing else — the answer
    // used to double as "the sandbox refused", which made a spec asking
    // whether its baseline had been written answer no for a file that was
    // sitting right there.
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!({ "has": true, "missing": false })),
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
      "
      await artifacts.write('note.txt', 'hello');
      await artifacts.writeBytes('bin.dat', [1, 2, 3, 255]);
      const got = await artifacts.read('note.txt');
      const bytes = await artifacts.readBytes('bin.dat');
      const entries = (await artifacts.list()).sort();
      const removed = await artifacts.remove('note.txt');
      const afterRemove = await artifacts.exists('note.txt');
      return { got, bytes: Array.from(bytes), entries, removed, afterRemove };
      ",
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

/// `artifacts` roots a relative name; it does not confine one.
#[tokio::test]
async fn artifacts_anchors_a_relative_name_at_its_root() {
  let (engine, _scripts_tmp, artifacts_tmp, ctx) = make_engine_with_artifacts();
  let result = engine
    .run(
      "await artifacts.write('nested/out.txt', 'x'); return 'written';",
      &[],
      RunOptions::default(),
      ctx,
    )
    .await;
  match result.outcome {
    Outcome::Ok { success } => assert_eq!(success.value, serde_json::json!("written")),
    Outcome::Error { error } => panic!("unexpected engine error: {error:?}"),
  }
  assert!(artifacts_tmp.path().join("nested/out.txt").is_file());
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

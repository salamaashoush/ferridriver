#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Script-layer integration tests for the `expect()` global.
//!
//! Covers Jest-style value matchers, asymmetric matchers (`expect.any`,
//! `expect.objectContaining`, ...), `expect.poll`, and `expect(fn).toThrow`.
//! Web-first matchers (`toBeVisible`, `toHaveText`, ...) are exercised
//! end-to-end in the CLI backend matrix
//! (`crates/ferridriver-cli/tests/backends.rs`) since they need a live
//! browser.

use std::sync::Arc;

use ferridriver_script::{
  ExtensionHost, InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptCaps, ScriptEngine,
  ScriptEngineConfig,
};

fn make_engine() -> (ScriptEngine, tempfile::TempDir, RunContext) {
  let tmp = tempfile::tempdir().expect("tempdir");
  let sandbox = PathSandbox::new(tmp.path()).expect("sandbox");
  let vars = Arc::new(InMemoryVars::new());
  let context = RunContext {
    vars,
    sandbox: Arc::new(sandbox),
    artifacts: None,
    page: None,
    browser_context: None,
    request: None,
    browser: None,
    extensions: Vec::new(),
    host: ExtensionHost::Script,
    caps: ScriptCaps::default(),
    session: None,
  };
  let engine = ScriptEngine::new(ScriptEngineConfig::default());
  (engine, tmp, context)
}

async fn run_ok(src: &str) -> serde_json::Value {
  let (engine, _tmp, ctx) = make_engine();
  let result = engine.run(src, &[], RunOptions::default(), ctx).await;
  match result.outcome {
    Outcome::Ok { success } => success.value,
    Outcome::Error { error } => panic!("expected ok, got: {error:?}\nscript:\n{src}"),
  }
}

async fn run_err(src: &str) -> String {
  let (engine, _tmp, ctx) = make_engine();
  let result = engine.run(src, &[], RunOptions::default(), ctx).await;
  match result.outcome {
    Outcome::Ok { success } => panic!("expected error, got ok: {success:?}\nscript:\n{src}"),
    Outcome::Error { error } => format!("{error:?}"),
  }
}

#[tokio::test]
async fn to_be_primitive_pass() {
  run_ok("expect(1).toBe(1); return 'ok'").await;
}

#[tokio::test]
async fn to_be_primitive_fail_throws() {
  let err = run_err("expect(1).toBe(2); return 'unreached'").await;
  assert!(err.contains("toBe"), "expected toBe in error, got: {err}");
}

async fn run_err_structured(src: &str) -> ferridriver_script::ScriptError {
  let (engine, _tmp, ctx) = make_engine();
  let result = engine.run(src, &[], RunOptions::default(), ctx).await;
  match result.outcome {
    Outcome::Ok { success } => panic!("expected error, got ok: {success:?}"),
    Outcome::Error { error } => error,
  }
}

#[tokio::test]
async fn js_stack_is_captured_on_failure() {
  // Multi-line script so the stack carries a meaningful line number;
  // any thrown error from QuickJS should populate ScriptError.stack
  // with `at ... (<source>:N:M)` frames.
  let err = run_err_structured("function inner() { expect(1).toBe(2); }\ninner();\nreturn 'unreached';").await;
  let stack = err.stack.clone().unwrap_or_default();
  assert!(!stack.is_empty(), "stack must be populated; full err: {err:?}");
  assert!(stack.contains("at "), "stack lacks frame prefix: {stack}");
}

#[tokio::test]
async fn to_equal_failure_message_has_unified_diff() {
  // A failing toEqual must surface a multi-line `Diff:` section with
  // unified-diff `+`/`-` markers in the JS-visible error. Proves the
  // Rust-side similar-based diff round-trips through QuickJS into the
  // thrown error message.
  let err = run_err("expect({a: 1, b: 'x'}).toEqual({a: 2, b: 'x'}); return 'unreached'").await;
  assert!(err.contains("toEqual"), "no toEqual in message: {err}");
  assert!(err.contains("Diff:"), "no Diff section in message: {err}");
  assert!(err.contains('-'), "no '-' marker in message: {err}");
  assert!(err.contains('+'), "no '+' marker in message: {err}");
}

#[tokio::test]
async fn to_equal_nested_pass() {
  run_ok("expect({a: [1, 2]}).toEqual({a: [1, 2]}); return 'ok'").await;
}

#[tokio::test]
async fn to_equal_with_asymmetric_any_number() {
  run_ok("expect({id: 7, name: 'n'}).toEqual({id: expect.any(Number), name: 'n'}); return 'ok'").await;
}

#[tokio::test]
async fn to_equal_with_asymmetric_object_containing() {
  run_ok(
    "const actual = {a: 1, b: 2, c: 3}; \
     expect(actual).toEqual(expect.objectContaining({a: 1, c: 3})); \
     return 'ok'",
  )
  .await;
}

#[tokio::test]
async fn to_equal_with_asymmetric_array_containing() {
  run_ok("expect([1, 2, 3, 4]).toEqual(expect.arrayContaining([2, 3])); return 'ok'").await;
}

#[tokio::test]
async fn to_equal_with_asymmetric_string_matching_regex() {
  run_ok("expect('Hello World').toEqual(expect.stringMatching(/hello/i)); return 'ok'").await;
}

#[tokio::test]
async fn to_equal_with_asymmetric_string_containing() {
  run_ok("expect('Hello World').toEqual(expect.stringContaining('World')); return 'ok'").await;
}

#[tokio::test]
async fn asymmetric_not_inverts() {
  run_ok("expect('Hello').toEqual(expect.not.stringContaining('Bye')); return 'ok'").await;
}

#[tokio::test]
async fn to_be_close_to_default_digits() {
  run_ok("expect(0.1 + 0.2).toBeCloseTo(0.3); return 'ok'").await;
}

#[tokio::test]
async fn to_be_close_to_explicit_digits() {
  run_ok("expect(3.14159).toBeCloseTo(3.14, 2); return 'ok'").await;
}

#[tokio::test]
async fn not_inverts_to_be() {
  run_ok("expect(1).not.toBe(2); return 'ok'").await;
}

#[tokio::test]
async fn not_invert_fail_throws() {
  let err = run_err("expect(1).not.toBe(1); return 'unreached'").await;
  assert!(err.contains("toBe"), "expected toBe in error, got: {err}");
}

#[tokio::test]
async fn to_contain_array_and_string() {
  run_ok("expect([1, 2, 3]).toContain(2); return 'ok'").await;
  run_ok("expect('hello world').toContain('world'); return 'ok'").await;
}

#[tokio::test]
async fn to_have_length_array_and_string() {
  run_ok("expect([1, 2, 3]).toHaveLength(3); return 'ok'").await;
  run_ok("expect('abcd').toHaveLength(4); return 'ok'").await;
}

#[tokio::test]
async fn to_have_property_dot_path_with_value() {
  run_ok("expect({a: {b: 42}}).toHaveProperty('a.b', 42); return 'ok'").await;
}

#[tokio::test]
async fn to_have_property_array_path_index() {
  run_ok("expect({arr: [10, 20]}).toHaveProperty(['arr', 1], 20); return 'ok'").await;
}

#[tokio::test]
async fn to_match_substring() {
  run_ok("expect('hello world').toMatch('world'); return 'ok'").await;
}

#[tokio::test]
async fn to_match_regex() {
  run_ok("expect('hello world').toMatch(/^hello/); return 'ok'").await;
}

#[tokio::test]
async fn to_match_object_subset() {
  run_ok("expect({a: 1, b: 2, c: 3}).toMatchObject({a: 1, c: 3}); return 'ok'").await;
}

#[tokio::test]
async fn to_be_instance_of_builtins() {
  run_ok("expect([1, 2, 3]).toBeInstanceOf(Array); return 'ok'").await;
}

// ── the live subject ─────────────────────────────────────────────────
//
// `expect(...)` keeps the value it was handed, so the matchers Playwright
// defines over the value itself answer as they do upstream. A JSON
// snapshot of the subject can express none of the assertions below.

#[tokio::test]
async fn to_be_is_object_is() {
  run_ok("const a = {v:1}; expect(a).toBe(a); return 'ok'").await;
  run_ok("expect({v:1}).not.toBe({v:1}); return 'ok'").await;
  run_ok("expect({v:1}).toEqual({v:1}); return 'ok'").await;
  run_ok("expect(NaN).toBe(NaN); expect(0).not.toBe(-0); return 'ok'").await;
  let err = run_err("expect({v:1}).toBe({v:1}); return 'unreached'").await;
  assert!(
    err.contains("replace \\\"toBe\\\" with \\\"toEqual\\\"") || err.contains("replace \"toBe\" with \"toEqual\""),
    "a failed toBe over equal shapes must name toEqual, got: {err}"
  );
}

#[tokio::test]
async fn a_function_is_a_value_subject() {
  run_ok("const f = (a,b) => a; expect(f).toBe(f); return 'ok'").await;
  run_ok("const f = () => 1; expect(f).not.toBe(() => 1); return 'ok'").await;
  run_ok("expect((a,b) => a).toHaveLength(2); return 'ok'").await;
  run_ok("expect(() => 1).toBeInstanceOf(Function); return 'ok'").await;
  run_ok("expect(() => 1).toBeTruthy(); return 'ok'").await;
}

#[tokio::test]
async fn undefined_is_not_null() {
  run_ok("expect(undefined).not.toBeNull(); return 'ok'").await;
  run_ok("expect(null).not.toBeUndefined(); return 'ok'").await;
  run_ok("expect(null).toBeDefined(); return 'ok'").await;
  run_ok("expect(undefined).not.toBeDefined(); return 'ok'").await;
  run_ok("expect(undefined).not.toBe(null); return 'ok'").await;
}

#[tokio::test]
async fn to_be_nan_needs_a_number() {
  run_ok("expect(NaN).toBeNaN(); return 'ok'").await;
  run_ok("expect('NaN').not.toBeNaN(); return 'ok'").await;
  run_ok("expect(null).not.toBeNaN(); return 'ok'").await;
}

#[tokio::test]
async fn to_be_instance_of_walks_the_prototype_chain() {
  run_ok("class A {}; class B extends A {}; expect(new B()).toBeInstanceOf(A); return 'ok'").await;
  run_ok("class A {}; expect(new A()).toBeInstanceOf(Object); return 'ok'").await;
  run_ok("class A {}; expect(new A()).not.toBeInstanceOf(Error); return 'ok'").await;
  let err = run_err("expect(1).toBeInstanceOf(5); return 'unreached'").await;
  assert!(err.contains("must be a function"), "expected a TypeError, got: {err}");
}

#[tokio::test]
async fn to_contain_compares_items_strictly() {
  run_ok("const o = {}; expect([o]).toContain(o); return 'ok'").await;
  run_ok("expect([{a:1}]).not.toContain({a:1}); return 'ok'").await;
  run_ok("expect([{a:1}]).toContainEqual({a:1}); return 'ok'").await;
  run_ok("expect(new Set(['a'])).toContain('a'); return 'ok'").await;
}

#[tokio::test]
async fn to_contain_misuse_is_a_type_error_under_not() {
  for src in [
    "expect(null).toContain(1); return 'unreached'",
    "expect(null).not.toContain(1); return 'unreached'",
    "expect('hi').toContain(1); return 'unreached'",
    "expect(7).toContain(1); return 'unreached'",
  ] {
    let err = run_err(src).await;
    assert!(
      err.contains("TypeError"),
      "expected a TypeError from `{src}`, got: {err}"
    );
  }
}

#[tokio::test]
async fn to_have_length_reads_the_live_length() {
  run_ok("expect(new Uint8Array(4)).toHaveLength(4); return 'ok'").await;
  // UTF-16 code units, like `.length` in the engine.
  run_ok("expect('a\\u{1F600}').toHaveLength(3); return 'ok'").await;
  let err = run_err("expect({a:1}).toHaveLength(1); return 'unreached'").await;
  assert!(err.contains("length property"), "expected a TypeError, got: {err}");
}

#[tokio::test]
async fn expect_takes_a_custom_message() {
  let err = run_err("expect(1, 'ids match').toBe(2); return 'unreached'").await;
  assert!(err.contains("ids match"), "custom message missing: {err}");
  let err = run_err("expect(1, { message: 'ids match' }).toBe(2); return 'unreached'").await;
  assert!(err.contains("ids match"), "custom message missing: {err}");
}

#[tokio::test]
async fn the_core_builtin_list_matches_the_shipped_matchers() {
  // `expect.extend`'s shadowing rule reads a list in ferridriver-expect
  // while the binding installs the real methods; this is what keeps the
  // two from drifting.
  let names = run_ok(
    "const proto = Object.getPrototypeOf(expect(1));
     return Object.getOwnPropertyNames(proto).filter(n => n.startsWith('to')).sort();",
  )
  .await;
  let shipped: Vec<String> = serde_json::from_value(names).expect("names");
  let listed: Vec<String> = ferridriver_expect::BUILTIN_MATCHER_NAMES
    .iter()
    .map(|s| (*s).to_string())
    .collect();
  assert_eq!(
    shipped, listed,
    "ferridriver_expect::BUILTIN_MATCHER_NAMES must list exactly the matchers the class ships"
  );
}

// ── structural equality over live values ─────────────────────────────

#[tokio::test]
async fn to_equal_compares_maps_and_sets() {
  run_ok("expect(new Map([['a', 1]])).toEqual(new Map([['a', 1]])); return 'ok'").await;
  run_ok("expect(new Map([['a', 1]])).not.toEqual(new Map([['a', 2]])); return 'ok'").await;
  run_ok("expect(new Map([['a', 1]])).not.toEqual(new Map()); return 'ok'").await;
  run_ok("expect(new Set([1, 2])).toEqual(new Set([2, 1])); return 'ok'").await;
  run_ok("expect(new Set([1, 2])).not.toEqual(new Set([1, 3])); return 'ok'").await;
  // A Map is not the plain object with the same entries.
  run_ok("expect(new Map([['a', 1]])).not.toEqual({ a: 1 }); return 'ok'").await;
  // Nested, and with an asymmetric matcher inside.
  run_ok("expect({m: new Map([['a', {b: 1}]])}).toEqual({m: new Map([['a', {b: expect.any(Number)}]])}); return 'ok'")
    .await;
}

#[tokio::test]
async fn to_equal_compares_dates_regexps_and_errors() {
  run_ok("expect(new Date(5)).toEqual(new Date(5)); return 'ok'").await;
  run_ok("expect(new Date(5)).not.toEqual(new Date(6)); return 'ok'").await;
  run_ok("expect(new Date(NaN)).toEqual(new Date(NaN)); return 'ok'").await;
  run_ok("expect(/ab+/gi).toEqual(/ab+/gi); return 'ok'").await;
  run_ok("expect(/ab+/g).not.toEqual(/ab+/i); return 'ok'").await;
  run_ok("expect(new Error('boom')).toEqual(new Error('boom')); return 'ok'").await;
  run_ok("expect(new Error('boom')).not.toEqual(new Error('other')); return 'ok'").await;
  run_ok("expect(new RangeError('x')).not.toEqual(new Error('x')); return 'ok'").await;
}

#[tokio::test]
async fn to_equal_ignores_undefined_keys_and_to_strict_equal_does_not() {
  run_ok("expect({ a: 1, b: undefined }).toEqual({ a: 1 }); return 'ok'").await;
  run_ok("expect({ a: 1 }).toEqual({ a: 1, b: undefined }); return 'ok'").await;
  run_ok("expect({ a: 1, b: undefined }).not.toStrictEqual({ a: 1 }); return 'ok'").await;
  run_ok("expect({ a: 1, b: undefined }).toStrictEqual({ a: 1, b: undefined }); return 'ok'").await;
}

#[tokio::test]
async fn to_strict_equal_compares_the_class() {
  run_ok(
    "class Point { constructor() { this.x = 1; } }
     expect(new Point()).toEqual({ x: 1 });
     expect(new Point()).not.toStrictEqual({ x: 1 });
     expect(new Point()).toStrictEqual(new Point());
     return 'ok'",
  )
  .await;
}

#[tokio::test]
async fn to_strict_equal_sees_array_holes() {
  run_ok("expect([, 1]).toEqual([undefined, 1]); return 'ok'").await;
  run_ok("expect([, 1]).not.toStrictEqual([undefined, 1]); return 'ok'").await;
  run_ok("expect([, 1]).toStrictEqual([, 1]); return 'ok'").await;
}

#[tokio::test]
async fn to_equal_compares_bigints_and_typed_arrays() {
  run_ok("expect(1n).toEqual(1n); return 'ok'").await;
  run_ok("expect(1n).not.toEqual(2n); return 'ok'").await;
  run_ok("expect(new Uint8Array([1, 2])).toEqual(new Uint8Array([1, 2])); return 'ok'").await;
  run_ok("expect(new Uint8Array([1, 2])).not.toEqual(new Uint8Array([1, 3])); return 'ok'").await;
}

#[tokio::test]
async fn a_cyclic_structure_terminates() {
  run_ok(
    "const a = { name: 'a' }; a.self = a;
     const b = { name: 'a' }; b.self = b;
     expect(a).toEqual(b);
     const c = { name: 'c' }; c.self = c;
     expect(a).not.toEqual(c);
     return 'ok'",
  )
  .await;
}

#[tokio::test]
async fn to_have_property_walks_the_live_value() {
  run_ok("expect({ a: { b: 42 } }).toHaveProperty('a.b', 42); return 'ok'").await;
  run_ok("expect({ arr: [10, 20] }).toHaveProperty(['arr', 1], 20); return 'ok'").await;
  run_ok("expect({ m: new Date(3) }).toHaveProperty('m', new Date(3)); return 'ok'").await;
  run_ok(
    "class Holder { get computed() { return 7; } }
     expect(new Holder()).toHaveProperty('computed', 7);
     return 'ok'",
  )
  .await;
  run_ok("expect({ a: 1 }).not.toHaveProperty('b'); return 'ok'").await;
}

#[tokio::test]
async fn to_contain_equal_uses_deep_equality_over_live_items() {
  run_ok("expect([{ a: 1 }]).toContainEqual({ a: 1 }); return 'ok'").await;
  run_ok("expect([new Date(1)]).toContainEqual(new Date(1)); return 'ok'").await;
  run_ok("expect(new Set([{ a: 1 }])).toContainEqual({ a: 1 }); return 'ok'").await;
  run_ok("expect([{ a: 1 }]).not.toContainEqual({ a: 2 }); return 'ok'").await;
}

// ── expect.extend ────────────────────────────────────────────────────

const WITHIN: &str = "const within = { toBeWithin(received, lo, hi) { \
   const pass = received >= lo && received <= hi; \
   return { pass, message: () => `expected ${received} ${this.isNot ? 'not ' : ''}to be within ${lo}..${hi}` }; } };";

#[tokio::test]
async fn extend_adds_a_matcher_to_the_returned_expect() {
  run_ok(&format!(
    "{WITHIN} const e = expect.extend(within); e(5).toBeWithin(0, 10); return 'ok'"
  ))
  .await;
  let err = run_err(&format!(
    "{WITHIN} const e = expect.extend(within); e(50).toBeWithin(0, 10); return 'unreached'"
  ))
  .await;
  assert!(err.contains("to be within 0..10"), "matcher message missing: {err}");
}

#[tokio::test]
async fn a_custom_matcher_inverts_and_reads_its_context() {
  run_ok(&format!(
    "{WITHIN} const e = expect.extend(within); e(50).not.toBeWithin(0, 10); return 'ok'"
  ))
  .await;
  let err = run_err(&format!(
    "{WITHIN} const e = expect.extend(within); e(5).not.toBeWithin(0, 10); return 'unreached'"
  ))
  .await;
  assert!(err.contains("not to be within"), "this.isNot not observed: {err}");
}

#[tokio::test]
async fn extend_publishes_a_new_name_on_the_original_expect_too() {
  // Playwright's legacy behavior: a non-builtin name is usable through
  // the expect `extend` was called on, without capturing the result.
  run_ok(&format!(
    "{WITHIN} expect.extend(within); expect(5).toBeWithin(0, 10); return 'ok'"
  ))
  .await;
}

#[tokio::test]
async fn extend_never_shadows_a_builtin_on_the_original_expect() {
  run_ok(
    "const e = expect.extend({ toBe(received, expected) { return { pass: true, message: () => 'always' }; } });
     e(1).toBe(2);
     let threw = false;
     try { expect(1).toBe(2); } catch { threw = true; }
     if (!threw) throw new Error('the original expect lost its built-in toBe');
     return 'ok'",
  )
  .await;
}

#[tokio::test]
async fn extend_refuses_a_non_function() {
  let err = run_err("expect.extend({ toBeX: 5 }); return 'unreached'").await;
  assert!(
    err.contains("is not a valid matcher") && err.contains("number"),
    "expected the extend TypeError, got: {err}"
  );
}

#[tokio::test]
async fn a_custom_matcher_may_be_async_and_still_fails() {
  run_ok(
    "const e = expect.extend({ async toBeLate(received) { return { pass: received === 1, message: () => 'late' }; } });
     await e(1).toBeLate();
     return 'ok'",
  )
  .await;
  let err = run_err(
    "const e = expect.extend({ async toBeLate(received) { return { pass: false, message: () => 'late' }; } });
     await e(1).toBeLate();
     return 'unreached'",
  )
  .await;
  assert!(err.contains("late"), "async matcher message missing: {err}");
}

#[tokio::test]
async fn a_custom_matcher_returning_junk_says_so() {
  let err = run_err("const e = expect.extend({ toBeX() { return 5; } }); e(1).toBeX(); return 'unreached'").await;
  assert!(
    err.contains("Unexpected return from a matcher function"),
    "expected the result validation, got: {err}"
  );
}

#[tokio::test]
async fn configure_returns_a_new_expect() {
  run_ok(
    "const quiet = expect.configure({ message: 'ids match' });
     let msg = '';
     try { quiet(1).toBe(2); } catch (e) { msg = String(e.message); }
     if (!msg.includes('ids match')) throw new Error('configured message missing: ' + msg);
     let plain = '';
     try { expect(1).toBe(2); } catch (e) { plain = String(e.message); }
     if (plain.includes('ids match')) throw new Error('the original expect was mutated');
     return 'ok'",
  )
  .await;
}

#[tokio::test]
async fn a_custom_matcher_observes_the_configured_timeout() {
  run_ok(
    "const e = expect.configure({ timeout: 1234 }).extend({
       toSeeTimeout(received) { return { pass: this.timeout === 1234, message: () => 'timeout was ' + this.timeout }; },
     });
     e(1).toSeeTimeout();
     return 'ok'",
  )
  .await;
}

#[tokio::test]
async fn soft_is_a_getter_returning_an_expect() {
  run_ok("if (typeof expect.soft !== 'function') throw new Error('soft is not callable'); return 'ok'").await;
  run_ok("expect.soft(1).toBe(1); return 'ok'").await;
  run_ok("if (expect.soft.soft !== expect.soft.soft.soft) { } return 'ok'").await;
  run_ok("await expect.soft.poll(() => 1, { timeout: 500, intervals: [5] }).toBe(1); return 'ok'").await;
}

#[tokio::test]
async fn get_state_answers_an_object() {
  run_ok("if (typeof expect.getState() !== 'object') throw new Error('no state'); return 'ok'").await;
}

#[tokio::test]
async fn merge_expects_exposes_every_matcher() {
  run_ok(
    "const a = expect.extend({ toBeA(received) { return { pass: received === 'a', message: () => 'not a' }; } });
     const b = expect.extend({ toBeB(received) { return { pass: received === 'b', message: () => 'not b' }; } });
     const both = mergeExpects(a, b);
     both('a').toBeA();
     both('b').toBeB();
     return 'ok'",
  )
  .await;
}

#[tokio::test]
async fn a_custom_matcher_reaches_the_settled_chain() {
  run_ok(
    "const e = expect.extend({ toBeA(received) { return { pass: received === 'a', message: () => 'not a' }; } });
     await e(Promise.resolve('a')).resolves.toBeA();
     await e(Promise.resolve('b')).resolves.not.toBeA();
     return 'ok'",
  )
  .await;
}

#[tokio::test]
async fn a_custom_matcher_is_also_an_asymmetric_matcher() {
  // Playwright publishes every registered matcher as an asymmetric one,
  // so it can stand in for a value inside a structural comparison.
  run_ok(
    "const e = expect.extend({ toBeEven(received) { return { pass: received % 2 === 0, message: () => 'odd' }; } });
     e({ n: 4 }).toEqual({ n: e.toBeEven() });
     e([2, 4]).toEqual([e.toBeEven(), e.toBeEven()]);
     e({ n: 3 }).toEqual({ n: e.not.toBeEven() });
     e({ a: { n: 8 } }).toMatchObject({ a: { n: e.toBeEven() } });
     return 'ok'",
  )
  .await;
  let err = run_err(
    "const e = expect.extend({ toBeEven(received) { return { pass: received % 2 === 0, message: () => 'odd' }; } });
     e({ n: 3 }).toEqual({ n: e.toBeEven() });
     return 'unreached'",
  )
  .await;
  assert!(err.contains("toEqual"), "expected a toEqual failure, got: {err}");
}

#[tokio::test]
async fn an_async_custom_matcher_cannot_be_asymmetric() {
  let err = run_err(
    "const e = expect.extend({ async toBeEven(received) { return { pass: true, message: () => '' }; } });
     e({ n: 4 }).toEqual({ n: e.toBeEven() });
     return 'unreached'",
  )
  .await;
  // It fails the comparison rather than silently passing.
  assert!(err.contains("toEqual"), "expected a toEqual failure, got: {err}");
}

#[tokio::test]
async fn array_of_matches_every_item() {
  run_ok("expect([1, 2, 3]).toEqual(expect.arrayOf(expect.any(Number))); return 'ok'").await;
  run_ok("expect([]).toEqual(expect.arrayOf(expect.any(Number))); return 'ok'").await;
  run_ok("expect([1, 'two']).toEqual(expect.not.arrayOf(expect.any(Number))); return 'ok'").await;
  run_ok("expect({items: [1, 2]}).toMatchObject({items: expect.arrayOf(expect.any(Number))}); return 'ok'").await;
  let err = run_err("expect([1, 'two']).toEqual(expect.arrayOf(expect.any(Number))); return 'unreached'").await;
  assert!(err.contains("toEqual"), "expected a toEqual failure, got: {err}");
}

// ── .resolves / .rejects ─────────────────────────────────────────────

#[tokio::test]
async fn resolves_runs_the_matcher_on_the_resolved_value() {
  run_ok("await expect(Promise.resolve(1)).resolves.toBe(1); return 'ok'").await;
  run_ok("await expect(Promise.resolve({a:1})).resolves.toEqual({a:1}); return 'ok'").await;
  run_ok("await expect(Promise.resolve(1)).resolves.not.toBe(2); return 'ok'").await;
  // A function returning a promise is accepted, as upstream.
  run_ok("await expect(async () => 7).resolves.toBe(7); return 'ok'").await;
}

#[tokio::test]
async fn rejects_runs_the_matcher_on_the_reason() {
  run_ok("await expect(Promise.reject(new Error('boom'))).rejects.toThrow('boom'); return 'ok'").await;
  run_ok("await expect(Promise.reject(new RangeError('r'))).rejects.toThrow(RangeError); return 'ok'").await;
  run_ok("await expect(Promise.reject('plain')).rejects.toBe('plain'); return 'ok'").await;
  run_ok("await expect(Promise.reject(new Error('boom'))).rejects.not.toThrow('other'); return 'ok'").await;
}

#[tokio::test]
async fn settling_the_wrong_way_names_which_way() {
  let err = run_err("await expect(Promise.reject(new Error('x'))).resolves.toBe(1); return 'unreached'").await;
  assert!(
    err.contains("rejected instead of resolved"),
    "expected the resolves diagnosis, got: {err}"
  );
  let err = run_err("await expect(Promise.resolve(1)).rejects.toBe(1); return 'unreached'").await;
  assert!(
    err.contains("resolved instead of rejected"),
    "expected the rejects diagnosis, got: {err}"
  );
}

#[tokio::test]
async fn a_settled_chain_needs_a_promise() {
  let err = run_err("await expect(1).resolves.toBe(1); return 'unreached'").await;
  assert!(
    err.contains("promise, or a function returning a promise"),
    "expected the promise requirement, got: {err}"
  );
}

#[tokio::test]
async fn a_settled_matcher_still_fails_normally() {
  let err = run_err("await expect(Promise.resolve(1)).resolves.toBe(2); return 'unreached'").await;
  assert!(err.contains("toBe"), "expected a toBe failure, got: {err}");
}

#[tokio::test]
async fn poll_refuses_a_settled_chain() {
  let err = run_err("await expect.poll(() => 1).resolves.toBe(1); return 'unreached'").await;
  assert!(
    err.contains("does not support") && err.contains("resolves"),
    "expected the poll refusal, got: {err}"
  );
}

#[tokio::test]
async fn expect_poll_compares_identity() {
  run_ok(
    "const wanted = {}; let n = 0;
     await expect.poll(() => { n += 1; return n >= 2 ? wanted : {}; }, { timeout: 2000, intervals: [5] }).toBe(wanted);
     return 'ok'",
  )
  .await;
}

#[tokio::test]
async fn to_throw_sync() {
  run_ok("await expect(() => { throw new Error('boom'); }).toThrow(); return 'ok'").await;
}

#[tokio::test]
async fn to_throw_substring_match() {
  run_ok("await expect(() => { throw new Error('out of range'); }).toThrow('out of range'); return 'ok'").await;
}

#[tokio::test]
async fn to_throw_regex_match() {
  run_ok("await expect(() => { throw new Error('boom42'); }).toThrow(/boom\\d+/); return 'ok'").await;
}

#[tokio::test]
async fn to_throw_class_match() {
  run_ok("await expect(() => { throw new RangeError('bad'); }).toThrow(RangeError); return 'ok'").await;
}

#[tokio::test]
async fn to_throw_no_throw_fails() {
  let err = run_err("await expect(() => 42).toThrow(); return 'unreached'").await;
  assert!(err.contains("toThrow"), "expected toThrow in error, got: {err}");
}

#[tokio::test]
async fn not_to_throw_passes_when_no_throw() {
  run_ok("await expect(() => 42).not.toThrow(); return 'ok'").await;
}

#[tokio::test]
async fn to_throw_async_promise() {
  run_ok("await expect(async () => { throw new Error('async boom'); }).toThrow('async boom'); return 'ok'").await;
}

#[tokio::test]
async fn truthy_and_falsy() {
  run_ok("expect(1).toBeTruthy(); return 'ok'").await;
  run_ok("expect(0).toBeFalsy(); return 'ok'").await;
  run_ok("expect('').toBeFalsy(); return 'ok'").await;
  run_ok("expect(null).toBeFalsy(); return 'ok'").await;
}

#[tokio::test]
async fn null_and_undefined() {
  run_ok("expect(null).toBeNull(); return 'ok'").await;
  run_ok("expect(undefined).toBeUndefined(); return 'ok'").await;
  run_ok("expect(1).toBeDefined(); return 'ok'").await;
}

#[tokio::test]
async fn greater_less_than() {
  run_ok("expect(5).toBeGreaterThan(3); return 'ok'").await;
  run_ok("expect(3).toBeGreaterThanOrEqual(3); return 'ok'").await;
  run_ok("expect(2).toBeLessThan(3); return 'ok'").await;
  run_ok("expect(3).toBeLessThanOrEqual(3); return 'ok'").await;
}

#[tokio::test]
async fn poll_to_equal_succeeds_after_a_few_polls() {
  // The generator returns increasing values; toEqual(3) becomes true
  // on the 3rd call.
  run_ok(
    "let count = 0; \
     await expect.poll(() => { count += 1; return count; }, { timeout: 2000 }).toEqual(3); \
     return 'ok'",
  )
  .await;
}

#[tokio::test]
async fn poll_to_satisfy_with_predicate() {
  run_ok(
    "let count = 0; \
     await expect.poll(() => { count += 1; return count; }, { timeout: 2000 }).toSatisfy(v => v >= 3); \
     return 'ok'",
  )
  .await;
}

#[tokio::test]
async fn poll_timeout_throws_with_last_value() {
  let err = run_err(
    "await expect.poll(() => 'never matches', { timeout: 300 }).toEqual('something'); \
     return 'unreached'",
  )
  .await;
  assert!(
    err.contains("toEqual") && err.contains("timed out"),
    "expected timeout error message, got: {err}"
  );
}

#[tokio::test]
async fn close_to_asymmetric() {
  run_ok("expect({pi: 3.14159}).toEqual({pi: expect.closeTo(3.14, 2)}); return 'ok'").await;
}

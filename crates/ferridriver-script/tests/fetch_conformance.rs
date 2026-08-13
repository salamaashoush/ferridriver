#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Point-by-point WHATWG conformance of the web-platform globals, as one
//! table: source snippet -> expected result.
//!
//! Each row is a spec behaviour that is cheap to state and easy to
//! regress — `Symbol.toStringTag`, the GET/HEAD-with-body rule, the
//! `Response` constructor's status rules, `URL.searchParams` liveness.
//! The table reports EVERY mismatch in one run rather than stopping at
//! the first, so a sweep like "add toStringTag to the platform objects"
//! can be checked in a single pass.

use std::sync::Arc;

use ferridriver_script::{Outcome, PathSandbox, RunContext, RunOptions, ScriptEngine, ScriptEngineConfig};

async fn run(src: &str) -> Outcome {
  let tmp = tempfile::tempdir().expect("tempdir");
  let ctx = RunContext {
    vars: Arc::new(ferridriver_script::InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(tmp.path()).expect("sandbox")),
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
  ScriptEngine::new(ScriptEngineConfig::default())
    .run(src, &[], RunOptions::default(), ctx)
    .await
    .outcome
}

/// `(what the spec says, source, expected result)`. Kept at module
/// scope so the test body stays a loop over the table.
const CHECKS: &[(&str, &str, &str)] = &[
  (
    "GET with body throws",
    "try { new Request('http://x/', { body: 'a' }); return 'NO THROW'; } catch (e) { return 'threw'; }",
    "threw",
  ),
  (
    "HEAD with body throws",
    "try { new Request('http://x/', { method: 'HEAD', body: 'a' }); return 'NO THROW'; } catch (e) { return 'threw'; }",
    "threw",
  ),
  (
    "Headers toStringTag",
    "return Object.prototype.toString.call(new Headers());",
    "[object Headers]",
  ),
  (
    "Request toStringTag",
    "return Object.prototype.toString.call(new Request('http://x/'));",
    "[object Request]",
  ),
  (
    "Response toStringTag",
    "return Object.prototype.toString.call(new Response());",
    "[object Response]",
  ),
  (
    "FormData toStringTag",
    "return Object.prototype.toString.call(new FormData());",
    "[object FormData]",
  ),
  (
    "Blob toStringTag",
    "return Object.prototype.toString.call(new Blob());",
    "[object Blob]",
  ),
  (
    "URLSearchParams toStringTag",
    "return Object.prototype.toString.call(new URLSearchParams());",
    "[object URLSearchParams]",
  ),
  (
    "Request.isHistoryNavigation",
    "return String(new Request('http://x/').isHistoryNavigation);",
    "false",
  ),
  (
    "Request.isReloadNavigation",
    "return String(new Request('http://x/').isReloadNavigation);",
    "false",
  ),
  (
    "Request.duplex roundtrip",
    "return String(new Request('http://x/', { method:'POST', body:'a', duplex:'half' }).duplex);",
    "half",
  ),
  (
    "Response.statusText default",
    "return JSON.stringify(new Response().statusText);",
    "\"\"",
  ),
  ("Response.type default", "return new Response().type;", "default"),
  (
    "Response.url default",
    "return JSON.stringify(new Response().url);",
    "\"\"",
  ),
  ("Response.error type", "return Response.error().type;", "error"),
  (
    "Response.error status 0",
    "return String(Response.error().status);",
    "0",
  ),
  (
    "Response.redirect location",
    "return Response.redirect('http://y/', 301).headers.get('location');",
    "http://y/",
  ),
  (
    "Response.json ct",
    "return Response.json({a:1}).headers.get('content-type');",
    "application/json",
  ),
  (
    "Headers ctor rejects number",
    "try { new Headers(1); return 'NO THROW'; } catch(e) { return 'threw'; }",
    "threw",
  ),
  (
    "Headers invalid name throws",
    "try { new Headers({'a b':'1'}); return 'NO THROW'; } catch(e) { return 'threw'; }",
    "threw",
  ),
  (
    "Headers value normalize",
    "const h=new Headers(); h.set('x','  a  '); return h.get('x');",
    "a",
  ),
  (
    "Blob endings native",
    "return String(new Blob(['a\\r\\nb'], {endings:'native'}).size);",
    "4",
  ),
  (
    "Blob type lowercased",
    "return new Blob([], {type:'TEXT/PLAIN'}).type;",
    "text/plain",
  ),
  (
    "File instanceof Blob",
    "return String(new File(['a'],'n.txt') instanceof Blob);",
    "true",
  ),
  (
    "Request clone keeps signal",
    "const c=new AbortController(); const r=new Request('http://x/',{signal:c.signal}); return String(r.clone().signal===r.signal);",
    "true",
  ),
  (
    "Body reread throws",
    "const r=new Response('a'); await r.text(); try { await r.text(); return 'NO THROW'; } catch(e){ return 'threw'; }",
    "threw",
  ),
  (
    "bodyUsed after read",
    "const r=new Response('a'); await r.text(); return String(r.bodyUsed);",
    "true",
  ),
  (
    "Response null body 204",
    "try { new Response('a',{status:204}); return 'NO THROW'; } catch(e){ return 'threw'; }",
    "threw",
  ),
  (
    "Response status range",
    "try { new Response('a',{status:99}); return 'NO THROW'; } catch(e){ return 'threw'; }",
    "threw",
  ),
  (
    "formData urlencoded",
    "const r=new Response('a=1',{headers:{'content-type':'application/x-www-form-urlencoded'}}); const f=await r.formData(); return f.get('a');",
    "1",
  ),
  (
    "blob() types from ct",
    "const r=new Response('x',{headers:{'content-type':'text/csv'}}); return (await r.blob()).type;",
    "text/csv",
  ),
  (
    "bytes() is Uint8Array",
    "const b=await new Response('ab').bytes(); return b.constructor.name;",
    "Uint8Array",
  ),
  (
    "URLSearchParams in URL live",
    "const u=new URL('http://x/?a=1'); u.searchParams.set('a','2'); return u.href;",
    "http://x/?a=2",
  ),
  (
    "Headers append no cookie semicolon",
    "const h=new Headers(); h.append('cookie','a=1'); h.append('cookie','b=2'); return h.get('cookie');",
    "a=1, b=2",
  ),
];

#[tokio::test(flavor = "multi_thread")]
async fn web_platform_objects_match_the_spec() {
  let mut fails = Vec::new();
  for (name, src, want) in CHECKS {
    let o = run(src).await;
    let got = match &o {
      Outcome::Ok { success } => success
        .value
        .as_str()
        .map_or_else(|| success.value.to_string(), ToString::to_string),
      Outcome::Error { error } => format!("ERROR: {error:?}"),
    };
    if got.trim() != *want {
      fails.push(format!("  {name}\n     want: {want}\n     got:  {got}"));
    }
  }
  assert!(
    fails.is_empty(),
    "{} conformance mismatch(es):\n{}",
    fails.len(),
    fails.join("\n")
  );
}

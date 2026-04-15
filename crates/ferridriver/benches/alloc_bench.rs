//! Allocation benchmarks: old patterns vs new optimized patterns.
//!
//! Each benchmark group has a "before" (simulating the old code path) and
//! "after" (the new optimized code path) so you can see the improvement.
//!
//! Run: cargo bench -p ferridriver --bench `alloc_bench`

use std::sync::Arc;

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};

// ============================================================================
// 1. build_parts_json: Vec<String> collect+join (before) vs single buffer (after)
// ============================================================================

fn build_parts_json_before(parts: &[(&str, &str)]) -> String {
  let json_parts: Vec<String> = parts
    .iter()
    .map(|(engine, body)| {
      let body_escaped = serde_json::to_string(body).unwrap_or_else(|_| format!("\"{body}\""));
      format!(r#"{{"engine":"{engine}","body":{body_escaped}}}"#)
    })
    .collect();
  format!("[{}]", json_parts.join(","))
}

fn build_parts_json_after(parts: &[(&str, &str)]) -> String {
  let mut buf = String::with_capacity(parts.len() * 40 + 2);
  buf.push('[');
  for (i, (engine, body)) in parts.iter().enumerate() {
    if i > 0 {
      buf.push(',');
    }
    buf.push_str(r#"{"engine":""#);
    buf.push_str(engine);
    buf.push_str(r#"","body":""#);
    for ch in body.chars() {
      match ch {
        '"' => buf.push_str(r#"\""#),
        '\\' => buf.push_str(r"\\"),
        c => buf.push(c),
      }
    }
    buf.push_str("\"}");
  }
  buf.push(']');
  buf
}

fn bench_build_parts_json(c: &mut Criterion) {
  let mut group = c.benchmark_group("build_parts_json");

  let parts_2: Vec<(&str, &str)> = vec![("css", ".container"), ("role", "button")];
  let parts_4: Vec<(&str, &str)> = vec![
    ("css", ".form"),
    ("role", "textbox"),
    ("text", "Email Address"),
    ("nth", "0"),
  ];
  let parts_8: Vec<(&str, &str)> = vec![
    ("css", ".app"),
    ("role", "navigation"),
    ("css", ".sidebar"),
    ("role", "list"),
    ("role", "listitem"),
    ("text", "Settings"),
    ("css", ".panel"),
    ("nth", "2"),
  ];

  for (name, parts) in [("2_parts", &parts_2), ("4_parts", &parts_4), ("8_parts", &parts_8)] {
    group.bench_function(format!("before/{name}"), |b| {
      b.iter(|| black_box(build_parts_json_before(parts)));
    });
    group.bench_function(format!("after/{name}"), |b| {
      b.iter(|| black_box(build_parts_json_after(parts)));
    });
  }
  group.finish();
}

// ============================================================================
// 2. CdpPage-like clone: String fields (before) vs Arc<str> fields (after)
// ============================================================================

#[derive(Clone)]
struct PageHandleBefore {
  transport: Arc<()>,
  session_id: Option<String>,
  target_id: String,
  browser_context_id: Option<String>,
  events: Arc<()>,
  frame_contexts: Arc<()>,
  dialog_handler: Arc<()>,
  exposed_fns: Arc<()>,
  routes: Arc<()>,
}

#[derive(Clone)]
struct PageHandleAfter {
  transport: Arc<()>,
  session_id: Option<Arc<str>>,
  target_id: Arc<str>,
  browser_context_id: Option<Arc<str>>,
  events: Arc<()>,
  frame_contexts: Arc<()>,
  dialog_handler: Arc<()>,
  exposed_fns: Arc<()>,
  routes: Arc<()>,
}

fn bench_page_handle_clone(c: &mut Criterion) {
  let mut group = c.benchmark_group("page_handle_clone");

  let before = PageHandleBefore {
    transport: Arc::new(()),
    session_id: Some("F4E2A7C8B3D1E6F0A9B5C4D7E8F1A2B3".into()),
    target_id: "7B3A9C2D4E1F6A8B5C0D3E7F2A4B6C8D".into(),
    browser_context_id: Some("1A2B3C4D5E6F7A8B9C0D1E2F3A4B5C6D".into()),
    events: Arc::new(()),
    frame_contexts: Arc::new(()),
    dialog_handler: Arc::new(()),
    exposed_fns: Arc::new(()),
    routes: Arc::new(()),
  };

  let after = PageHandleAfter {
    transport: Arc::new(()),
    session_id: Some(Arc::from("F4E2A7C8B3D1E6F0A9B5C4D7E8F1A2B3")),
    target_id: Arc::from("7B3A9C2D4E1F6A8B5C0D3E7F2A4B6C8D"),
    browser_context_id: Some(Arc::from("1A2B3C4D5E6F7A8B9C0D1E2F3A4B5C6D")),
    events: Arc::new(()),
    frame_contexts: Arc::new(()),
    dialog_handler: Arc::new(()),
    exposed_fns: Arc::new(()),
    routes: Arc::new(()),
  };

  // Single clone
  group.bench_function("before/1_clone", |b| {
    b.iter(|| {
      let c = before.clone();
      black_box(&c.transport);
      black_box(&c.session_id);
      black_box(&c.target_id);
      black_box(&c.browser_context_id);
      black_box(&c.events);
      black_box(&c.frame_contexts);
      black_box(&c.dialog_handler);
      black_box(&c.exposed_fns);
      black_box(&c.routes);
    });
  });
  group.bench_function("after/1_clone", |b| {
    b.iter(|| {
      let c = after.clone();
      black_box(&c.transport);
      black_box(&c.session_id);
      black_box(&c.target_id);
      black_box(&c.browser_context_id);
      black_box(&c.events);
      black_box(&c.frame_contexts);
      black_box(&c.dialog_handler);
      black_box(&c.exposed_fns);
      black_box(&c.routes);
    });
  });

  // 7 clones (retry loop depth)
  group.bench_function("before/7_retries", |b| {
    b.iter(|| {
      for _ in 0..7 {
        let c = before.clone();
        black_box(&c.target_id);
      }
    });
  });
  group.bench_function("after/7_retries", |b| {
    b.iter(|| {
      for _ in 0..7 {
        let c = after.clone();
        black_box(&c.target_id);
      }
    });
  });
  group.finish();
}

// ============================================================================
// 3. filter(): clone + 4x chain (before) vs build suffix + 1x chain (after)
// ============================================================================

fn filter_before(selector: &str) -> String {
  // Old: clone + chain 4 times, each copying the growing string
  let mut sel = selector.to_string();
  sel = format!("{sel} >> has-text=Submit");
  sel = format!("{sel} >> has-not-text=Cancel");
  sel = format!("{sel} >> has=.icon");
  sel = format!("{sel} >> has-not=.disabled");
  sel
}

fn filter_after(selector: &str) -> String {
  // New: build suffix once, concat once
  let suffix = "has-text=Submit >> has-not-text=Cancel >> has=.icon >> has-not=.disabled";
  format!("{selector} >> {suffix}")
}

fn bench_filter(c: &mut Criterion) {
  let mut group = c.benchmark_group("filter_pattern");

  let base = "css=.form >> role=button >> text=Save";

  group.bench_function("before", |b| {
    b.iter(|| black_box(filter_before(base)));
  });
  group.bench_function("after", |b| {
    b.iter(|| black_box(filter_after(base)));
  });
  group.finish();
}

// ============================================================================
// 4. Locator clone: String frame_id (before) vs Arc<str> frame_id (after)
// ============================================================================

#[derive(Clone)]
struct LocatorBefore {
  page: Arc<()>,
  selector: String,
  frame_id: Option<String>,
}

#[derive(Clone)]
struct LocatorAfter {
  page: Arc<()>,
  selector: String,
  frame_id: Option<Arc<str>>,
}

fn bench_locator_clone(c: &mut Criterion) {
  let mut group = c.benchmark_group("locator_clone");

  let before = LocatorBefore {
    page: Arc::new(()),
    selector: "css=.container >> role=button[name=\"Submit\"] >> nth=0".into(),
    frame_id: Some("frame-123-abc-456-def".into()),
  };

  let after = LocatorAfter {
    page: Arc::new(()),
    selector: "css=.container >> role=button[name=\"Submit\"] >> nth=0".into(),
    frame_id: Some(Arc::from("frame-123-abc-456-def")),
  };

  group.bench_function("before", |b| {
    b.iter(|| {
      let c = before.clone();
      black_box(&c.page);
      black_box(&c.selector);
      black_box(&c.frame_id);
    });
  });
  group.bench_function("after", |b| {
    b.iter(|| {
      let c = after.clone();
      black_box(&c.page);
      black_box(&c.selector);
      black_box(&c.frame_id);
    });
  });
  group.finish();
}

// ============================================================================
// 5. ContextRef clone: String fields (before) vs Arc<str> fields (after)
// ============================================================================

#[derive(Clone)]
struct ContextRefBefore {
  state: Arc<()>,
  name: String,
  instance: String,
  context: String,
}

#[derive(Clone)]
struct ContextRefAfter {
  state: Arc<()>,
  name: Arc<str>,
  instance: Arc<str>,
  context: Arc<str>,
}

fn bench_context_ref_clone(c: &mut Criterion) {
  let mut group = c.benchmark_group("context_ref_clone");

  let before = ContextRefBefore {
    state: Arc::new(()),
    name: "staging:admin-context".into(),
    instance: "staging".into(),
    context: "admin-context".into(),
  };

  let after = ContextRefAfter {
    state: Arc::new(()),
    name: Arc::from("staging:admin-context"),
    instance: Arc::from("staging"),
    context: Arc::from("admin-context"),
  };

  group.bench_function("before", |b| {
    b.iter(|| {
      let c = before.clone();
      black_box(&c.state);
      black_box(&c.name);
      black_box(&c.instance);
      black_box(&c.context);
    });
  });
  group.bench_function("after", |b| {
    b.iter(|| {
      let c = after.clone();
      black_box(&c.state);
      black_box(&c.name);
      black_box(&c.instance);
      black_box(&c.context);
    });
  });
  group.finish();
}

// ============================================================================

criterion_group!(
  benches,
  bench_build_parts_json,
  bench_page_handle_clone,
  bench_filter,
  bench_locator_clone,
  bench_context_ref_clone,
);
criterion_main!(benches);

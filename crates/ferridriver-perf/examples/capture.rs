//! Capture a real trace to a file, so the analysis can be iterated on
//! without relaunching a browser each time.
//!
//! `cargo run -p ferridriver-perf --example capture -- <url> <out.json> [mode] [+category,...]`
//!
//! `mode` is `prenav` or `interact`. A `+` argument adds trace
//! categories on top of the default set, which is how the optional ones
//! get recorded: CSS selector statistics need
//! `+disabled-by-default-blink.debug,disabled-by-default-devtools.timeline.invalidationTracking`,
//! and neither Lighthouse nor the `DevTools` panel records them unless
//! asked.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let mut args = std::env::args().skip(1);
  let url = args.next().unwrap_or_else(|| "https://example.com".into());
  let out = args.next().unwrap_or_else(|| "trace.json".into());

  let browser = ferridriver::browser_type::chromium()
    .launch(ferridriver::options::LaunchOptions {
      headless: Some(true),
      ..Default::default()
    })
    .await?;
  let context = browser.new_context().await?;
  let page = context.new_page().await?;

  // Optional third arg mimics the MCP order: land on the page first,
  // then trace a re-navigation.
  if std::env::args().nth(3).as_deref() == Some("prenav") {
    page.goto(&url).await?;
    page.wait_for_load_state(None).await?;
  }
  // Extra categories, if any were named. The default set is what
  // Lighthouse records; anything beyond it is opt-in on both sides,
  // which is why an insight that reads one can report "not measured"
  // on an ordinary trace.
  let extra: Vec<String> = std::env::args()
    .find_map(|arg| arg.strip_prefix('+').map(str::to_string))
    .map(|list| list.split(',').map(str::to_string).collect())
    .unwrap_or_default();
  let categories: Option<Vec<String>> = (!extra.is_empty()).then(|| {
    let mut all: Vec<String> = ferridriver::trace_categories::DEFAULT
      .iter()
      .map(|c| (*c).to_string())
      .collect();
    all.extend(extra);
    all
  });
  page.start_tracing(categories.as_deref()).await?;
  page.goto(&url).await?;
  page.wait_for_load_state(None).await?;
  // `load` fires before the compositor has necessarily committed a
  // frame, and the paint markers (FCP, LCP candidates) are emitted from
  // that commit. Ending the trace at `load` captures a trace with no
  // paint metrics in it at all.
  page
    .evaluate(
      "Promise.all([...document.images].map(i => i.complete ? 0 : new Promise(r => { i.onload = r; i.onerror = r; })))\
         .then(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r(1)))))",
      ferridriver::protocol::serializers::SerializedArgument::default(),
      None,
    )
    .await?;
  // Optional `interact` mode drives a click whose handler thrashes
  // layout, so the INP and forced-reflow insights have something real to
  // report.
  if std::env::args().nth(3).as_deref() == Some("interact") {
    page
      .evaluate(
        "(() => { const b = document.createElement('button'); b.id='thrash'; b.textContent='go';\
           b.onclick = () => { const d = document.createElement('div');\
             for (let i = 0; i < 4000; i++) { d.style.width = i + 'px'; void d.offsetHeight;\
               document.body.appendChild(d); void document.body.offsetHeight; } };\
           document.body.appendChild(b); return 1; })()",
        ferridriver::protocol::serializers::SerializedArgument::default(),
        None,
      )
      .await?;
    page.click("#thrash").await?;
    page
      .evaluate(
        "new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r(1))))",
        ferridriver::protocol::serializers::SerializedArgument::default(),
        None,
      )
      .await?;
  }

  let events = page.stop_tracing().await?;

  std::fs::write(&out, serde_json::to_vec(&events)?)?;
  println!("wrote {} events to {out}", events.len());

  let report = ferridriver_perf::analyze_values(&events);
  println!("{}", serde_json::to_string_pretty(&report.metrics)?);
  for insight in &report.insights {
    println!("[{:?}] {}", insight.severity, insight.title);
    for check in &insight.checks {
      println!("   {} {}", if check.passed { "ok" } else { "!!" }, check.detail);
    }
  }
  browser.close().await?;
  Ok(())
}

//! Capture a real trace to a file, so the analysis can be iterated on
//! without relaunching a browser each time.
//!
//! `cargo run -p ferridriver-perf --example capture -- <url> <out.json>`

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
  page.start_tracing(None).await?;
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

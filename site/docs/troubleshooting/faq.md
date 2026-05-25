# FAQ

## Is ferridriver production-ready?

It is **pre-1.0**. The browser API mirrors Playwright closely and the
test suite is comprehensive across all four backends, but breaking
changes between minor versions are expected until 1.0. Read the
[CHANGELOG](/changelog) before upgrading.

## Why "ferridriver"?

*Ferris* (the Rust mascot) + *driver* (browser driver). One word, no
hyphen.

## Why not just use Playwright via NAPI / FFI?

Two reasons:

1. Playwright's core is TypeScript running in Node. Calling it from
   Rust still requires Node and an IPC hop. ferridriver runs the
   protocol in Rust directly — no second runtime.
2. The hot paths (auto-wait polling, selector compilation, retry
   loops) stay in Rust. From JavaScript via `@ferridriver/node`, you
   pay one NAPI call per assertion, not one per poll iteration.

## How does ferridriver compare to thirtyfour / fantoccini?

`thirtyfour` and `fantoccini` are W3C WebDriver clients in Rust.
ferridriver speaks CDP, BiDi, and Playwright's WebKit Inspector
protocol directly — no WebDriver server in between. Different layer of
the stack, more features (auto-wait, network mocking, traces, video,
strict locators), more browsers (Playwright WebKit), more shapes
(test runner, BDD, MCP, NAPI binding).

If you're integrating with an existing Selenium Grid, use
`thirtyfour`. If you're building a new browser-test setup, use
ferridriver.

## Can I use ferridriver from Python / Java / Go?

Not directly. There is no Python or Java binding today. The Rust
library is documented at [docs.rs](https://docs.rs/ferridriver) — a
binding shouldn't be hard to write, but it's not on our roadmap.

For Node and Bun: `@ferridriver/node` exists.

## How do I run the test suite for multiple browsers in parallel?

Define one project per browser (see
[Configuration → Projects](/test-runner/config#projects-matrix-runs))
and shard each project in CI. `--project chromium --shard 1/4` on one
job, `--project firefox --shard 1/4` on another, etc.

## Does ferridriver work in Docker?

Yes. The official base images are not yet published, but anything that
runs Chromium headless works. Make sure to install browser system
dependencies in the image:

```dockerfile
FROM rust:1.91-slim
RUN apt-get update && apt-get install -y \
    libnss3 libnspr4 libatk1.0-0 libatk-bridge2.0-0 libcups2 \
    libdrm2 libxkbcommon0 libxcomposite1 libxdamage1 libxfixes3 \
    libxrandr2 libgbm1 libpango-1.0-0 libcairo2 libasound2 \
    && rm -rf /var/lib/apt/lists/*

RUN cargo install ferridriver-cli --locked
RUN ferridriver install chromium
```

(`ferridriver install --with-deps chromium` does the same.)

## Can I drive an existing Chrome window?

Yes, with the `cdp-raw` backend:

```bash
# Start Chrome with remote debugging
google-chrome --remote-debugging-port=9222

# Connect
ferridriver mcp --connect ws://localhost:9222/devtools/browser/...
```

Or programmatically:

```rust
let browser = Browser::connect("ws://localhost:9222/devtools/browser/...").await?;
```

## How do I capture network traffic for a test?

Set the HAR record option on the context:

```toml
[test.browser.useOptions.recordHar]
path    = "test-results/network.har"
content = "embed"
```

The HAR file is written when the context closes. Inspect it with any
HAR viewer (Chrome DevTools "Import HAR…", `har` tool, etc.).

Note: the BiDi backend does not support HAR.

## Does the MCP server work with Claude Code?

Yes:

```bash
claude mcp add ferridriver -- ferridriver mcp
```

Then ask Claude to navigate, take a screenshot, or run a script. See
[MCP setup](/mcp/setup) for Claude Desktop, Cursor, and HTTP transport.

## Can I write BDD steps in Rust and JavaScript in the same suite?

Yes. Rust step macros and the JS / TS step bundle register into one
registry. A single `.feature` file can mix steps defined in either —
the runner picks the matching step regardless of source language.

## Is the API stable across minor versions?

Not yet. `0.x → 0.x+1` may include breaking changes. The CHANGELOG
calls them out. We will commit to semver compatibility starting at
`1.0.0`.

## Where do I report bugs?

<https://github.com/salamaashoush/ferridriver/issues>

Include:

- `ferridriver --version`
- `rustc --version`
- OS and architecture
- Output of `ferridriver -vv ...` reproducing the failure
- A minimal repro (a single `.feature` file or a 10-line Rust test)

## How do I contribute?

See [CONTRIBUTING](/contributing). PRs welcome — start with a draft
that explains the problem before you write much code.

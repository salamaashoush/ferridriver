# ferridriver-cli

MCP (Model Context Protocol) server for AI-powered browser automation. Exposes 28 tools for navigating, interacting with, and extracting content from web pages, backed by the ferridriver browser engine.

Ships as the `ferridriver` binary. Supports stdio (default) and HTTP transports, and four browser backends.

## Install

```bash
# From source
cargo install ferridriver-cli

# From GitHub releases
curl -fsSL https://github.com/salamaashoush/ferridriver/releases/latest/download/ferridriver-VERSION-TARGET.tar.gz | tar xz
```

## Usage

Register with your MCP client (Claude Desktop, Cursor, Claude Code, etc.):

```json
{
  "mcpServers": {
    "browser": {
      "command": "ferridriver",
      "args": []
    }
  }
}
```

## CLI flags

```
-v, --verbose...           increase log level (-v = info+debug, -vv = trace)
-c, --config <PATH>        YAML / TOML / JSON config file

    --backend <B>          cdp-pipe (default) | cdp-raw | webkit | bidi
    --headless             run browser headless (default: true)
    --executable-path      path to a Chrome / Chromium binary
    --connect <URL>        WebSocket URL of a running browser
    --auto-connect <CH>    discover a running browser by channel name
    --user-data-dir <DIR>  persistent Chrome profile directory

    --transport <T>        stdio (default) | http
    --port <N>             HTTP port (default: 8080)
```

## Tools (28)

### Navigation
- **connect** — attach to a running Chrome (debugger URL or `auto_discover`)
- **navigate** — go to URL
- **page** — manage pages / sessions (`back`, `forward`, `reload`, `new`, `close`, `select`, `list`, `close_browser`)

### Interaction (12)
**click**, **click_at**, **hover**, **fill**, **fill_form**, **type_text**, **press_key**, **drag**, **scroll**, **select_option**, **upload_file**

### Content (8)
- **snapshot** — accessibility tree snapshot with depth limiting and incremental tracking
- **screenshot** — visual capture (PNG / JPEG / WebP)
- **evaluate** — run JavaScript
- **wait_for** — wait for selector or text
- **search_page** — grep-like text search with context
- **find_elements** — list elements matching a CSS or rich selector
- **get_markdown** — extract page as clean markdown

### State (4)
- **cookies** — get / set / delete / clear cookies
- **storage** — get / set / list / clear localStorage
- **emulate** — viewport, user agent, geolocation, network conditions
- **diagnostics** — console messages, network requests, performance tracing

### BDD (3)
- **list_steps**, **run_step**, **run_scenario**

## Sessions

All tools accept an optional `session` parameter (default: `"default"`). Different sessions have isolated cookies, localStorage, and network state. Use for multi-user testing or parallel automation.

```
session: "admin"        isolated context named "admin"
session: "staging:qa"   context "qa" on Chrome instance "staging"
```

## Accessibility snapshots

The `snapshot` tool returns an LLM-optimized accessibility tree with `[ref=eN]` identifiers. Use these refs with click / hover / fill tools for precise element targeting.

```
### Page
- URL: https://example.com
- Title: Example

### Snapshot
- heading "Example Domain" [ref=e1] [level=1]
- paragraph "This domain is for..." [ref=e2]
- link "More information..." [ref=e3] [url=https://www.iana.org/...]
```

## Running

```bash
# stdio transport (default) for Claude Desktop / Cursor / Claude Code
ferridriver

# HTTP transport for remote clients
ferridriver --transport http --port 8080

# WebKit backend (macOS, no Chrome needed)
ferridriver --backend webkit

# Firefox over BiDi
ferridriver --backend bidi

# Attach to a running Chrome
ferridriver --auto-connect my-channel
ferridriver --connect ws://localhost:9222/devtools/browser/...
```

## Building

```bash
cargo build --release -p ferridriver-cli
```

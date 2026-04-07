# ferridriver-cli

MCP (Model Context Protocol) server for AI-powered browser automation. Provides browser tools for navigating, interacting with, and extracting content from web pages.

## Installation

```bash
cargo install ferridriver-cli
```

## Usage

Add to your MCP client configuration (Claude Desktop, Cursor, etc.):

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

## Tools

### Navigation
- **connect** -- attach to a running Chrome (debugger URL or `auto_discover`)
- **navigate** -- go to URL
- **page** -- manage pages/sessions (back, forward, reload, new, close, select, list, close_browser)

### Interaction
- **click** / **click_at** / **hover** / **fill** / **fill_form** / **type_text** / **press_key** / **drag** / **scroll** / **select_option** / **upload_file**

### Content
- **snapshot** -- accessibility tree snapshot with depth limiting and incremental tracking
- **screenshot** -- visual capture (PNG/JPEG/WebP)
- **evaluate** -- run JavaScript
- **wait_for** -- wait for selector or text
- **search_page** -- grep-like text search with context
- **find_elements** -- list elements matching a CSS or rich selector
- **get_markdown** -- extract page as clean markdown

### State
- **cookies** -- get/set/delete/clear cookies
- **storage** -- get/set/list/clear localStorage
- **emulate** -- viewport, user agent, geolocation, network conditions
- **diagnostics** -- console messages, network requests, performance tracing

## Sessions

All tools accept an optional `session` parameter (default: `"default"`). Different sessions have isolated cookies, localStorage, and network state. Use for multi-user testing or parallel automation.

```
session: "admin"      -- isolated context named "admin"
session: "staging:qa" -- context "qa" on Chrome instance "staging"
```

## Accessibility Snapshots

The `snapshot` tool returns an LLM-optimized accessibility tree with `[ref=eN]` identifiers. Use these refs with click/hover/fill tools for precise element targeting.

```
### Page
- URL: https://example.com
- Title: Example

### Snapshot
- heading "Example Domain" [ref=e1] [level=1]
- paragraph "This domain is for..." [ref=e2]
- link "More information..." [ref=e3] [url=https://www.iana.org/...]
```

## Building

```bash
cargo build --release --package ferridriver-cli
```

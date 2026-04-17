# MCP server

A scripting-focused MCP server for browser automation. Nine tools: `navigate`, `connect`, `page`, `snapshot`, `screenshot`, `evaluate`, `search_page`, `diagnostics`, and `run_script`. The `ferridriver` binary is an MCP server by default — stdio transport out of the box, HTTP transport with a single flag.

Works with any MCP client: Claude Desktop, Cursor, Claude Code, and others.

## Design

LLMs drive this server like they drive a JavaScript runtime:

- **Observe** via `snapshot` (accessibility tree) or `screenshot` (visual fallback).
- **Act** via `run_script` — a single tool call that runs sandboxed JavaScript against the live session. One script can navigate, fill forms, click, assert, and make HTTP calls in one atomic LLM turn. Multi-step flows take a single LLM round-trip.
- **Verify** with another `snapshot` or `evaluate`.

Browser interaction flows through `run_script` bindings (`page`, `context`, `request`) — Playwright-shaped API over the ferridriver core. See [Tools](/mcp/tools) for the full script surface.

## Running

```bash
# stdio (what most desktop clients want)
ferridriver

# HTTP on port 8080
ferridriver --transport http --port 8080

# Different backends
ferridriver --backend webkit      # macOS WKWebView
ferridriver --backend bidi        # Firefox via WebDriver BiDi

# Attach to a running Chrome
ferridriver --auto-connect my-channel
ferridriver --connect ws://localhost:9222/devtools/browser/...
```

## Next

- [Tools](/mcp/tools) — the 9-tool surface + `run_script` script API
- [Setup](/mcp/setup) — client configuration snippets (Claude Desktop, Cursor, Claude Code)

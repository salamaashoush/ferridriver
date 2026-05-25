# MCP server

Scripting-focused MCP server for browser automation. **Ten tools.**
`navigate`, `connect`, `page` (session bootstrap) · `snapshot`,
`screenshot`, `evaluate`, `search_page`, `diagnostics` (observation) ·
`run_script` (action) · `ferridriver_extensions` (introspection).

Works with any MCP client: Claude Code, Claude Desktop, Cursor, …

## Design

LLMs drive this server like they drive a JavaScript runtime:

- **Observe** via `snapshot` (accessibility tree) or `screenshot`
  (visual fallback).
- **Act** via `run_script` — a single tool call that runs sandboxed
  JavaScript against the live session. One script can navigate, fill
  forms, click, assert, and make HTTP calls in one atomic LLM turn.
  Multi-step flows take a single round-trip.
- **Verify** with another `snapshot` or `evaluate`.

Browser interaction flows through `run_script` bindings (`page`,
`context`, `request`, `browser`) — Playwright-shaped APIs over the
ferridriver core. See [Tools](/mcp/tools) for the full script surface.

## Running

```bash
# stdio (Claude Code, Cursor, Claude Desktop)
ferridriver mcp

# HTTP on port 8080
ferridriver mcp --transport http --port 8080

# Backend choice
ferridriver mcp --backend webkit          # Playwright WebKit
ferridriver mcp --backend bidi            # Firefox via WebDriver BiDi
ferridriver mcp --backend cdp-pipe --headless

# Attach to a running Chrome
ferridriver mcp --auto-connect chrome
ferridriver mcp --connect ws://localhost:9222/devtools/browser/...
```

## Sessions

All tools accept an optional `session` parameter
(default: `"default"`). Format: `instance:context` — `instance` selects
the browser process, `context` selects the BrowserContext within it.
Sessions have isolated cookies, localStorage, and network state.

```
session: "admin"          isolated context named "admin"
session: "staging:qa"     context "qa" on Chrome instance "staging"
```

## Extensions

JS / TS extension files can register additional MCP tools via
`defineTool` and / or BDD steps via `Given` / `When` / `Then`. They run
on the same QuickJS engine as `run_script` and BDD step bodies. See
[`docs/extensions.md`](https://github.com/salamaashoush/ferridriver/blob/main/docs/extensions.md)
for the authoring contract (manifest, capabilities, `allow.commands`,
`allow.net`).

## Next

- [Tools](/mcp/tools) — the 10-tool surface plus the `run_script` script API
- [Setup](/mcp/setup) — client configuration snippets (Claude Code, Cursor, Claude Desktop)

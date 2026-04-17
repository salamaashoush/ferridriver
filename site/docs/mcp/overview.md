# MCP server

28 browser-automation tools exposed over the Model Context Protocol. The `ferridriver` binary is an MCP server by default — stdio transport out of the box, HTTP transport with a single flag.

Works with any MCP client: Claude Desktop, Cursor, Claude Code, and others.

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

- [Tools](/mcp/tools) — all 28 tools grouped by category
- [Setup](/mcp/setup) — client configuration snippets (Claude Desktop, Cursor, Claude Code)

# Client setup

## Claude Desktop / Cursor

Add to `claude_desktop_config.json` (macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`):

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

With custom flags:

```json
{
  "mcpServers": {
    "browser": {
      "command": "ferridriver",
      "args": ["--backend", "webkit"]
    }
  }
}
```

## Claude Code

```bash
claude mcp add browser ferridriver
# or with args
claude mcp add browser -- ferridriver --backend webkit
```

## HTTP transport (remote client)

Start the server:

```bash
ferridriver --transport http --port 8080
```

Point the client at `http://localhost:8080/mcp`.

## Install the binary

```bash
# From crates.io
cargo install ferridriver-cli

# Or prebuilt release
curl -fsSL https://github.com/salamaashoush/ferridriver/releases/latest/download/ferridriver-VERSION-TARGET.tar.gz | tar xz
```

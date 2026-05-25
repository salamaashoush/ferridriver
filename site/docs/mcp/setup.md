# Client setup

## Claude Code

```bash
claude mcp add ferridriver -- ferridriver mcp
# or with args
claude mcp add ferridriver -- ferridriver mcp --backend webkit --headless
```

## Claude Desktop, Cursor

Add to `claude_desktop_config.json` (macOS path:
`~/Library/Application Support/Claude/claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "ferridriver": {
      "command": "ferridriver",
      "args": ["mcp"]
    }
  }
}
```

With custom flags:

```json
{
  "mcpServers": {
    "ferridriver": {
      "command": "ferridriver",
      "args": ["mcp", "--backend", "webkit", "--headless"]
    }
  }
}
```

## HTTP transport (remote client)

Start the server:

```bash
ferridriver mcp --transport http --port 8080
```

Point the client at `http://localhost:8080/mcp`.

There is no built-in authentication. Deploy behind a firewall or reverse
proxy.

## Install the binary

```bash
# From crates.io
cargo install ferridriver-cli

# From GitHub releases
curl -fsSL https://github.com/salamaashoush/ferridriver/releases/latest/download/ferridriver-VERSION-TARGET.tar.gz | tar xz

# Or the bundled install script (Linux / macOS)
curl -fsSL https://raw.githubusercontent.com/salamaashoush/ferridriver/main/install.sh | bash
```

## Browsers

```bash
ferridriver install chromium                          # default
ferridriver install --with-deps chromium              # Linux: + system libs
```

WebKit backend: provide the Playwright WebKit binary via
`FERRIDRIVER_WEBKIT` or `npx playwright install webkit`.

Firefox backend: install Firefox locally (ferridriver does not bundle
it).

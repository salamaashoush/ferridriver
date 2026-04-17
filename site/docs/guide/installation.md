# Installation

## Rust library

```toml
# Cargo.toml
[dependencies]
ferridriver = "0.1"
```

## Node.js / Bun

```bash
npm install @ferridriver/node
# or
bun add @ferridriver/node
```

Platform binaries are shipped via `optionalDependencies`:

| Platform | Package |
|---|---|
| macOS arm64 | `@ferridriver/node-darwin-arm64` |
| Linux x64 (glibc) | `@ferridriver/node-linux-x64-gnu` |
| Linux arm64 (glibc) | `@ferridriver/node-linux-arm64-gnu` |
| Windows x64 | `@ferridriver/node-win32-x64-msvc` |

## CLI / MCP server

```bash
# From crates.io
cargo install ferridriver-cli

# From GitHub releases (prebuilt binaries)
curl -fsSL https://github.com/salamaashoush/ferridriver/releases/latest/download/ferridriver-VERSION-TARGET.tar.gz | tar xz
```

## Test runner

```bash
npm install -D @ferridriver/test
# or
bun add -d @ferridriver/test
```

## Browser

The test runner can download Chromium for you:

```bash
npx @ferridriver/test install chromium
# With system dependencies (fonts, libs)
npx @ferridriver/test install --with-deps chromium
```

## System dependencies

No build-time system libraries required. Optional runtime dependency: `ffmpeg` on `PATH` for `--video` recording.

- **Ubuntu / Debian:** `sudo apt-get install -y pkg-config libclang-dev` (for building from source), `sudo apt-get install -y ffmpeg` (for `--video`)
- **macOS:** `brew install pkg-config`, `brew install ffmpeg`
- **WebKit backend:** macOS 11 or newer
- **Bidi backend:** Firefox with WebDriver BiDi enabled

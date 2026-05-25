# Installation

## One-line install (Linux, macOS)

```bash
curl -fsSL https://raw.githubusercontent.com/salamaashoush/ferridriver/main/install.sh | bash
```

Installs system dependencies (Linux), the `ferridriver` binary, and
Chromium for Testing.

Options:

```bash
curl -fsSL .../install.sh | bash -s -- --no-browser   # skip Chromium download
curl -fsSL .../install.sh | bash -s -- --deps-only    # system deps only
```

`FERRIDRIVER_INSTALL_DIR` (default `$HOME/.ferridriver/bin`) controls the
binary destination.

## Rust library

```toml
# Cargo.toml
[dependencies]
ferridriver = "0.2"
```

`ferridriver-test`, `ferridriver-bdd`, `ferridriver-expect`,
`ferridriver-mcp`, and the matching `*-macros` crates are published
alongside.

## CLI binary

```bash
# From crates.io
cargo install ferridriver-cli

# From GitHub releases (prebuilt binary)
curl -fsSL https://github.com/salamaashoush/ferridriver/releases/latest/download/ferridriver-VERSION-TARGET.tar.gz | tar xz
```

## Node.js and Bun

```bash
npm install @ferridriver/node
# or
bun add @ferridriver/node
```

The platform binary is pulled in via `optionalDependencies`:

| Platform           | Package                                |
|--------------------|----------------------------------------|
| macOS arm64        | `@ferridriver/node-darwin-arm64`       |
| Linux x64 (glibc)  | `@ferridriver/node-linux-x64-gnu`      |
| Linux arm64 (glibc)| `@ferridriver/node-linux-arm64-gnu`    |
| Windows x64        | `@ferridriver/node-win32-x64-msvc`     |

## Browsers

Download Chromium and Firefox into the local cache:

```bash
ferridriver install chromium                          # default
ferridriver install --with-deps chromium              # Linux: also install system libs
ferridriver install firefox chromium-headless-shell
```

Override the cache location with `FERRIDRIVER_BROWSERS_PATH`. Defaults:

- Linux: `~/.cache/ferridriver/`
- macOS: `~/Library/Caches/ferridriver/`
- Windows: `%LOCALAPPDATA%/ferridriver/`

### WebKit backend

The WebKit backend speaks Playwright's WebKit Inspector protocol. Today
the binary must be provided out-of-band:

- Set `FERRIDRIVER_WEBKIT` to a Playwright WebKit checkout containing
  `pw_run.sh`, **or**
- Install Playwright once (`npx playwright install webkit`) — ferridriver
  picks up the Playwright cache automatically.

## System dependencies

No build-time system libraries are required for the Rust crates beyond a
working linker and `pkg-config` (Linux).

Runtime dependencies:

- **Linux video recording (`--video`)** — install `ffmpeg`.
- **Firefox backend (`bidi`)** — install Firefox (`apt`, `brew`, `pacman`); ferridriver does not bundle it.
- **WebKit backend** — Playwright WebKit binary as above.

## Platform support

- Linux x86_64 / aarch64 (glibc)
- macOS arm64 (Apple Silicon) and x86_64
- Windows x64

All four backends are available on all platforms.

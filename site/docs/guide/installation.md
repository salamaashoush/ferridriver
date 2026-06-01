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
ferridriver = "0.4"
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

## Browsers

Download browser binaries into the local cache. Supported targets:
`chromium`, `chromium-headless-shell`, `firefox`, `webkit`.

```bash
ferridriver install chromium                          # default
ferridriver install --with-deps chromium              # Linux: also install system libs
ferridriver install firefox chromium-headless-shell
ferridriver install webkit                            # Playwright WebKit build
```

Override the cache location with `FERRIDRIVER_BROWSERS_PATH`. Defaults:

- Linux: `~/.cache/ferridriver/`
- macOS: `~/Library/Caches/ferridriver/`
- Windows: `%LOCALAPPDATA%/ferridriver/`

### WebKit backend

The WebKit backend speaks Playwright's WebKit Inspector protocol over
`pw_run.sh`. The binary is discovered in this order:

- `FERRIDRIVER_WEBKIT`, pointed at a Playwright WebKit checkout containing
  `pw_run.sh`, **or**
- the Playwright cache (`npx playwright install webkit`) — ferridriver
  picks it up automatically, **or**
- the ferridriver cache, populated by `ferridriver install webkit`.

WebKit is available on Linux and macOS only (the `--inspector-pipe`
transport relies on a Unix fork/dup model). It is not native WKWebView.

## System dependencies

No build-time system libraries are required for the Rust crates beyond a
working linker and `pkg-config` (Linux).

Runtime dependencies:

- **Linux video recording (`--video`)** — install `ffmpeg`.
- **Firefox backend (`bidi`)** — install Firefox (`apt`, `brew`, `pacman`); ferridriver does not bundle it.
- **WebKit backend** — Playwright WebKit binary as above.

## Platform support

The prebuilt `ferridriver` CLI binary ships for Linux x86_64 / aarch64
(musl) and macOS x86_64 / arm64. There is no prebuilt Windows CLI binary
(`cdp-pipe`'s fd 3/4 transport and signal handling are Unix-only); build
from source if you need other targets.

The `@ferridriver/node` NAPI binary ships for macOS arm64 and Linux
x86_64 / aarch64 (glibc).

The CDP backends (`cdp-pipe`, `cdp-raw`) and the Firefox `bidi` backend
run on Linux and macOS. The `webkit` backend runs on Linux and macOS
only.

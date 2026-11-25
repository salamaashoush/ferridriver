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
`chromium`, `chromium-headless-shell`, `chromedriver`, `firefox`, `webkit`,
`ladybird`, `lightpanda`.

```bash
ferridriver install chromium                          # default
ferridriver install --with-deps chromium              # Linux: also install system libs
ferridriver install firefox chromium-headless-shell
ferridriver install webkit                            # Playwright WebKit build
ferridriver install chromium chromedriver             # ChromeDriver of that Chromium's version
ferridriver install ladybird lightpanda               # the two independent engines
```

`chromedriver` follows the newest Chromium in the cache, since a driver
only starts the browser of its own version.

Override the cache location with `FERRIDRIVER_BROWSERS_PATH`. Defaults:

- Linux: `~/.cache/ferridriver/`
- macOS: `~/Library/Caches/ferridriver/`
- Windows: `%LOCALAPPDATA%/ferridriver/`

Installing a browser removes the older versions of that same browser from
the cache, and prints each one it removed. A version newer than the one just
installed is kept (a beta beside a stable build, for example), and so is every
other browser. Pass `--no-remove` to keep everything:

```bash
ferridriver install --no-remove chromium
```

### WebKit backend

The WebKit backend speaks Playwright's WebKit Inspector protocol over
`pw_run.sh`. The binary is discovered in this order:

- `FERRIDRIVER_WEBKIT`, pointed at a Playwright WebKit checkout containing
  `pw_run.sh`, **or**
- the Playwright cache (`npx playwright install webkit`) — ferridriver
  picks it up automatically, **or**
- the ferridriver cache, populated by `ferridriver install webkit`.

`ferridriver install webkit` takes the build the latest Playwright release
ships. To pin one, as a project on a fixed `playwright-core` does, pass the
revision from that release's `browsers.json` to
`BrowserInstaller::install_webkit_revision`. It installs into the Playwright
cache as `playwright install` does, where both Playwright and ferridriver find
it, and keeps a build already installed there. `with_download_hosts` points
the download at a mirror instead of Playwright's CDN.

WebKit is available on Linux and macOS only (the `--inspector-pipe`
transport relies on a Unix fork/dup model). It is not native WKWebView.

### Ladybird

`ferridriver install ladybird` installs the newest release listed in
`download.ladybird.org/releases.json`: the macOS disk image (Apple
Silicon), or on Linux the package from Ladybird's apt repository for x86_64
or arm64, unpacked into the cache rather than installed system-wide. Each
download is checked against the SHA-256 Ladybird publishes. The Linux build
needs glibc 2.39 or newer and the X11/Wayland libraries any desktop has.

The browser is found in this order: `LADYBIRD_PATH` (the `Ladybird`
executable), the ferridriver cache, then `/Applications/Ladybird.app` or
`~/Applications/Ladybird.app` on macOS and `/opt/ladybird` on Linux. A
source build works too: point `LADYBIRD_PATH` or `executablePath` at its
`bin/Ladybird`. ferridriver drives it through the `WebDriver` service that
sits beside the browser, so the build must include it.

### Lightpanda

`ferridriver install lightpanda` installs the newest Lightpanda release
from its GitHub releases (macOS and Linux, x86_64 and arm64), checked
against the SHA-256 GitHub lists for it. Set `GITHUB_TOKEN` to lift the
anonymous API rate limit on shared CI addresses. The executable is found in
this order: `LIGHTPANDA_PATH`, the ferridriver cache, then `lightpanda` on
`PATH` (Homebrew's `lightpanda-io/browser/lightpanda`, the AUR's
`lightpanda-nightly-bin`). Lightpanda has no Windows build; run it in WSL.

## System dependencies

No build-time system libraries are required for the Rust crates beyond a
working linker and `pkg-config` (Linux).

Runtime dependencies:

- **Video recording (`--video`)** — `ferridriver install ffmpeg` fetches the ffmpeg Playwright records with; an `ffmpeg` on `PATH` works too.
- **Firefox backend (`bidi`)** — install Firefox (`apt`, `brew`, `pacman`); ferridriver does not bundle it.
- **WebKit backend** — Playwright WebKit binary as above.
- **Ladybird, Lightpanda** — `ferridriver install ladybird` / `lightpanda` as above.

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

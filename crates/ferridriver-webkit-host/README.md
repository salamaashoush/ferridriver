# ferridriver-webkit-host

Linux WebKit host subprocess for ferridriver. Drives `webkit6::WebView` on
a GTK4 main loop, exposing the same binary IPC protocol (defined in
[`ferridriver-webkit-wire`](../ferridriver-webkit-wire/)) that the macOS
Obj-C host (`crates/ferridriver/src/backend/webkit/host.m`) speaks.

## Status

**Phase 2 scaffold.** The dispatcher loop and Op enum are wired; concrete
WebKitGTK 6 handlers land incrementally. Every Op currently returns
`Rep::Error "unsupported: not yet implemented"` except:

- `ListViews` — returns an empty `ViewList` so the parent's launch probe
  succeeds.
- `Shutdown` — exits cleanly.

See `docs/webkit-linux-port.md` for the full plan.

## System dependencies

The host binary is Linux-only and (once Phase 2b lands) will require:

```sh
# Ubuntu / Debian (22.04 LTS does NOT have libwebkitgtk-6.0 — use 24.04+)
sudo apt-get install -y libwebkitgtk-6.0-dev libgtk-4-dev libsoup-3.0-dev \
                        libjavascriptcoregtk-6.0-dev xvfb

# Arch
sudo pacman -S webkitgtk-6.0 gtk4 libsoup3 xorg-server-xvfb
```

On non-Linux targets the binary stub-compiles and exits with code 64
("EX_USAGE") to make the wrong-platform error obvious.

## Running

```sh
# Built by `cargo build --workspace`.
ls target/debug/ferridriver-webkit-host

# Normally launched as a subprocess by `WebKitBrowser::launch()` —
# the parent creates a Unix socketpair and passes the child end as fd 3.
# To run standalone for debugging:
FERRIDRIVER_WEBKIT_HOST=$(pwd)/target/debug/ferridriver-webkit-host \
  cargo run --bin ferridriver -- bdd ...
```

## Headless

WebKitGTK 6 needs a display. On CI and headless dev boxes, wrap the
ferridriver test command with `xvfb-run -a`:

```sh
xvfb-run -a just test-backend webkit
```

Real headless via WPE backend is out of scope; see
`docs/webkit-linux-port.md` §8.

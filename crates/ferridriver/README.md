# ferridriver

[![crates.io](https://img.shields.io/crates/v/ferridriver.svg?logo=rust&color=c97b4a)](https://crates.io/crates/ferridriver)
[![docs.rs](https://img.shields.io/docsrs/ferridriver?logo=docs.rs&color=c97b4a)](https://docs.rs/ferridriver)
[![MSRV](https://img.shields.io/badge/MSRV-1.91-c97b4a?logo=rust)](https://github.com/salamaashoush/ferridriver/blob/main/rust-toolchain.toml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-c97b4a.svg)](https://github.com/salamaashoush/ferridriver)

Browser automation written in Rust with a Playwright-compatible API. Four
backends behind one surface — `Browser`, `BrowserContext`, `Page`, `Frame`,
`Locator`, `ElementHandle` — selected per launch.

| Backend kind         | Browser            | Transport |
|----------------------|--------------------|-----------|
| `BackendKind::CdpPipe` (default) | Chromium / Chrome | CDP over Unix pipes (fd 3/4) |
| `BackendKind::CdpRaw`            | Chromium / Chrome | CDP over WebSocket |
| `BackendKind::WebKit`            | Playwright WebKit | Playwright Inspector protocol over `pw_run.sh` |
| `BackendKind::Bidi`              | Firefox           | WebDriver BiDi over WebSocket |

This crate is the Rust core. For the test runner, BDD framework, MCP
server, CLI binary, or Node binding, depend on `ferridriver-test`,
`ferridriver-bdd`, `ferridriver-mcp`, `ferridriver-cli`, or
`@ferridriver/node` respectively.

## Usage

```rust
use ferridriver::browser_type::chromium;
use ferridriver::options::LaunchOptions;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let browser = chromium().launch(LaunchOptions::default()).await?;
    let page = browser.page().await?;

    page.goto("https://example.com", None).await?;
    let title = page.title().await?;
    println!("{title}");

    page.locator("#search").fill("rust").await?;
    page.locator("button[type=submit]").click().await?;
    page.wait_for_load_state(Some("networkidle")).await?;

    browser.close().await?;
    Ok(())
}
```

`firefox()` and `webkit()` are factories with the same shape.
`BrowserType::with_backend(...)` and `chromium_with(BrowserTypeOptions { ... })`
let you pin a backend explicitly.

## Public API surface

- `Browser::launch`, `Browser::connect`, `Browser::context`, `Browser::page`, `Browser::close`
- `BrowserContext::new_page`, `pages`, `cookies`, `add_cookies`, `clear_cookies`,
  `grant_permissions`, `set_geolocation`, `add_init_script`, `route`,
  `set_extra_http_headers`, `storage_state`, `close`
- `Page::goto`, `go_back`, `go_forward`, `reload`, `url`, `title`, `content`,
  `markdown`, `evaluate`, `evaluate_handle`, `screenshot`, `pdf`, `route`,
  `wait_for_url`, `wait_for_load_state`, `wait_for_navigation`,
  `wait_for_event`, `wait_for_request`, `wait_for_response`,
  `wait_for_dialog`, `wait_for_file_chooser`, `wait_for_download`,
  `add_init_script`, `expose_function`, `set_extra_http_headers`,
  `emulate_media`, `set_viewport_size`, `start_tracing`, `stop_tracing`,
  `keyboard()`, `mouse()`, `touchscreen()`, plus the shorthand action and
  query methods (`click`, `fill`, `is_visible`, `text_content`, …).
- `Locator`: lazy, strict-by-default. `click`, `fill`, `hover`, `tap`,
  `drag_to`, `select_option`, `check`, `uncheck`, `set_input_files`,
  `screenshot`, `evaluate`, `evaluate_all`, `count`, `all`, `first`,
  `last`, `nth`, `and`, `or`, `filter`, `locator`, `frame_locator`,
  `get_by_role`, `get_by_text`, `get_by_label`, `get_by_placeholder`,
  `get_by_alt_text`, `get_by_title`, `get_by_test_id`, `wait_for`.
- `Frame`, `FrameLocator`, `ElementHandle`, `Route`, `Request`, `Response`,
  `Dialog`, `Download`, `FileChooser`, `Video`, `JSHandle`.

## Errors

`FerriError` is the single public error type. Variants the user is most
likely to match on:

| Variant                     | Helper                  | Notes |
|-----------------------------|-------------------------|-------|
| `Timeout`                   | `is_timeout_error()`    | Auto-wait or `expect` timeout. `name()` returns `"TimeoutError"` for JS interop. |
| `TargetClosed`              | `is_target_closed_error()` | Page or browser closed mid-operation. |
| `StrictModeViolation`       | `is_strict_mode_violation()` | Locator resolved to >1 element. |
| `Unsupported`               | `is_unsupported()`      | Backend cannot perform the operation (e.g. HAR on BiDi). |
| `InvalidSelector`           |                         | Selector failed to parse. |
| `Navigation` / `Protocol` / `Backend` / `Evaluation` / `Snapshot` / `Io` / `Json` |  | Underlying failure surfaces. |

## Selectors

Full Playwright selector engine — CSS (default), `role=`, `text=`, `label=`,
`placeholder=`, `alt=`, `title=`, `testid=`, `xpath=`, `id=`, `nth=`,
`visible=true`, `has=`, `has-text=`, `has-not=`, `has-not-text=`, chained
with `>>`. The engine compiles in Rust and is injected once per page; no
per-query JS eval.

## Feature parity by backend

All four backends speak the same `Browser` / `Page` / `Locator` surface.
Genuinely-unsupported operations return `FerriError::Unsupported` rather
than placeholder values. Today's gaps:

- HAR recording: BiDi returns `Unsupported`.
- Download interception: BiDi returns `Unsupported`.
- CDP-only tracing: WebKit and BiDi.

For everything else, the same test runs on every backend without change.

## License

MIT OR Apache-2.0

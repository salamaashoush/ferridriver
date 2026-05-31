# Troubleshooting

Common failures and their fixes. If your problem is not here, file an
issue at <https://github.com/salamaashoush/ferridriver/issues> with the
output of `ferridriver -vv ...` and a minimal repro.

## Install

### `command not found: ferridriver`

The binary is not on `PATH`. The install script drops it in
`$HOME/.ferridriver/bin` (override with `FERRIDRIVER_INSTALL_DIR`). Add
that to `PATH` or use the cargo path:

```bash
export PATH="$HOME/.cargo/bin:$HOME/.ferridriver/bin:$PATH"
```

### `cargo install ferridriver-cli` fails on Linux

You need `pkg-config` and `libclang-dev` for some transitive deps:

```bash
sudo apt-get install -y pkg-config libclang-dev   # Debian / Ubuntu
sudo pacman -S pkgconf clang                       # Arch
sudo dnf install pkg-config clang-devel            # Fedora
```

### `ferridriver install chromium` hangs / times out

Behind a proxy: set `HTTPS_PROXY` and `HTTP_PROXY` before running.
ferridriver retries downloads up to 5 times before giving up.

If you already have Chrome / Chromium installed elsewhere, point at it
instead and skip the download:

```bash
FERRIDRIVER_BROWSERS_PATH=/path/to/your/cache ferridriver mcp
# or per-launch:
LaunchOptions { executable_path: Some("/usr/bin/chromium".into()), .. }
```

## Browsers

### `Chromium not found` / `Failed to launch browser`

ferridriver searches: `FERRIDRIVER_BROWSERS_PATH`, then the platform
cache (`~/.cache/ferridriver/` on Linux,
`~/Library/Caches/ferridriver/` on macOS,
`%LOCALAPPDATA%/ferridriver/` on Windows), then the system Chromium.

Fix:

```bash
ferridriver install chromium
# or
ferridriver install --with-deps chromium   # also installs Linux libs
```

### Linux: Chromium starts then crashes with `SUID sandbox`

Either install setuid Chromium dependencies (`--with-deps` covers them)
or disable the sandbox for headless CI:

```rust
LaunchOptions {
    args: vec!["--no-sandbox".into(), "--disable-setuid-sandbox".into()],
    ..Default::default()
}
```

### `Playwright WebKit binary not found`

Download the WebKit binary, or point ferridriver at an existing one:

```bash
# Download into the ferridriver cache
ferridriver install webkit

# Or install via Playwright (one-time)
npx playwright install webkit

# Or set the override
export FERRIDRIVER_WEBKIT=/path/to/playwright/webkit-XXXX/
```

ferridriver searches: `FERRIDRIVER_WEBKIT`, then the Playwright cache
(`~/.cache/ms-playwright/webkit-*` on Linux,
`~/Library/Caches/ms-playwright/webkit-*` on macOS), then the
ferridriver cache.

### Firefox: `firefox: command not found`

Install Firefox locally — ferridriver does not bundle it. Then either
put it on `PATH` or set the launch option:

```rust
LaunchOptions {
    executable_path: Some("/usr/bin/firefox".into()),
    ..Default::default()
}
```

## Tests

### Tests flake at `workers = 8` but pass at `workers = 4`

You have a hidden shared-state dependency — a database row, a
localStorage key, a login session, a port. Find it; do not lower the
worker count as a workaround.

Bisect by tag:

```bash
ferridriver bdd --workers 8 --tags '@db'        # narrow scope
ferridriver bdd --workers 8 --tags 'not @db'
```

### `Timeout: element not actionable`

The element existed at 5s but wasn't visible, attached, or enabled.
Common causes:

- Element is behind a modal / overlay → close it first.
- Element only appears after a network call → assert visibility
  explicitly first: `expect(&loc).to_be_visible().with_timeout(...)`.
- Element is in an iframe → use `page.frame_locator("iframe").locator(...)`.

### `StrictModeViolation: locator resolved to 3 elements`

Selectors are strict by default. Either narrow the selector or pick
one:

```rust
page.locator(".btn").first().click().await?;
page.locator(".btn").nth(1).click().await?;
page.locator(".btn:visible").click().await?;
```

### `to_have_text` fails but the text looks identical

Whitespace. ferridriver compares `text_content()` which preserves
internal whitespace. Use `to_contain_text` or normalize:

```rust
expect(&loc).to_have_text(text.trim()).await?;
```

### Assertion times out at exactly 5000ms every time

Default `expectTimeout`. Either fix the underlying wait or bump the
timeout:

```rust
expect(&loc).to_be_visible().with_timeout(Duration::from_secs(30)).await?;
```

```toml
[test]
expectTimeout = 10000
```

## MCP server

### Claude / Cursor can't find the server

The client launches `ferridriver` from its own `PATH`, which usually
isn't your shell `PATH`. Use an absolute path in the config:

```json
{
  "mcpServers": {
    "ferridriver": {
      "command": "/Users/you/.ferridriver/bin/ferridriver",
      "args": ["mcp"]
    }
  }
}
```

### `run_script` reports `sandbox_violation`

The script tried to import `node_modules` or read outside `script_root`.
Bare specifiers (`import 'lodash'`) are rejected — bundle helpers via
the extension system or read paths relative to `script_root`.

### `process.env` is empty

By design. Allow specific keys via the config:

```toml
[scripting]
allowEnv = ["HOME", "TZ", "DATABASE_URL"]
```

Names not listed remain absent. There is no way to widen this from
inside a script.

### MCP refs (`[ref=eN]`) point at the wrong element

Refs are tied to the most recent `snapshot`. After `navigate`,
`page(select)`, or any DOM-mutating `run_script`, old refs are stale.
Re-snapshot before clicking. Prefer Playwright-style locators
(`getByRole`, `getByText`) inside `run_script` — they survive DOM churn.

## CI

### CI is 3× slower than local

Likely missing browser cache between runs. Add:

```yaml
- uses: actions/cache@v4
  with:
    path: ~/.cache/ferridriver
    key: ${{ runner.os }}-ferridriver-browsers-v1
```

See [Recipes → CI](/recipes/ci-github-actions).

### Tests pass locally, fail in CI

Common causes:

- **CPU contention.** CI runners are slower than laptops. Either pin
  `workers = 2` or bump `timeout` for CI:

  ```toml
  [test.profiles.ci]
  timeout = 60000
  expectTimeout = 10000
  workers = 2
  ```

  Run with `--profile ci`.

- **Missing system fonts.** Some rendering tests rely on fonts your
  local machine has installed. Install Liberation Sans /
  DejaVu / Noto in CI.

- **Headless behavior.** A few sites detect headless mode and serve
  different HTML. Run locally without `--headless` (MCP and `run` are
  headed by default) to confirm.

## Performance

### `--workers 8` is slower than `--workers 4`

Past 4–8 workers you start thrashing I/O and RAM on most laptops and
small CI runners. Each worker is a full Chromium process — RAM usage
scales linearly.

### Browser launches dominate the run

You may be launching too often. The default model is one browser per
worker per run; if you see "launch" lines for every test, a fixture or
hook is closing the worker browser between tests instead of reusing it.

### Build is slow

Use the `release-fast` profile for iteration:

```bash
just build-fast
```

Thin LTO and parallel codegen cut release build time by ~40% vs the
default `release` profile (full LTO).

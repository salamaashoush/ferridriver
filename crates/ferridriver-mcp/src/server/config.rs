//! Configuration trait and defaults for [`crate::server::McpServer`].

use ferridriver::state::ConnectMode;

/// Trait for customizing the MCP server behavior.
///
/// Implement this to control chrome args, browser instance resolution,
/// server metadata, and pre-dispatch validation. The library stays generic --
/// any domain-specific concepts (environments, auth, etc.) belong in the
/// consumer's own `ServerHandler` wrapper.
pub trait McpServerConfig: Send + Sync + 'static {
  /// Root directory for the scripting artifacts used by `run_script`.
  ///
  /// All `fs` operations inside scripts (`readFile`, `writeFile`, `readdir`,
  /// `exists`) and all dynamic `import(...)` calls are constrained to this
  /// directory — traversal (`..`), absolute paths, and symlink escapes are
  /// rejected. The directory is created at server startup if it does not
  /// exist.
  ///
  /// Default: `./.ferridriver/scripts` relative to cwd. The dotfolder
  /// convention avoids colliding with the common `scripts/` directory most
  /// projects already use for build/CI tooling, and leaves room for sibling
  /// subdirectories (`.ferridriver/artifacts`, `.ferridriver/cache`, ...)
  /// without further namespace pollution.
  fn script_root(&self) -> std::path::PathBuf {
    std::path::PathBuf::from(".ferridriver/scripts")
  }

  /// Root directory for script output artifacts (screenshots, PDFs, traces,
  /// downloaded bodies). Exposed to scripts as the `artifacts` global.
  ///
  /// Kept separate from `script_root` so outputs don't pollute the source
  /// tree. Same artifacts rules apply. The directory is created at server
  /// startup if it does not exist.
  ///
  /// Default: `./.ferridriver/artifacts` relative to cwd.
  fn artifacts_root(&self) -> std::path::PathBuf {
    std::path::PathBuf::from(".ferridriver/artifacts")
  }

  /// Ceiling on the total size of [`Self::artifacts_root`], in bytes.
  ///
  /// A server that stays up for days accumulates screenshots and traces
  /// from calls whose results were read and forgotten. When set, each call
  /// that writes an artifact sweeps the directory back under the ceiling by
  /// deleting least-recently-modified files — never the ones it just wrote.
  ///
  /// Default: no ceiling.
  fn artifacts_max_bytes(&self) -> Option<u64> {
    None
  }

  /// Values that must not reach a caller verbatim, as `name -> value`.
  ///
  /// Every response the server renders — a returned value, a console line, a
  /// page URL, echoed code — has these replaced by `<secret>NAME</secret>`,
  /// and echoed code reads them from the environment instead. A convenience
  /// rather than a security boundary: only declared values are matched.
  ///
  /// Default: none, so nothing is redacted.
  fn secrets(&self) -> ferridriver::response::Secrets {
    ferridriver::response::Secrets::default()
  }

  /// Engine-level defaults (timeout, memory, console limits) for `run_script`.
  fn script_engine_config(&self) -> ferridriver_script::ScriptEngineConfig {
    ferridriver_script::ScriptEngineConfig::default()
  }

  /// Base Chrome arguments applied to ALL browser instances.
  ///
  /// Called once at server construction. Override to inject flags that
  /// apply globally (e.g. shared proxy settings).
  fn chrome_args(&self) -> Vec<String> {
    Vec::new()
  }

  /// Launch settings for a specific browser instance: extra arguments,
  /// profile directory, executable, headless, backend and environment.
  ///
  /// Called before launching a browser for the given instance name,
  /// which comes from the composite session key
  /// `"<instance>:<context>"`. Override to inject per-instance DNS
  /// resolver rules, a persistent profile, or an environment.
  ///
  /// # Errors
  ///
  /// Return `Err` to ABORT the launch (e.g. the instance name maps to no
  /// environment). Launching anyway would put the caller on a browser
  /// that looks configured and is not.
  fn instance_overrides(&self, _instance: &str) -> Result<ferridriver::options::InstanceOverrides, String> {
    Ok(ferridriver::options::InstanceOverrides::default())
  }

  /// Instance names this config defines, so a bare session key that
  /// names one selects that instance instead of being read as a context
  /// on `default`.
  fn instance_names(&self) -> Vec<String> {
    Vec::new()
  }

  /// Section-level launch settings, resolved WITHOUT running any
  /// operator command.
  ///
  /// Read once while the server is being constructed, which is a
  /// synchronous path on the async runtime — [`Self::instance_overrides`]
  /// may shell out for up to its command timeout and would stall the
  /// reactor before the server ever serves.
  fn base_overrides(&self) -> ferridriver::options::InstanceOverrides {
    ferridriver::options::InstanceOverrides::default()
  }

  /// Viewport emulated on the pages this server opens.
  ///
  /// Playwright's default (1280x720) rather than `None`: returning
  /// nothing here means no `Emulation.setDeviceMetricsOverride` at all,
  /// which is Playwright's `viewport: null`. On a persistent profile
  /// that leaves every page at whatever window bounds Chrome restored
  /// from the profile's last run, so one `--window-size` launch pins the
  /// size of every session afterwards. `None` stays reachable through an
  /// explicit `viewport: null`.
  fn default_viewport(&self) -> Option<ferridriver::options::ViewportConfig> {
    Some(ferridriver::options::ViewportConfig::default())
  }

  /// Options of the contexts this server opens pages in (a device's
  /// viewport, user agent, scale factor, mobile and touch flags), beyond
  /// the viewport [`Self::default_viewport`] gives. `None` leaves those
  /// contexts with the browser's own.
  fn default_context_options(&self) -> Option<ferridriver::options::BrowserContextOptions> {
    None
  }

  /// Resolve how to connect to a browser instance by name.
  ///
  /// Called before launching a new browser. If this returns `Some(ConnectMode)`,
  /// ferridriver connects to an existing browser instead of launching a new one.
  ///
  /// Use this to integrate with external browser managers:
  /// - Read a `DevToolsActivePort` file from a known profile directory
  /// - Query a service registry for running browser endpoints
  /// - Connect to a browser launched by another tool with debugging enabled
  ///
  /// The instance name comes from the session key (e.g. `"staging"` from `"staging:admin"`).
  /// Return `None` to fall through to the default behavior (launch a new browser).
  fn resolve_instance(&self, _instance: &str) -> Option<ConnectMode> {
    None
  }

  /// Validate that an instance can be started, before launching a browser.
  ///
  /// Called on the cold-start path with the instance parsed from the session
  /// key. Return `Err(message)` to abort with an actionable error instead of
  /// launching (e.g. the session key resolved to a bogus instance). Default
  /// allows everything.
  ///
  /// # Errors
  ///
  /// Returns `Err` with a user-facing message when `instance` must not be
  /// launched (surfaced as the tool error).
  fn instance_health(&self, _instance: &str) -> Result<(), String> {
    Ok(())
  }

  /// Server name for MCP `get_info`.
  fn server_name(&self) -> &str {
    DEFAULT_SERVER_NAME
  }

  /// Server instructions for MCP `get_info`.
  fn server_instructions(&self) -> &str {
    DEFAULT_INSTRUCTIONS
  }

  /// Milliseconds to let work an action triggered settle before the
  /// snapshot that follows. `0` disables the wait.
  fn settle_timeout_ms(&self) -> u64 {
    DEFAULT_SETTLE_TIMEOUT_MS
  }
}

/// Long enough for the fetch a click kicked off to land, short enough
/// that a polling page does not hold the tool open. Playwright's MCP
/// settles on the same number.
pub const DEFAULT_SETTLE_TIMEOUT_MS: u64 = 500;

/// Default server name for MCP `get_info`.
pub const DEFAULT_SERVER_NAME: &str = "ferridriver";

/// Default instructions embedded in the MCP server.
pub const DEFAULT_INSTRUCTIONS: &str = "\
Browser automation via Chrome DevTools Protocol.\n\
\n\
== RECOMMENDED WORKFLOW ==\n\
1. `navigate` or `connect` to bring up a session.\n\
2. `snapshot` to see the page as an accessibility tree (ref=eN handles, text, roles) \
BEFORE deciding on selectors. Cheap, fast, low token cost — always your first action.\n\
3. Act via one of:\n\
   a. `run_script` — sandboxed JS with full `page`, `context`, `request` globals for \
imperative logic (loops, conditionals, try/catch, computed values, HTTP calls). \
Pair with `args` to avoid string interpolation. This is the primary action tool.\n\
   b. `evaluate` — single-line JS executed IN the page (DOM context). Use for \
quick reads; use `run_script` for anything multi-step.\n\
4. `snapshot` again to verify.\n\
\n\
Browser interaction flows through `run_script` bindings:\n\
- Clicks, fills, hovers → `await page.click(sel)`, `await page.fill(sel, val)`, \
`await page.locator(sel).hover()`.\n\
- Locator chains → `page.getByRole('button', ...).first().click()`.\n\
- Cookies, storage, geolocation → `await context.addCookies([...])`, \
`await context.setGeolocation(...)`.\n\
- Waits → `await page.waitForSelector(sel, { state, timeout })`.\n\
- API calls → `await request.get(url)`, `await request.post(url, { json: {...} })`.\n\
- Saving outputs (screenshots, PDFs, traces) → `await artifacts.writeBytes('page.png', \
await page.screenshot())`. The `artifacts` global is rooted at the server's configured \
artifacts_root (default `.ferridriver/artifacts/`) — separate from script source so outputs \
don't pollute your tree.\n\
\n\
== SESSION KEYS ==\n\
All tools accept an optional 'session' parameter. Format: 'instance:context'.\n\
- Instance (before ':') selects which browser process. Each instance can have its own \
Chrome flags, DNS rules, and profile. Examples: 'staging', 'dev', 'prod'.\n\
- Context (after ':') isolates cookies/storage within that browser. Use for multi-user \
testing. Examples: 'admin', 'user1', 'tester'.\n\
- Combined: 'staging:admin' = staging browser, admin context.\n\
- Plain name without ':' uses the default instance: 'mytest' = 'default:mytest'.\n\
- Omitted entirely: uses 'default:default'.\n\
- `run_script` `vars` persist per session: values set via `vars.set(...)` in one call \
are visible to the next `run_script` with the same session. The `vars` global is a \
plain string key/value store (use JSON.stringify for complex values).\n\
\n\
== SNAPSHOTS AND REFS ==\n\
`snapshot` returns an accessibility tree with [ref=eN] identifiers. Refs are tied to \
that specific snapshot — after `navigate`, `page(select)`, or any DOM mutation, old \
refs are invalid. Re-snapshot before acting. When scripting, prefer Playwright-style \
locators (`page.getByRole`, `page.getByText`, `page.locator(selector)`) over refs \
— they survive re-snapshots.\n\
\n\
== TAB MANAGEMENT ==\n\
`page(action='list')` lists tabs, `page(action='select', page_index=N)` switches. A tab \
a page opens (a popup, a target=_blank link) is listed under `### Open tabs` in the \
next response and never takes focus by itself: the page you were on stays current until \
you select another. Do not use `evaluate` or `run_script` to enumerate tabs — CDP \
page-target mapping is only exposed via the `page` tool.\n\
\n\
== SCRIPTING SAFETY ==\n\
`run_script` runs in a sandboxed QuickJS runtime: no raw filesystem access (only \
`fs.*` (Node) for source files + `artifacts.*` rooted at artifacts_root \
for outputs), no runner-side network except via `request.*` (HttpClient), no \
`process` / `require` / bare `import`. Caller-controlled data MUST be passed via the \
`args` array, never interpolated into the `source` string — the engine does not protect \
against source-level injection.";

/// Default config for standalone ferridriver (no customization).
pub struct DefaultConfig;
impl McpServerConfig for DefaultConfig {}

# Protocol deadlines

Rust core owns the deadline for a native `page.webmcp` or `frame.webmcp`
operation. Discovery, version lookup, script injection and tool execution share
one absolute deadline. Nested work can shorten it but cannot extend it.
Omitting `timeout` uses the page default; `timeout: 0` removes the client
deadline unless an enclosing operation already has a finite deadline.

CDP, BiDi and WebKit inherit that budget instead of imposing their ordinary
command timeout. Queue admission counts against it. WebKit's outer transport
acknowledgement and inner target response use the same deadline. Cancelling a
wait removes its pending response entry; a late reply does not recreate it.
Cancellation does not undo work already dispatched to the browser, and tool
invocations are never automatically replayed.

The deadline pauses while ferridriver's debugger is parked. Cleanup uses a
separate wall-clock budget. Provider-side timers remain wall-clock timers;
parking the client cannot suspend a timer already running inside a remote
driver.

## Classic WebDriver and mobile providers

Classic WebDriver serializes target selection and execution. The selected
session receives the remaining script timeout immediately before scoped script
execution. An unlimited budget sends the standard `script: null` value. A
subsequent ordinary script restores its ordinary timeout under the same
selection lock, so concurrent callers cannot inherit each other's settings.

Dropping a queued caller prevents dispatch. Dropping an in-flight caller keeps
the selection owner alive until the reply or deadline; another target cannot
overtake a still-running command. If the client deadline expires while remote
state is uncertain, further commands fail and explicit close remains available.

Providers impose additional limits:

- Safari 26.6.1 accepted a 45,000 ms script timeout and completed one 31-second
  Promise evaluation. It rejected `script: null` with `invalid argument`.
  Ferridriver reports `Unsupported` before invoking a tool when a provider
  rejects the unlimited setting.
- XCUITest's asynchronous atom execution requires a finite budget within the
  negotiated `appium:webviewAtomWaitTimeout`, whose pinned driver default is
  120,000 ms. An incompatible budget is rejected before execution.

These transport guarantees do not imply native WebMCP availability in every
browser. Native discovery still reports `Unsupported` where the browser does
not expose its WebMCP testing API. Ordinary automation APIs outside the scoped
WebMCP path retain their existing timeout behavior.

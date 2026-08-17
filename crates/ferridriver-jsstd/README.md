# ferridriver-jsstd

Vendored subset of [awslabs/llrt](https://github.com/awslabs/llrt) (Apache
License 2.0), providing the WHATWG Streams implementation, the `node:os`
module, and the pieces they depend on for the ferridriver QuickJS runtime.

Upstream: `0.8.1-beta`, re-synced against `awslabs/llrt@e987d2b` (main, 2026-08-16).

| upstream crate     | module here  |
| ------------------ | ------------ |
| `llrt_utils`       | `utils`      |
| `llrt_context`     | `context`    |
| `llrt_exceptions`  | `exceptions` |
| `llrt_events`      | `events`     |
| `llrt_abort`       | `abort`      |
| `llrt_encoding`    | `encoding`   |
| `llrt_buffer`      | `buffer`     |
| `llrt_json`        | `json`       |
| `llrt_crypto`      | `crypto`     |
| `llrt_os`          | `os`         |
| `llrt_stream_web`  | `stream_web` |
| `llrt_url`         | `url`        |
| `llrt_util`        | `text`       |
| `llrt_test`        | `test` (dev) |

The rest of llrt — its hyper/fetch stack, timers, console — is deliberately
not vendored: ferridriver has its own, over `reqwest`. `os` is vendored
because ferridriver has nothing equivalent and the module is pure host
introspection with no overlap with the automation stack. From `llrt_util`
only the four text codecs are taken (`TextEncoder`, `TextDecoder` and their
stream forms); its `format` / `inherits` / `inspect` are `node::util`'s,
which are richer.

## What a host installs

Two entry points, and nothing else to remember:

- `jsstd::init(ctx)` — every global this crate provides: `DOMException`,
  `Event` / `EventTarget`, `AbortController` / `AbortSignal`, the Streams
  surface, `Buffer` / `Blob` / `File`, `crypto`, `TextEncoder` /
  `TextDecoder` (+ their stream forms), `URL` / `URLSearchParams`, `atob` /
  `btoa`, `structuredClone` and `performance`.
- `jsstd::modules::modules()` — every Node / web MODULE it serves, each
  entry carrying its specifiers, the `ModuleDef` the ES loader declares,
  and the object `require()` returns: `path`, `buffer`, `os`, `util`,
  `events`, `assert` (+ `/strict`), `url`, `process`, `timers` (+
  `/promises`), `crypto`. The host merges that list into its own loader,
  its `require` table and its bundler's external list, so the three
  cannot drift apart.

## `src/node/` — ferridriver-authored

Not everything Node exposes has a usable upstream in llrt. `llrt_util` is
`TextEncoder`/`TextDecoder` plus `format` and `inherits` (no `promisify`, no
`inspect`, no `types`), and `llrt_assert` is a single `ok`. Those modules are
written here instead, under `src/node/`, so the runtime still has exactly one
implementation of each surface:

| module | why it is ours |
| ------ | -------------- |
| `node::inspect` | The `util.inspect` / `util.format` renderer, moved out of `ferridriver-script`'s `console` so `console.log`, `util.format` and `util.inspect` cannot drift apart |
| `node::deep_equal` | Structural equality for `util.isDeepStrictEqual` (and `assert.deepStrictEqual` when it lands) |
| `node::util` | The `util` module |
| `node::assert` | The `assert` module (upstream `llrt_assert` is a single `ok`) |
| `node::process` | The module form of the host's `process` global |
| `node::timers` | The module form of the host's timers, plus `timers/promises` |
| `node::path` | The `path` module, moved out of `ferridriver-script`'s `node_compat` |
| `node::bytes` | The one JS-value-to-`Vec<u8>` walk: `BufferSource`, `Buffer`, byte arrays, encoded strings. `crypto`, the compression streams, `Buffer.from` and `setInputFiles` all read through it — there were three separate walks before |

`src/node/` carries its own `rustfmt.toml` re-enabling formatting (the crate
disables it for the vendored subtree) and follows the repo's house style. It
is compiled under this crate's relaxed lints because pedantic's
`needless_pass_by_value` cannot be satisfied by an rquickjs callback, which
must take owned JS values.

## `src/web/` — ferridriver-authored

Web-platform globals llrt has no upstream for: `atob` / `btoa` (the WHATWG
forgiving-base64 algorithm, which `base64::STANDARD` does not implement),
`structuredClone`, and `performance.now()` / `performance.timeOrigin` over a
monotonic base. `llrt_buffer`'s module form reads `atob` / `btoa` off the
globals, so installing them here is what makes `require('buffer').atob`
resolve. Same formatting rules as `src/node/`.

## Keeping it re-syncable

Sources are kept byte-close to upstream, including upstream's 4-space
formatting, so a re-sync against a newer llrt stays a mechanical diff. The
crate therefore does **not** inherit the workspace lints (see its
`Cargo.toml`), and `cargo fmt` must not be pointed at it.

Re-sync recipe (from a checkout of llrt):

```sh
for m in utils:libs/llrt_utils context:libs/llrt_context \
         encoding:libs/llrt_encoding exceptions:modules/llrt_exceptions \
         events:modules/llrt_events abort:modules/llrt_abort \
         os:modules/llrt_os buffer:modules/llrt_buffer \
         json:libs/llrt_json crypto:modules/llrt_crypto \
         url:modules/llrt_url \
         stream_web:modules/llrt_stream_web; do
  name="${m%%:*}"; path="${m##*:}"
  cp -R "$LLRT/$path/src" "src/$name" && mv "src/$name/lib.rs" "src/$name/mod.rs"
done
# `text` takes only llrt_util's four codec files; its own mod.rs stays.
for f in text_encoder text_decoder text_encoder_stream text_decoder_stream; do
  cp "$LLRT/modules/llrt_util/src/$f.rs" "src/text/$f.rs"
done
# per-module first, then the cross-crate rewrite (BSD sed has no \b — use perl)
for name in utils context encoding exceptions events abort os buffer json crypto url text stream_web; do
  find "src/$name" -name '*.rs' | while read -r f; do
    perl -pi -e "s/\bcrate::/crate::${name}::/g" "$f"
  done
done
find src -name '*.rs' | while read -r f; do
  perl -pi -e 's/\bllrt_([a-z_]+)/crate::$1/g' "$f"
done
```

Then re-apply the local deltas below.

## Local deltas

Everything here is a fix or a visibility widening, never a behaviour change
for ferridriver's convenience. Upstream candidates.

0. **Upstream regressions we do NOT take.** Still true at the 2026-08-16 main sync:
   upstream still ships the two transform-stream bugs listed in deltas 2
   and 3 below — and has since changed
   `transform_stream_error_writable_and_unblock_write` to take `_e` and
   ignore it, moving further from the spec. A future re-sync must keep
   OUR versions of `stream_web/transform/{controller,stream}.rs`,
   `stream_web/writable/mod.rs` and the visibility widenings; taking
   upstream wholesale reintroduces a hung `read()` and a `JS_FreeRuntime`
   assertion at teardown.

1. **`abort/abort_signal.rs`** — the `sleep-tokio` arm imports
   `CtxExtension` from `llrt_utils::ctx`, where it does not exist; it lives
   in `llrt_context`. Repointed at `crate::context`. (Upstream only builds
   the default `sleep-timers` arm, which is why this never surfaced there.)

2. **`stream_web/transform/controller.rs`** —
   `TransformStreamDefaultControllerPerformTransform` was missing spec step
   3: reacting to the transform promise's rejection by erroring the stream.
   A `transform()` that threw left both sides live, so a pending
   `reader.read()` never settled.

3. **`stream_web/transform/controller.rs`** —
   `TransformStreamErrorWritableAndUnblockWrite` was missing
   `WritableStreamDefaultControllerErrorIfNeeded`, so an errored transform
   left its writable in the `"writable"` state with an unresolved write
   request. Added `stream_web::writable::writable_stream_error_if_needed`
   for it.

4. **Visibility widenings only** — `SizeAlgorithm` / `SizeValue` /
   `SizeFunction` / `NativeSizeFunction` from `pub(super)` to `pub(crate)`,
   and `writable_stream_default_controller_error` to `pub(crate)`. Upstream
   these were crate-visible because each module was its own crate; nesting
   them under one crate narrowed them below what their own public API needs.

5. **Feature gates** — `sleep-tokio` is on by default. `sleep-timers` is
   deliberately *not* a Cargo feature (the timers module is not vendored, so
   `--all-features` would otherwise enable an arm that cannot compile); it is
   declared to rustc as a known-but-never-set cfg via `check-cfg` in
   `Cargo.toml`, which keeps the upstream `cfg` arms compiling out silently.

7. **`rquickjs` `half` feature** — enabled because the synced
   `utils/bytes.rs` and `stream_web/readable/byob_reader.rs` handle
   `Float16Array`. Without it `PredefinedAtom::Float16Array` does not
   exist and `f16` has no `TypedArrayItem` impl.

6. **Tests** — `abort::abort_signal::tests::test_abort_signal` is no longer
   gated on `sleep-timers`, so it covers the `sleep-tokio` path we build.
   Two regression tests were added to `stream_web/transform/tests.rs` for
   deltas 2 and 3.

8. **`os/mod.rs` — no Windows arm.** `llrt_os`'s `windows.rs` is not
   vendored: ferridriver targets macOS and Linux, and that arm needs four
   Windows-only dependencies (`whoami`, `windows-registry`,
   `windows-result`, `windows-version`).

9. **`os/unix.rs` — `getpwuid_r` instead of the `users` crate.** Upstream
   reads the login name and shell through `users` 0.11, which has been
   unmaintained since 2021. The replacement calls `getpwuid_r` directly —
   the same call that crate makes — including its ERANGE grow-the-buffer
   protocol.

10. **`os/statistics.rs` — real CPU times.** Upstream returns
    `times: { user: 0, nice: 0, sys: 0, idle: 0, irq: 0 }` for every CPU,
    with the comment "cannot be obtained at this time". sysinfo does not
    expose them, but the kernel does: `/proc/stat` on Linux and
    `host_processor_info` on macOS, which is where libuv reads them for
    Node. Ticks are converted to milliseconds through `_SC_CLK_TCK`.
    Darwin does not account interrupt time, so `irq` stays 0 there — as
    it does in libuv.

11. **`os/mod.rs` — `fill()` / `os_object()` split.** Upstream fills the
    module's default export inline inside `evaluate`. ferridriver serves
    every native module twice, as an ES module and as a synchronous
    `require()` namespace, and its loader requires both to read from one
    place, so the body moved into a function.

12. **`os` feature gates.** Upstream's `system` / `statistics` / `network`
    features are declared here too (all on by default) so the `#[cfg]`
    arms stay exactly as upstream wrote them.

## Known gaps against Node

- `os.constants` (signal, errno, priority and dlopen tables) is not
  implemented upstream and is not added here.
- `networkInterfaces()` marks link-local and multicast addresses
  `internal: true`; Node marks only loopback interfaces internal.

13. **`buffer/` — no `Blob`, no `File`.** `llrt_buffer`'s `blob.rs` and
    `file.rs` are not vendored and `init` does not define those classes:
    ferridriver has its own `Blob` and `File` in `ferridriver-script`, and
    a second implementation of each is what this crate exists to avoid.
    `llrt_stream_web` is therefore not a dependency of this module either.

14. **`buffer/class.rs`** is upstream's `buffer.rs`, renamed. A `buffer`
    module inside a `buffer` module trips `clippy::module_inception`,
    which is on by default and the repo's gate runs `-D warnings`.

15. **`buffer/mod.rs` — `equals` and `toJSON`.** Node defines both on
    `Buffer.prototype`; upstream defines neither, and the hand-written
    class this vendoring replaced had both, so not adding them would be a
    regression. Added after `set_prototype` rather than inside the
    vendored file, so `class.rs` stays a mechanical diff. Upstream
    candidates.

16. **`llrt_encoding`'s build script is not vendored.** It only calls
    `llrt_build::set_nightly_cfg()`; this repo pins stable, so the
    `rust_nightly` arms compile out. `rust_nightly` and `nightly` are
    declared as known-but-never-set cfgs in `Cargo.toml`.

## Known gaps against Node — `Buffer`

`Buffer` is a real `Uint8Array` subclass, so every typed-array method
works and index access reads bytes. Missing against Node: the
string-aware overrides of `includes` / `indexOf` / `lastIndexOf` / `fill`
(the `Uint8Array` versions are inherited, so they take byte values, not
strings), `swap16` / `swap32` / `swap64`, `compare`, and `Buffer.poolSize`.

17. **`crypto/provider/{ring,openssl,graviola}.rs` are not vendored.**
    Only the pure-Rust provider (`crypto-rust`, upstream's own default) is
    taken; the other three back-ends would each add a system dependency.
    Their feature names are declared as known-but-unset cfgs.

18. **`crypto` / `json` macro imports.** `iterable_enum` and `str_enum` are
    `#[macro_export]`ed, so they live at the crate root rather than under
    `utils` — the import lines are repointed at `crate::`.

19. **Hash crates keep their `oid` feature.** `sha1` / `sha2` / `md-5` are
    taken with `oid` (and `aes-gcm` with `hazmat`): PKCS#1 v1.5 signing
    needs `AssociatedOid`, and WebCrypto allows 32- and 64-bit GCM tags,
    which are gated behind those features in the 0.11 releases.

20. **`url/url_search_params.rs` — a non-string, non-object init.**
    Upstream ignores it, so `new URLSearchParams(null)` and
    `new URLSearchParams(42)` both build an EMPTY query. WebIDL's init
    union is not nullable, so anything that is neither a sequence nor a
    record converts to USVString: the queries are `null` and `42`, which
    is what every browser engine produces. Only `undefined` (the argument
    omitted) means empty.

21. **`url/url_class.rs` — `urlToHttpOptions` matches Node's shape.**
    Upstream reports `port` as a STRING, omits `search` / `hash` when
    they are empty, keeps the brackets on an IPv6 `hostname`, and joins
    the raw percent-encoded credentials into `auth`. Node reports a
    numeric port, always sets `search` and `hash`, hands over a bare IPv6
    host (what a socket connect takes) and `decodeURIComponent`s the
    credentials. `URL::inner_url` was upstream's only reader for the
    omitted-hash branch and goes with it; the `percent-encoding` dep is
    for the credential decode (`url` keeps that crate private).

# ferridriver-jsstd

Vendored subset of [awslabs/llrt](https://github.com/awslabs/llrt) (Apache
License 2.0), providing the WHATWG Streams implementation and the pieces it
depends on for the ferridriver QuickJS runtime.

Upstream: `0.8.1-beta`, re-synced against `awslabs/llrt@46d4215` (2026-08-04).

| upstream crate     | module here  |
| ------------------ | ------------ |
| `llrt_utils`       | `utils`      |
| `llrt_context`     | `context`    |
| `llrt_exceptions`  | `exceptions` |
| `llrt_events`      | `events`     |
| `llrt_abort`       | `abort`      |
| `llrt_stream_web`  | `stream_web` |
| `llrt_test`        | `test` (dev) |

The rest of llrt — its hyper/fetch stack, timers, buffer, url, console — is
deliberately not vendored: ferridriver has its own, over `reqwest`.

## Keeping it re-syncable

Sources are kept byte-close to upstream, including upstream's 4-space
formatting, so a re-sync against a newer llrt stays a mechanical diff. The
crate therefore does **not** inherit the workspace lints (see its
`Cargo.toml`), and `cargo fmt` must not be pointed at it.

Re-sync recipe (from a checkout of llrt):

```sh
for m in utils:libs/llrt_utils context:libs/llrt_context \
         exceptions:modules/llrt_exceptions events:modules/llrt_events \
         abort:modules/llrt_abort stream_web:modules/llrt_stream_web; do
  name="${m%%:*}"; path="${m##*:}"
  cp -R "$LLRT/$path/src" "src/$name" && mv "src/$name/lib.rs" "src/$name/mod.rs"
done
# per-module first, then the cross-crate rewrite (BSD sed has no \b — use perl)
for name in utils context exceptions events abort stream_web; do
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

0. **Upstream regressions we do NOT take.** As of the 2026-08-04 sync,
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

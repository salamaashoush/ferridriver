# `ferridriver-test`

```
ferridriver-test <command> [FLAGS] [FILES...]
```

Four subcommands, all sharing a common set of runner flags.

| Command | Purpose |
|---|---|
| `test` | Run `.spec.ts` / `.test.ts` / `.feature` files (E2E + BDD, auto-detected) |
| `ct` (alias `component`) | Run component tests against a Vite dev server |
| `codegen URL` | Record interactions; emit Rust / TypeScript / Gherkin |
| `install [BROWSER]` | Download Chromium (add `--with-deps` for system libs) |

## Shared runner flags

```
-j, --workers <N>           parallel workers
    --retries <N>           retry failed tests
    --timeout <MS>          per-test timeout
    --headed                visible browser window
-g, --grep <RE>             filter test names
    --grep-invert <RE>      exclude test names
    --shard <CUR/TOTAL>     CI shard selection
    --tag <NAME>            filter by tag annotation
    --backend <B>           cdp-pipe | cdp-raw | webkit | bidi
    --browser <B>           chromium | firefox | webkit (sets default backend)
    --reporter <R>          terminal | junit | json
    --video <M>             off | on | retain-on-failure
    --trace <M>             off | on | retain-on-failure | on-first-retry
    --update-snapshots      refresh stored snapshots
    --list                  list discovered tests without running
    --forbid-only           fail if any test.only() is present (CI safety)
    --last-failed           re-run only previously failed tests
    --storage-state <PATH>  pre-authenticated storage state
-w, --watch                 re-run on file changes
-v, --verbose               debug-level logging
    --debug <CATS>          cdp,steps,action,worker,fixture
    --output <DIR>          report + artifact directory
    --profile <NAME>        config profile override
    --web-server-dir <DIR>  serve static dir (sets base_url)
    --web-server-cmd <CMD>  run dev server before tests
    --web-server-url <URL>  URL to wait for with --web-server-cmd
-c, --config <PATH>         config file path
```

## `test`-only flags

```
    --steps <GLOB>          BDD step definition file glob (append)
-t, --tags "<EXPR>"          BDD tag expression (@smoke and not @wip)
    --strict                 treat undefined/pending BDD steps as errors
    --order <O>              BDD scenario order: defined | random[:SEED]
    --language <LANG>        Gherkin keyword language (fr, de, ...)
```

## `ct`-only flags

```
    --framework <F>          react (default) | vue | svelte | solid
    --register-source <PATH> custom adapter source path
```

## `codegen`

```
ferridriver-test codegen <URL> [FLAGS]

-l, --language <L>           rust (default) | typescript | bdd
-o, --output <FILE>          write to file instead of stdout
    --viewport <WxH>         viewport size (e.g. 1280x720)
```

## `install`

```
ferridriver-test install [BROWSER] [FLAGS]
    --with-deps              also install system dependencies (fonts, libs)
```

## Examples

```bash
ferridriver-test test                                    # all tests in cwd
ferridriver-test test --headed -j 4
ferridriver-test test tests/smoke.spec.ts                # specific file
ferridriver-test test tests/features/*.feature -t "@smoke"
ferridriver-test test tests/ --steps 'steps/**/*.ts'     # mixed E2E + BDD
ferridriver-test ct --framework react                    # React component tests
ferridriver-test codegen https://example.com             # record as Rust
ferridriver-test install --with-deps                     # Chromium + system deps
```

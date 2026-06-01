# Running BDD

Features run through one of two paths, both driven by the same
`TestRunner`.

## `ferridriver bdd` (the CLI)

Primary path. Runs Gherkin features through the core test runner with
Rust and / or JS / TS step bodies — no Node, no Bun:

```bash
# Rust steps only
ferridriver bdd tests/features/

# With JavaScript / TypeScript step files
ferridriver bdd \
  --steps 'steps/**/*.{js,ts}' \
  --tags '@smoke and not @wip' \
  --workers 4 \
  --reporter junit \
  --reporter terminal \
  tests/features/
```

`--steps` is repeatable and overrides `[test].steps` from the config
file. Defaults to `steps/**/*.{js,ts}` and
`step_definitions/**/*.{js,ts}` when omitted.

### Flags

```
--steps GLOB              JS / TS step files (repeatable)
--tags EXPR               tag filter: @smoke and not @wip, etc.
--workers N               parallel workers (default: CPU count)
--reporter NAME           reporter name (repeatable)
--strict                  treat undefined / pending steps as failures
--dry-run                 parse + report scenarios without executing
--fail-fast               stop after first failure
--step-timeout MS         override per-step timeout
--order MODE              defined | random | random:SEED
--language LANG           default Gherkin keyword language (en, de, fr, ...)
--shard X/N               run shard X of N across CI machines
--world-parameters JSON   pass JSON object to this.parameters
```

Plus the shared browser flags (`--backend`, `--headless`,
`--executable-path`, `--connect`, `--auto-connect`, `--user-data-dir`).

## Rust / `cargo test`

Use `bdd_main!()` as the binary entry point and the shared runner flags:

```rust
// tests/bdd.rs
ferridriver_bdd::bdd_main!();
```

```toml
# Cargo.toml
[[test]]
name = "bdd"
path = "tests/bdd.rs"
harness = false

[dev-dependencies]
ferridriver-bdd  = "0.4"
ferridriver-test = "0.4"
```

```bash
cargo test --test bdd -- -j 4 --tags '@smoke and not @wip'
```

Tests run headed by default; pass `--headless` to opt into headless mode.

## Reporters

| Reporter         | Output |
|------------------|--------|
| `terminal`       | Gherkin-formatted hierarchy with colors (default) |
| `progress`       | Compact dot-based status |
| `dot`            | One character per scenario |
| `json`           | Machine-readable JSON |
| `junit`          | JUnit XML (CI-friendly) |
| `html`           | Self-contained HTML report |
| `cucumber-json`  | Cucumber JSON format (dashboards) |
| `messages` / `ndjson` | Cucumber Messages NDJSON stream |
| `usage`          | Step expression call counts + duration |
| `rerun`          | Failed scenario `file:line` for re-execution |
| `github`         | GitHub Actions annotations |
| `empty`          | No output |

Specify multiple reporters by repeating `--reporter`, e.g. `--reporter
terminal --reporter junit`. Each name is matched exactly (no `name:path`
syntax). File-writing reporters emit into the run's output directory —
`junit` writes `junit.xml`, `json` writes `results.json`. Set the path or
per-reporter options with a `[[test.reporter]]` table in the config file.

## Environment variables

The shared runner env vars apply (see [CLI reference](/cli/ferridriver) —
`FERRIDRIVER_DEBUG`, `FERRIDRIVER_PROFILE`, `RUST_LOG`, etc.).

# Running BDD

There is no standalone `ferridriver bdd` command. Features run through one of three paths:

## Rust / `cargo test`

Use `bdd_main!()` and the shared runner flags:

```bash
cargo test --test bdd -- --headed -j 4 --tags "@smoke and not @wip"
```

Register the test binary so `harness = false` lets `bdd_main!()` own the entry point:

```toml
# Cargo.toml
[[test]]
name = "bdd"
path = "tests/bdd.rs"
harness = false

[dev-dependencies]
ferridriver-bdd = "0.1"
ferridriver-test = "0.1"
```

## TypeScript / `@ferridriver/test`

Mixed `.feature` + `.spec.ts` runs in one invocation with shared config:

```bash
npx @ferridriver/test test tests/features/ \
  --steps 'steps/**/*.ts' \
  -t "@smoke and not @wip" \
  -j 4 --reporter junit --output reports/
```

BDD-only flags on the `test` subcommand: `--steps <GLOB>` (append), `-t, --tags "<EXPR>"`, `--strict`, `--order defined|random[:SEED]`, `--language <LANG>` (Gherkin keyword language).

## Environment variables

- `FERRIDRIVER_FEATURES` — comma-separated glob patterns (default: `features/**/*.feature`)
- `FERRIDRIVER_TAGS` — tag filter expression

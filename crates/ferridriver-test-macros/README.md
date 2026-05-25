# ferridriver-test-macros

[![crates.io](https://img.shields.io/crates/v/ferridriver-test-macros.svg?logo=rust&color=c97b4a)](https://crates.io/crates/ferridriver-test-macros)
[![docs.rs](https://img.shields.io/docsrs/ferridriver-test-macros?logo=docs.rs&color=c97b4a)](https://docs.rs/ferridriver-test-macros)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-c97b4a.svg)](https://github.com/salamaashoush/ferridriver)

Procedural macros for `ferridriver-test`. Re-exported through
`ferridriver_test::prelude` — depend on this crate directly only when you
need the macros without the runner.

| Macro                    | Purpose |
|--------------------------|---------|
| `#[ferritest]`           | Mark an async function as a ferridriver test. Optional attributes: `retries`, `timeout`, `tag`, `skip`, `slow`, `fixme`, `fail`, `only`, `info`, `use_options`. |
| `#[ferritest_each(data = [...])]` | Parameterized test. Generates one test per row of `data`. |
| `#[before_all]` / `#[after_all]` | Per-suite per-worker hook. |
| `#[before_each]` / `#[after_each]` | Per-test hook. `#[after_each]` runs even on failure. |
| `#[fixture(scope = "test"|"worker"|"global")]` | Declare a custom fixture. |
| `ferridriver_test::main!()` | Generate the `fn main()` harness for a test binary (used in `tests/harness.rs`). |

See `ferridriver-test` for the full attribute syntax, condition grammar,
and lifecycle.

## License

MIT OR Apache-2.0

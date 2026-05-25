# ferridriver-bdd-macros

[![crates.io](https://img.shields.io/crates/v/ferridriver-bdd-macros.svg?logo=rust&color=c97b4a)](https://crates.io/crates/ferridriver-bdd-macros)
[![docs.rs](https://img.shields.io/docsrs/ferridriver-bdd-macros?logo=docs.rs&color=c97b4a)](https://docs.rs/ferridriver-bdd-macros)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-c97b4a.svg)](https://github.com/salamaashoush/ferridriver)

Procedural macros for `ferridriver-bdd`. Re-exported through
`ferridriver_bdd::prelude` — depend on this crate directly only when you
need the macros without the runner.

| Macro                                  | Purpose |
|----------------------------------------|---------|
| `#[given(EXPR)]` / `#[when(EXPR)]` / `#[then(EXPR)]` | Register a Cucumber-expression step. |
| `#[step(EXPR)]`                        | Keyword-agnostic step (matches Given / When / Then / And / But). |
| `#[given(regex = PATTERN)]` etc.       | Register a raw-regex step instead of a Cucumber expression. |
| `#[before(scenario)]` / `#[after(scenario)]` | Per-scenario hook. Optional `tags = "@expr"`, `order = N`. |
| `#[before(all)]` / `#[after(all)]`     | Global lifecycle hook. |
| `#[before(feature)]` / `#[after(feature)]` | Per-feature hook. |
| `#[before(step)]` / `#[after(step)]`   | Per-step hook. |
| `#[param_type(name = "color", regex = "red|green|blue")]` | Register a custom Cucumber expression parameter type. |

Step handler signature:

```rust
#[given("I have {int} items in my {string}")]
async fn step(
    world: &mut BrowserWorld,
    count: i64,
    bucket: String,
    // optional, recognized by name:
    table: Option<&DataTable>,
    docstring: Option<&str>,
) -> Result<(), StepError> { Ok(()) }
```

Parameter extraction is type-directed:
- `String` → `{string}`
- `i64` → `{int}`
- `f64` → `{float}`
- Custom `{name}` → registered regex (extract as `String`).

`table` / `data_table` and `docstring` / `doc_string` are optional and
recognized by parameter name; they receive `None` when absent in the
step.

See `ferridriver-bdd` for the registry, world, and execution model.

## License

MIT OR Apache-2.0

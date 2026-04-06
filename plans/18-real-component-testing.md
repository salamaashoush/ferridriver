# Feature: Real Per-Component Testing for WASM Frameworks (Leptos/Dioxus)

## Context
Current CT implementation is broken — it builds the entire app with `trunk build`/`dx build` and tests it as a whole. This is E2E testing, NOT component testing. Real CT means mounting a single component in isolation with controlled props, testing it, unmounting, and repeating.

## Problem
WASM is compiled once — you can't dynamically load components at runtime like JS does with `import()`. The entire binary must contain all components and expose a mount API that JS can call.

## Design

### Architecture
Embed a **component registry** in the WASM binary that JS controls:

1. **WASM exports `ferri_mount(id, props_json, target_id)`** — looks up component by name, deserializes props, mounts to DOM element
2. **Component registration** — users register components (manual initially, auto-generated later via build script)
3. **Shell HTML page** — empty `<div id="root">`, loads WASM, exposes `window.__ferriMount`
4. **Test runner** — navigates to shell, calls `window.__ferriMount("Counter", {initial: 5})` per test

### User API
```rust
#[component_test(component = Counter, props(initial = 5))]
async fn counter_starts_at_five(page: Page) -> Result<(), TestFailure> {
    expect(&page.locator("#count")).to_have_text("5").await?;
    page.locator("#inc").click().await?;
    expect(&page.locator("#count")).to_have_text("6").await?;
    Ok(())
}
```

### Component Registry (Leptos)
```rust
// In user's lib.rs
#[cfg(feature = "ct")]
pub fn register_ct_components() {
    register_component("Counter", Box::new(|props_json| {
        let props: serde_json::Value = serde_json::from_str(props_json)?;
        let initial = props.get("initial").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        let el = document().get_element_by_id("root").unwrap();
        mount_to(&el, move || view! { <Counter initial=initial /> });
        Ok(())
    }));
}
```

### Build Flow
- `trunk build --features ct` → produces WASM with component registry (NOT full app)
- Shell page loads WASM, exposes `window.__ferriMount`
- Each test: mount component → interact → assert → unmount → next test

### Phases
1. **Manual registration** — users write `register_component()` calls
2. **Auto-registry via build script** — scan `#[component_test]` for component refs, generate registration
3. **Props via macro** — `#[component_test(component = Counter, props(initial = 5))]`

### Key Constraints
- All components must be compiled into one binary (no dynamic loading)
- Props must be JSON-serializable (serde)
- Leptos `mount_to()` requires `'static` closures — use thread-local registry
- Unmount/cleanup between tests needs fresh signals (Leptos doesn't fully dispose)

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver-ct-leptos/src/lib.rs` | Modify — mount protocol, shell generation |
| `crates/ferridriver-ct-leptos-macros/src/lib.rs` | Modify — component ID + props in macro |
| `crates/ferridriver-ct-dioxus/src/lib.rs` | Same for Dioxus |
| `crates/ferridriver-test/src/ct/mod.rs` | Modify — WASM mount bridge |
| `examples/ct-leptos/src/lib.rs` | Modify — register components |
| New: shell HTML template | Create — empty page with mount handler |

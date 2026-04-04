//! Component testing: mount and test individual UI components in a real browser.
//!
//! Two paths share the same `ComponentServer`:
//!
//! **WASM path** (Rust frameworks: Leptos, Dioxus, Yew, Sycamore):
//! - Compile component to WASM via `cargo + wasm-bindgen`
//! - ComponentServer serves the WASM bundle + HTML wrapper
//! - Browser loads WASM, component mounts, tests interact
//!
//! **Vite path** (JS frameworks: React, Vue, Svelte, Solid):
//! - Vite dev server bundles the component
//! - ComponentServer proxies or Vite serves directly
//! - Browser loads JS bundle, component mounts, tests interact
//!
//! ```ignore
//! // Rust (Leptos)
//! #[component_test(leptos)]
//! async fn counter_increments(page: Page) {
//!     mount!(Counter, initial = 5);
//!     page.locator("button").click().await?;
//!     expect(&page.locator("span")).to_have_text("6").await?;
//! }
//!
//! // TypeScript (React)
//! test.component('counter increments', async ({ mount, page }) => {
//!     await mount(Counter, { props: { initial: 5 } });
//!     await page.locator('button').click();
//!     await expect(page.locator('span')).toHaveText('6');
//! });
//! ```

pub mod server;
pub mod vite;
pub mod wasm;

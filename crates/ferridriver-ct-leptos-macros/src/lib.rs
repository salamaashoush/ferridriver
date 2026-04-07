//! Proc macros for Leptos component testing.
//!
//! `#[component_test]` registers a test function with the custom harness.
//!
//! Supports both styles:
//! ```ignore
//! // Style 1: Return Result (idiomatic, uses ?)
//! #[component_test]
//! async fn my_test(page: Page) -> Result<(), TestFailure> {
//!     page.locator("#btn").click().await?;
//!     expect(&page.locator("#count")).to_have_text("1").await?;
//!     Ok(())
//! }
//!
//! // Style 2: No return type (unwrap/assert, panics on failure)
//! #[component_test]
//! async fn my_test(page: Page) {
//!     page.locator("#btn").click().await.unwrap();
//!     assert_eq!(count, 1);
//! }
//! ```

use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, ReturnType, parse_macro_input};

#[proc_macro_attribute]
pub fn component_test(_attr: TokenStream, item: TokenStream) -> TokenStream {
  let input = parse_macro_input!(item as ItemFn);
  let fn_name = &input.sig.ident;
  let fn_name_str = fn_name.to_string();
  let body = &input.block;
  let vis = &input.vis;

  // Check if the user provided an explicit return type.
  // If they wrote `-> Result<(), TestFailure>`, use the body as-is.
  // If no return type (unit), wrap with Ok(()) at the end.
  let has_return_type = !matches!(&input.sig.output, ReturnType::Default);

  let fn_body = if has_return_type {
    // User handles Result themselves.
    quote! { #body }
  } else {
    // Wrap: run body, return Ok(()).
    quote! {
      {
        let __body = async { #body };
        __body.await;
        Ok(())
      }
    }
  };

  let expanded = quote! {
    #vis async fn #fn_name(page: ferridriver::Page) -> Result<(), ferridriver_test::model::TestFailure> {
      #fn_body
    }

    ferridriver_test::inventory::submit! {
      ferridriver_ct_leptos::ComponentTestRegistration {
        name: #fn_name_str,
        test_fn: |page| Box::pin(#fn_name(page)),
      }
    }
  };

  expanded.into()
}

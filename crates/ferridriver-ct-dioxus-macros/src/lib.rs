//! Proc macros for Dioxus component testing.
//!
//! Same pattern as the Leptos adapter: `#[component_test]` registers via
//! inventory, custom harness runs all tests on shared browsers.

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

  let has_return_type = !matches!(&input.sig.output, ReturnType::Default);

  let fn_body = if has_return_type {
    quote! { #body }
  } else {
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
      ferridriver_ct_dioxus::ComponentTestRegistration {
        name: #fn_name_str,
        test_fn: |page| Box::pin(#fn_name(page)),
      }
    }
  };

  expanded.into()
}

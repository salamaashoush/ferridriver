//! Proc macros for the ferridriver test framework.
//!
//! Provides `#[ferritest]` to register async browser test functions
//! with automatic fixture injection based on parameter types.
//!
//! ```ignore
//! use ferridriver_test::prelude::*;
//!
//! #[ferritest]
//! async fn basic_navigation(page: Page) {
//!     page.goto("https://example.com", None).await.unwrap();
//!     expect(&page).to_have_title("Example").await.unwrap();
//! }
//!
//! #[ferritest(retries = 2, timeout = "30s", tag = "smoke")]
//! async fn flaky_test(page: Page, context: BrowserContext) {
//!     // ...
//! }
//! ```

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{parse_macro_input, FnArg, ItemFn, Lit, Meta, Pat, Token, Type};

/// Attribute arguments: `#[ferritest(retries = 2, timeout = "30s", tag = "smoke")]`
struct FerritestArgs {
  retries: Option<u32>,
  timeout_ms: Option<u64>,
  tags: Vec<String>,
  skip: bool,
  slow: bool,
  fixme: bool,
}

impl Parse for FerritestArgs {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    let mut args = Self {
      retries: None,
      timeout_ms: None,
      tags: Vec::new(),
      skip: false,
      slow: false,
      fixme: false,
    };

    let metas = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
    for meta in metas {
      match &meta {
        Meta::NameValue(nv) => {
          let ident = nv.path.get_ident().map(ToString::to_string).unwrap_or_default();
          match ident.as_str() {
            "retries" => {
              if let syn::Expr::Lit(lit) = &nv.value {
                if let Lit::Int(i) = &lit.lit {
                  args.retries = Some(i.base10_parse()?);
                }
              }
            }
            "timeout" => {
              if let syn::Expr::Lit(lit) = &nv.value {
                if let Lit::Str(s) = &lit.lit {
                  args.timeout_ms = Some(parse_duration_str(&s.value())?);
                }
              }
            }
            "tag" => {
              if let syn::Expr::Lit(lit) = &nv.value {
                if let Lit::Str(s) = &lit.lit {
                  args.tags.push(s.value());
                }
              }
            }
            _ => return Err(syn::Error::new_spanned(&nv.path, format!("unknown ferritest attribute: {ident}"))),
          }
        }
        Meta::Path(p) => {
          let ident = p.get_ident().map(ToString::to_string).unwrap_or_default();
          match ident.as_str() {
            "skip" => args.skip = true,
            "slow" => args.slow = true,
            "fixme" => args.fixme = true,
            _ => return Err(syn::Error::new_spanned(p, format!("unknown ferritest flag: {ident}"))),
          }
        }
        Meta::List(_) => {
          return Err(syn::Error::new_spanned(&meta, "unexpected nested attribute"));
        }
      }
    }
    Ok(args)
  }
}

fn parse_duration_str(s: &str) -> syn::Result<u64> {
  let s = s.trim();
  if let Some(secs) = s.strip_suffix('s') {
    secs
      .trim()
      .parse::<u64>()
      .map(|v| v * 1000)
      .map_err(|e| syn::Error::new(proc_macro2::Span::call_site(), format!("invalid timeout: {e}")))
  } else if let Some(ms) = s.strip_suffix("ms") {
    ms.trim()
      .parse::<u64>()
      .map_err(|e| syn::Error::new(proc_macro2::Span::call_site(), format!("invalid timeout: {e}")))
  } else {
    s.parse::<u64>()
      .map_err(|e| syn::Error::new(proc_macro2::Span::call_site(), format!("invalid timeout (use '30s' or '5000ms'): {e}")))
  }
}

/// Extract fixture name from a type path like `Page`, `Browser`, `BrowserContext`.
fn fixture_name_from_type(ty: &Type) -> Option<String> {
  if let Type::Path(tp) = ty {
    let seg = tp.path.segments.last()?;
    let name = seg.ident.to_string();
    Some(match name.as_str() {
      "Page" => "page".to_string(),
      "Browser" => "browser".to_string(),
      "BrowserContext" | "ContextRef" => "context".to_string(),
      other => other.to_lowercase(),
    })
  } else {
    None
  }
}

/// `#[ferritest]` attribute macro.
///
/// Transforms an async function into a registered test case with automatic
/// fixture injection based on parameter types.
#[proc_macro_attribute]
pub fn ferritest(attr: TokenStream, item: TokenStream) -> TokenStream {
  let args = parse_macro_input!(attr as FerritestArgs);
  let input = parse_macro_input!(item as ItemFn);

  let fn_name = &input.sig.ident;
  let fn_name_str = fn_name.to_string();
  let vis = &input.vis;
  let block = &input.block;
  let attrs = &input.attrs;

  // Parse parameters to determine fixture requests.
  let mut fixture_names: Vec<String> = Vec::new();
  let mut param_bindings = Vec::new();

  for arg in &input.sig.inputs {
    if let FnArg::Typed(pat_type) = arg {
      if let Pat::Ident(pat_ident) = pat_type.pat.as_ref() {
        let param_name = &pat_ident.ident;
        let param_type = &pat_type.ty;
        let fixture = fixture_name_from_type(param_type).unwrap_or_else(|| param_name.to_string());
        fixture_names.push(fixture.clone());
        param_bindings.push(quote! {
          let #param_name: #param_type = __pool.get::<#param_type>(#fixture).await
            .map_err(|e| ferridriver_test::model::TestFailure {
              message: format!("fixture '{}' failed: {}", #fixture, e),
              stack: None,
              diff: None,
              screenshot: None,
            })?;
        });
      }
    }
  }

  let fixture_array = fixture_names.iter().map(|f| quote! { #f });

  // Build annotations.
  let mut annotations = Vec::new();
  if args.skip {
    annotations.push(quote! { ferridriver_test::model::TestAnnotation::Skip { reason: None } });
  }
  if args.slow {
    annotations.push(quote! { ferridriver_test::model::TestAnnotation::Slow });
  }
  if args.fixme {
    annotations.push(quote! { ferridriver_test::model::TestAnnotation::Fixme { reason: None } });
  }
  for tag in &args.tags {
    annotations.push(quote! { ferridriver_test::model::TestAnnotation::Tag(#tag.to_string()) });
  }

  let retries_expr = match args.retries {
    Some(r) => quote! { Some(#r) },
    None => quote! { None },
  };
  let timeout_expr = match args.timeout_ms {
    Some(ms) => quote! { Some(std::time::Duration::from_millis(#ms)) },
    None => quote! { None },
  };

  let expanded = quote! {
    #(#attrs)*
    #vis async fn #fn_name(__pool: ferridriver_test::fixture::FixturePool) -> Result<(), ferridriver_test::model::TestFailure> {
      #(#param_bindings)*
      #block
      Ok(())
    }

    inventory::submit! {
      ferridriver_test::discovery::TestRegistration {
        file: file!(),
        line: line!(),
        name: #fn_name_str,
        suite: None,
        fixture_requests: &[#(#fixture_array),*],
        annotations: &[#(#annotations),*],
        timeout_ms: {
          #timeout_expr.map(|d: std::time::Duration| d.as_millis() as u64)
        },
        retries: #retries_expr,
        test_fn: |pool| Box::pin(#fn_name(pool)),
      }
    }
  };

  expanded.into()
}

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
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{parse_macro_input, Expr, FnArg, ItemFn, Lit, Meta, Pat, Token, Type};

/// Attribute arguments: `#[ferritest(retries = 2, timeout = "30s", tag = "smoke")]`
struct FerritestArgs {
  retries: Option<u32>,
  timeout_ms: Option<u64>,
  tags: Vec<String>,
  skip: bool,
  slow: bool,
  fixme: bool,
  only: bool,
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
      only: false,
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
            "only" => args.only = true,
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
  if args.only {
    annotations.push(quote! { ferridriver_test::model::TestAnnotation::Only });
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

/// Arguments for `#[ferritest_each]`: `data = [(...), (...)]`.
struct FerritestEachArgs {
  data: Vec<Vec<Expr>>,
}

impl Parse for FerritestEachArgs {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    // Parse: data = [(...), (...)]
    let ident: syn::Ident = input.parse()?;
    if ident != "data" {
      return Err(syn::Error::new_spanned(&ident, "expected `data = [...]`"));
    }
    let _: Token![=] = input.parse()?;

    let content;
    syn::bracketed!(content in input);

    let mut data = Vec::new();
    while !content.is_empty() {
      let inner;
      syn::parenthesized!(inner in content);
      let exprs: Punctuated<Expr, Token![,]> = Punctuated::parse_terminated(&inner)?;
      data.push(exprs.into_iter().collect());

      if content.peek(Token![,]) {
        let _: Token![,] = content.parse()?;
      }
    }

    Ok(Self { data })
  }
}

/// `#[ferritest_each(data = [("a", 1), ("b", 2)])]` — parameterized test macro.
///
/// Expands a single async test function into N registered tests, one per data row.
/// Parameters after fixture types (Page, Browser, etc.) receive the data values.
///
/// ```ignore
/// #[ferritest_each(data = [("admin", "admin@example.com"), ("guest", "guest@example.com")])]
/// async fn login(page: Page, role: &str, email: &str) {
///     page.goto(&format!("/login?role={role}"), None).await.unwrap();
/// }
/// ```
/// Registers: `login (admin, admin@example.com)` and `login (guest, guest@example.com)`.
#[proc_macro_attribute]
pub fn ferritest_each(attr: TokenStream, item: TokenStream) -> TokenStream {
  let args = parse_macro_input!(attr as FerritestEachArgs);
  let input = parse_macro_input!(item as ItemFn);

  let fn_name = &input.sig.ident;
  let fn_name_str = fn_name.to_string();
  let block = &input.block;
  let attrs = &input.attrs;

  // Split parameters into fixture params and data params.
  let all_params: Vec<_> = input.sig.inputs.iter().collect();
  let mut fixture_names: Vec<String> = Vec::new();
  let mut fixture_bindings = Vec::new();
  let mut data_params: Vec<(&syn::Ident, &Type)> = Vec::new();

  for arg in &all_params {
    if let FnArg::Typed(pat_type) = arg {
      if let Pat::Ident(pat_ident) = pat_type.pat.as_ref() {
        let param_name = &pat_ident.ident;
        let param_type = &*pat_type.ty;
        let fixture = fixture_name_from_type(param_type);
        if let Some(fixture) = fixture {
          fixture_names.push(fixture.clone());
          fixture_bindings.push(quote! {
            let #param_name: #param_type = __pool.get::<#param_type>(#fixture).await
              .map_err(|e| ferridriver_test::model::TestFailure {
                message: format!("fixture '{}' failed: {}", #fixture, e),
                stack: None,
                diff: None,
                screenshot: None,
              })?;
          });
        } else {
          data_params.push((param_name, param_type));
        }
      }
    }
  }

  let fixture_array = fixture_names.iter().map(|f| quote! { #f });

  // Generate one inventory::submit! per data row.
  let mut submissions = Vec::new();
  for (row_idx, row) in args.data.iter().enumerate() {
    if row.len() != data_params.len() {
      return syn::Error::new_spanned(
        &input.sig.ident,
        format!(
          "data row {} has {} values but function expects {} data parameters",
          row_idx,
          row.len(),
          data_params.len()
        ),
      )
      .to_compile_error()
      .into();
    }

    // Build name suffix: "(val1, val2)"
    let row_values_str: Vec<String> = row.iter().map(|e| quote!(#e).to_string().replace('"', "")).collect();
    let suffix = row_values_str.join(", ");
    let test_name = format!("{fn_name_str} ({suffix})");

    // Build let bindings for data params.
    let data_bindings: Vec<_> = data_params
      .iter()
      .zip(row.iter())
      .map(|((param_name, param_type), value)| {
        quote! { let #param_name: #param_type = #value; }
      })
      .collect();

    let inner_fn_name = format_ident!("__ferritest_each_{}_{}", fn_name, row_idx);
    let fixture_array2 = fixture_names.iter().map(|f| quote! { #f });

    submissions.push(quote! {
      async fn #inner_fn_name(__pool: ferridriver_test::fixture::FixturePool) -> Result<(), ferridriver_test::model::TestFailure> {
        #(#fixture_bindings)*
        #(#data_bindings)*
        #block
        Ok(())
      }

      inventory::submit! {
        ferridriver_test::discovery::TestRegistration {
          file: file!(),
          line: line!(),
          name: #test_name,
          suite: None,
          fixture_requests: &[#(#fixture_array2),*],
          annotations: &[],
          timeout_ms: None,
          retries: None,
          test_fn: |pool| Box::pin(#inner_fn_name(pool)),
        }
      }
    });
  }

  let expanded = quote! {
    #(#attrs)*
    #(#submissions)*
  };

  expanded.into()
}

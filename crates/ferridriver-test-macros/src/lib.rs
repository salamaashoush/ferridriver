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
use syn::{Expr, FnArg, ItemFn, Lit, Meta, Pat, Token, Type, parse_macro_input};

/// Attribute arguments: `#[ferritest(retries = 2, timeout = "30s", tag = "smoke")]`
struct FerritestArgs {
  retries: Option<u32>,
  timeout_ms: Option<u64>,
  tags: Vec<String>,
  /// None = not set, Some(None) = unconditional, Some(Some("firefox")) = conditional
  skip: Option<Option<String>>,
  /// None = not set, Some(None) = unconditional, Some(Some("ci")) = conditional
  slow: Option<Option<String>>,
  /// None = not set, Some(None) = unconditional, Some(Some("linux")) = conditional
  fixme: Option<Option<String>>,
  /// None = not set, Some(None) = unconditional, Some(Some("webkit")) = conditional
  fail: Option<Option<String>>,
  only: bool,
  /// Structured metadata annotations: `info = "type:description"`.
  infos: Vec<(String, String)>,
  /// Raw JSON string for fixture/context overrides (viewport, locale, etc.)
  use_options: Option<String>,
}

impl Parse for FerritestArgs {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    let mut args = Self {
      retries: None,
      timeout_ms: None,
      tags: Vec::new(),
      skip: None,
      slow: None,
      fixme: None,
      fail: None,
      only: false,
      infos: Vec::new(),
      use_options: None,
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
            },
            "timeout" => {
              if let syn::Expr::Lit(lit) = &nv.value {
                if let Lit::Str(s) = &lit.lit {
                  args.timeout_ms = Some(parse_duration_str(&s.value())?);
                }
              }
            },
            "tag" => {
              if let syn::Expr::Lit(lit) = &nv.value {
                if let Lit::Str(s) = &lit.lit {
                  args.tags.push(s.value());
                }
              }
            },
            "skip" => {
              if let syn::Expr::Lit(lit) = &nv.value {
                if let Lit::Str(s) = &lit.lit {
                  args.skip = Some(Some(s.value()));
                }
              }
            },
            "slow" => {
              if let syn::Expr::Lit(lit) = &nv.value {
                if let Lit::Str(s) = &lit.lit {
                  args.slow = Some(Some(s.value()));
                }
              }
            },
            "fixme" => {
              if let syn::Expr::Lit(lit) = &nv.value {
                if let Lit::Str(s) = &lit.lit {
                  args.fixme = Some(Some(s.value()));
                }
              }
            },
            "fail" => {
              if let syn::Expr::Lit(lit) = &nv.value {
                if let Lit::Str(s) = &lit.lit {
                  args.fail = Some(Some(s.value()));
                }
              }
            },
            "use_options" => {
              if let syn::Expr::Lit(lit) = &nv.value {
                if let Lit::Str(s) = &lit.lit {
                  args.use_options = Some(s.value());
                }
              }
            },
            "info" => {
              if let syn::Expr::Lit(lit) = &nv.value {
                if let Lit::Str(s) = &lit.lit {
                  let val = s.value();
                  if let Some((type_name, desc)) = val.split_once(':') {
                    args.infos.push((type_name.trim().to_string(), desc.trim().to_string()));
                  } else {
                    args.infos.push((val, String::new()));
                  }
                }
              }
            },
            _ => {
              return Err(syn::Error::new_spanned(
                &nv.path,
                format!("unknown ferritest attribute: {ident}"),
              ));
            },
          }
        },
        Meta::Path(p) => {
          let ident = p.get_ident().map(ToString::to_string).unwrap_or_default();
          match ident.as_str() {
            "skip" => args.skip = Some(None),
            "slow" => args.slow = Some(None),
            "fixme" => args.fixme = Some(None),
            "fail" => args.fail = Some(None),
            "only" => args.only = true,
            _ => return Err(syn::Error::new_spanned(p, format!("unknown ferritest flag: {ident}"))),
          }
        },
        Meta::List(_) => {
          return Err(syn::Error::new_spanned(&meta, "unexpected nested attribute"));
        },
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
    s.parse::<u64>().map_err(|e| {
      syn::Error::new(
        proc_macro2::Span::call_site(),
        format!("invalid timeout (use '30s' or '5000ms'): {e}"),
      )
    })
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

  // The function receives a TestContext. Extract the parameter name the user chose
  // (e.g., `ctx`, `context`, `t`, etc.)
  let ctx_param_name = if let Some(FnArg::Typed(pt)) = input.sig.inputs.first() {
    if let Pat::Ident(pi) = pt.pat.as_ref() {
      pi.ident.clone()
    } else {
      format_ident!("ctx")
    }
  } else {
    format_ident!("ctx")
  };

  // Request all standard fixtures so the worker provisions them.
  let fixture_names: Vec<String> = vec!["browser".into(), "context".into(), "page".into(), "test_info".into()];
  let fixture_array = fixture_names.iter().map(|f| quote! { #f });

  // Build annotations.
  // Helper: parse "condition" or "condition | reason" into (condition, reason) tokens.
  fn annotation_tokens(variant: &str, arg: &Option<Option<String>>, annotations: &mut Vec<proc_macro2::TokenStream>) {
    let variant_ident = quote::format_ident!("{}", variant);
    match arg {
      Some(None) => {
        annotations
          .push(quote! { ferridriver_test::model::TestAnnotation::#variant_ident { reason: None, condition: None } });
      },
      Some(Some(val)) => {
        // Support "condition | reason" format.
        if let Some((cond, reason)) = val.split_once('|') {
          let cond = cond.trim();
          let reason = reason.trim();
          annotations.push(quote! { ferridriver_test::model::TestAnnotation::#variant_ident {
            reason: Some(#reason.to_string()),
            condition: Some(#cond.to_string()),
          } });
        } else {
          annotations.push(quote! { ferridriver_test::model::TestAnnotation::#variant_ident {
            reason: None,
            condition: Some(#val.to_string()),
          } });
        }
      },
      None => {},
    }
  }

  let mut annotations = Vec::new();
  annotation_tokens("Skip", &args.skip, &mut annotations);
  annotation_tokens("Slow", &args.slow, &mut annotations);
  annotation_tokens("Fixme", &args.fixme, &mut annotations);
  annotation_tokens("Fail", &args.fail, &mut annotations);
  if args.only {
    annotations.push(quote! { ferridriver_test::model::TestAnnotation::Only });
  }
  for tag in &args.tags {
    annotations.push(quote! { ferridriver_test::model::TestAnnotation::Tag(#tag.to_string()) });
  }
  for (type_name, desc) in &args.infos {
    annotations.push(
      quote! { ferridriver_test::model::TestAnnotation::Info { type_name: #type_name.to_string(), description: #desc.to_string() } },
    );
  }

  let retries_expr = match args.retries {
    Some(r) => quote! { Some(#r) },
    None => quote! { None },
  };
  let timeout_ms_expr = match args.timeout_ms {
    Some(ms) => quote! { Some(#ms) },
    None => quote! { None },
  };
  let use_options_expr = match &args.use_options {
    Some(json) => quote! { Some(#json) },
    None => quote! { None },
  };

  let expanded = quote! {
    #(#attrs)*
    #vis async fn #fn_name(__pool: ferridriver_test::fixture::FixturePool) -> Result<(), ferridriver_test::model::TestFailure> {
      let #ctx_param_name = ferridriver_test::TestContext::new(__pool);
      #block
      Ok(())
    }

    inventory::submit! {
      ferridriver_test::discovery::TestRegistration {
        file: file!(),
        module_path: module_path!(),
        name: #fn_name_str,
        fixture_requests: &[#(#fixture_array),*],
        annotations: &[#(#annotations),*],
        timeout_ms: #timeout_ms_expr,
        retries: #retries_expr,
        use_options: #use_options_expr,
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
/// First parameter is `FixturePool`, remaining parameters receive the data values.
///
/// ```ignore
/// #[ferritest_each(data = [("admin", "admin@example.com"), ("guest", "guest@example.com")])]
/// async fn login(pool: FixturePool, role: &str, email: &str) {
///     let page = pool.page().await.unwrap();
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

  // First param is TestContext, rest are data params.
  let all_params: Vec<_> = input.sig.inputs.iter().collect();
  let ctx_param_name = if let Some(FnArg::Typed(pt)) = all_params.first() {
    if let Pat::Ident(pi) = pt.pat.as_ref() {
      pi.ident.clone()
    } else {
      format_ident!("ctx")
    }
  } else {
    format_ident!("ctx")
  };

  let data_params: Vec<(&syn::Ident, &Type)> = all_params
    .iter()
    .skip(1) // skip FixturePool
    .filter_map(|arg| {
      if let FnArg::Typed(pat_type) = arg {
        if let Pat::Ident(pat_ident) = pat_type.pat.as_ref() {
          return Some((&pat_ident.ident, &*pat_type.ty));
        }
      }
      None
    })
    .collect();

  let fixture_names: Vec<String> = vec!["browser".into(), "context".into(), "page".into(), "test_info".into()];

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
    let fixture_array = fixture_names.iter().map(|f| quote! { #f });
    let ctx_param = ctx_param_name.clone();

    submissions.push(quote! {
      async fn #inner_fn_name(__pool: ferridriver_test::fixture::FixturePool) -> Result<(), ferridriver_test::model::TestFailure> {
        let #ctx_param = ferridriver_test::TestContext::new(__pool);
        #(#data_bindings)*
        #block
        Ok(())
      }

      inventory::submit! {
        ferridriver_test::discovery::TestRegistration {
          file: file!(),
          module_path: module_path!(),
          name: #test_name,
          fixture_requests: &[#(#fixture_array),*],
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

// ── Hook macros ──

/// Shared implementation for all four hook macros.
fn hook_impl(kind_tag: &str, is_suite_hook: bool, item: TokenStream) -> TokenStream {
  let input = parse_macro_input!(item as ItemFn);
  let fn_name = &input.sig.ident;
  let vis = &input.vis;
  let block = &input.block;
  let attrs = &input.attrs;

  let kind_ident = format_ident!("{}", kind_tag);

  // Extract parameter name for TestContext.
  let ctx_param_name = if let Some(FnArg::Typed(pt)) = input.sig.inputs.first() {
    if let Pat::Ident(pi) = pt.pat.as_ref() {
      pi.ident.clone()
    } else {
      format_ident!("ctx")
    }
  } else {
    format_ident!("ctx")
  };

  if is_suite_hook {
    // before_all / after_all: fn(FixturePool) -> Result
    let expanded = quote! {
      #(#attrs)*
      #vis fn #fn_name(__pool: ferridriver_test::fixture::FixturePool)
        -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = Result<(), ferridriver_test::model::TestFailure>> + Send>>
      {
        Box::pin(async move {
          let #ctx_param_name = ferridriver_test::TestContext::new(__pool);
          #block
          Ok(())
        })
      }

      inventory::submit! {
        ferridriver_test::discovery::HookRegistration {
          module_path: module_path!(),
          suite_hook_fn: Some(#fn_name),
          each_hook_fn: None,
          kind: ferridriver_test::discovery::HookKindTag::#kind_ident,
        }
      }
    };
    expanded.into()
  } else {
    // before_each / after_each: fn(FixturePool, Arc<TestInfo>) -> Result
    let expanded = quote! {
      #(#attrs)*
      #vis fn #fn_name(
        __pool: ferridriver_test::fixture::FixturePool,
        __info: ::std::sync::Arc<ferridriver_test::model::TestInfo>,
      ) -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = Result<(), ferridriver_test::model::TestFailure>> + Send>>
      {
        Box::pin(async move {
          let #ctx_param_name = ferridriver_test::TestContext::new(__pool);
          #block
          Ok(())
        })
      }

      inventory::submit! {
        ferridriver_test::discovery::HookRegistration {
          module_path: module_path!(),
          suite_hook_fn: None,
          each_hook_fn: Some(#fn_name),
          kind: ferridriver_test::discovery::HookKindTag::#kind_ident,
        }
      }
    };
    expanded.into()
  }
}

/// Runs once before all tests in the containing module (suite).
///
/// ```ignore
/// mod my_suite {
///     use ferridriver_test::prelude::*;
///
///     #[before_all]
///     async fn setup(ctx: TestContext) {
///         // seed database, etc.
///     }
///
///     #[ferritest]
///     async fn test_one(ctx: TestContext) { ... }
/// }
/// ```
#[proc_macro_attribute]
pub fn before_all(_attr: TokenStream, item: TokenStream) -> TokenStream {
  hook_impl("BeforeAll", true, item)
}

/// Runs once after all tests in the containing module (suite).
#[proc_macro_attribute]
pub fn after_all(_attr: TokenStream, item: TokenStream) -> TokenStream {
  hook_impl("AfterAll", true, item)
}

/// Runs before each test in the containing module (suite).
///
/// ```ignore
/// mod my_suite {
///     use ferridriver_test::prelude::*;
///
///     #[before_each]
///     async fn login(ctx: TestContext) {
///         let page = ctx.page().await?;
///         page.goto("/login", None).await?;
///     }
///
///     #[ferritest]
///     async fn dashboard_test(ctx: TestContext) { ... }
/// }
/// ```
#[proc_macro_attribute]
pub fn before_each(_attr: TokenStream, item: TokenStream) -> TokenStream {
  hook_impl("BeforeEach", false, item)
}

/// Runs after each test in the containing module (suite), even on failure.
#[proc_macro_attribute]
pub fn after_each(_attr: TokenStream, item: TokenStream) -> TokenStream {
  hook_impl("AfterEach", false, item)
}

//! Proc macros for the ferridriver BDD/Cucumber framework.
//!
//! Provides step definition macros that register async handler functions
//! via `inventory` for automatic collection at runtime.
//!
//! ```ignore
//! use ferridriver_bdd::prelude::*;
//!
//! #[given("I navigate to {string}")]
//! async fn navigate(world: &mut BrowserWorld, url: String) {
//!     world.page().goto(&url, None).await.map_err(|e| step_err!("{e}"))?;
//! }
//!
//! #[when("I click {string}")]
//! async fn click(world: &mut BrowserWorld, selector: String) {
//!     world.page().locator(&selector).click(None).await.map_err(|e| step_err!("{e}"))?;
//! }
//!
//! #[then("the page title should be {string}")]
//! async fn check_title(world: &mut BrowserWorld, expected: String) {
//!     let title = world.page().title().await.map_err(|e| step_err!("{e}"))?;
//!     assert_eq!(title, expected);
//! }
//! ```

use proc_macro::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{parse_macro_input, FnArg, ItemFn, Lit, Meta, Pat, Token};

// ── Step macro argument parsing ──

struct StepArgs {
  expression: String,
  is_regex: bool,
}

impl Parse for StepArgs {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    // Try to parse `regex = "pattern"` first.
    if input.peek(syn::Ident) {
      let ident: syn::Ident = input.fork().parse()?;
      if ident == "regex" {
        let _: syn::Ident = input.parse()?;
        let _: Token![=] = input.parse()?;
        let lit: Lit = input.parse()?;
        return match lit {
          Lit::Str(s) => Ok(Self { expression: s.value(), is_regex: true }),
          _ => Err(syn::Error::new_spanned(lit, "expected a string literal regex pattern")),
        };
      }
    }
    // Otherwise parse as a cucumber expression string.
    let lit: Lit = input.parse()?;
    match lit {
      Lit::Str(s) => Ok(Self { expression: s.value(), is_regex: false }),
      _ => Err(syn::Error::new_spanned(lit, "expected a string literal cucumber expression")),
    }
  }
}

// ── Hook macro argument parsing ──

struct HookArgs {
  point: String,
  tags: Option<String>,
  order: i32,
}

impl Parse for HookArgs {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    let point_ident: syn::Ident = input.parse()?;
    let point = point_ident.to_string();

    let valid = ["all", "feature", "scenario", "step"];
    if !valid.contains(&point.as_str()) {
      return Err(syn::Error::new_spanned(
        &point_ident,
        format!("expected one of: {}", valid.join(", ")),
      ));
    }

    let mut tags = None;
    let mut order = 0i32;

    if input.peek(Token![,]) {
      let metas = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
      for meta in metas {
        if let Meta::NameValue(nv) = &meta {
          let ident = nv.path.get_ident().map(ToString::to_string).unwrap_or_default();
          match ident.as_str() {
            "tags" => {
              if let syn::Expr::Lit(lit) = &nv.value {
                if let Lit::Str(s) = &lit.lit {
                  tags = Some(s.value());
                }
              }
            }
            "order" => {
              if let syn::Expr::Lit(lit) = &nv.value {
                if let Lit::Int(i) = &lit.lit {
                  order = i.base10_parse()?;
                }
              }
            }
            _ => {
              return Err(syn::Error::new_spanned(
                &nv.path,
                format!("unknown hook attribute: {ident}"),
              ));
            }
          }
        }
      }
    }

    Ok(Self { point, tags, order })
  }
}

// ── Step macro codegen ──

fn generate_step(kind: &str, attr: TokenStream, item: TokenStream) -> TokenStream {
  let args = parse_macro_input!(attr as StepArgs);
  let input = parse_macro_input!(item as ItemFn);

  let fn_name = &input.sig.ident;
  let fn_name_str = fn_name.to_string();
  let vis = &input.vis;
  let block = &input.block;
  let attrs = &input.attrs;
  let expression = &args.expression;
  let is_regex = args.is_regex;

  let kind_ident = syn::Ident::new(kind, proc_macro2::Span::call_site());

  // Extract parameters after `world: &mut BrowserWorld`.
  // First param is always `world`, remaining are cucumber expression captures.
  let mut param_extractions = Vec::new();
  let mut param_names = Vec::new();
  let mut param_idx = 0usize;

  let inputs: Vec<_> = input.sig.inputs.iter().collect();
  let special_params = ["table", "data_table", "docstring", "doc_string"];
  for arg in inputs.iter().skip(1) {
    // skip `world`
    if let FnArg::Typed(pat_type) = arg {
      if let Pat::Ident(pat_ident) = pat_type.pat.as_ref() {
        // Skip special parameters (data table, docstring) -- they are bound separately.
        if special_params.contains(&pat_ident.ident.to_string().as_str()) {
          continue;
        }
        let param_name = &pat_ident.ident;
        let param_type = &pat_type.ty;
        let idx = param_idx;

        let extraction = type_to_extraction(param_type, idx);
        param_extractions.push(quote! {
          let #param_name: #param_type = #extraction;
        });
        param_names.push(quote! { #param_name });
        param_idx += 1;
      }
    }
  }

  // Check if the function takes a data_table parameter (Option<&DataTable>).
  let has_table = inputs.iter().any(|arg| {
    if let FnArg::Typed(pat_type) = arg {
      if let Pat::Ident(pat_ident) = pat_type.pat.as_ref() {
        return pat_ident.ident == "data_table" || pat_ident.ident == "table";
      }
    }
    false
  });

  let has_docstring = inputs.iter().any(|arg| {
    if let FnArg::Typed(pat_type) = arg {
      if let Pat::Ident(pat_ident) = pat_type.pat.as_ref() {
        return pat_ident.ident == "docstring" || pat_ident.ident == "doc_string";
      }
    }
    false
  });

  let table_binding = if has_table {
    quote! { let table = __table; let data_table = __table; }
  } else {
    quote! { let _ = __table; }
  };

  let docstring_binding = if has_docstring {
    quote! { let docstring = __docstring; let doc_string = __docstring; }
  } else {
    quote! { let _ = __docstring; }
  };

  let handler_name = syn::Ident::new(
    &format!("__bdd_step_handler_{fn_name_str}"),
    proc_macro2::Span::call_site(),
  );
  let reg_name = syn::Ident::new(
    &format!("__bdd_step_reg_{fn_name_str}"),
    proc_macro2::Span::call_site(),
  );

  let expanded = quote! {
    #(#attrs)*
    #vis async fn #fn_name(
      __world: &mut ferridriver_bdd::world::BrowserWorld,
      __params: Vec<ferridriver_bdd::step::StepParam>,
      __table: Option<&ferridriver_bdd::step::DataTable>,
      __docstring: Option<&str>,
    ) -> Result<(), ferridriver_bdd::step::StepError> {
      #(#param_extractions)*
      #table_binding
      #docstring_binding
      let world = __world;
      #block
      Ok(())
    }

    fn #handler_name() -> ferridriver_bdd::step::StepHandler {
      std::sync::Arc::new(
        |world, params, table, docstring| {
          Box::pin(#fn_name(world, params, table, docstring))
        },
      )
    }

    ferridriver_bdd::submit_step! {
      #reg_name,
      ferridriver_bdd::step::StepKind::#kind_ident,
      #expression,
      #handler_name,
      regex = #is_regex,
    }
  };

  expanded.into()
}

fn type_to_extraction(ty: &syn::Type, idx: usize) -> proc_macro2::TokenStream {
  let type_str = quote!(#ty).to_string();
  match type_str.trim() {
    "String" => quote! {
      __params.get(#idx)
        .and_then(|p| p.as_string())
        .unwrap_or_default()
    },
    "i64" => quote! {
      __params.get(#idx)
        .and_then(|p| p.as_int())
        .unwrap_or(0)
    },
    "f64" => quote! {
      __params.get(#idx)
        .and_then(|p| p.as_float())
        .unwrap_or(0.0)
    },
    _ => quote! {
      __params.get(#idx)
        .and_then(|p| p.as_string())
        .unwrap_or_default()
    },
  }
}

// ── Hook macro codegen ──

fn generate_hook(prefix: &str, attr: TokenStream, item: TokenStream) -> TokenStream {
  let args = parse_macro_input!(attr as HookArgs);
  let input = parse_macro_input!(item as ItemFn);

  let fn_name = &input.sig.ident;
  let fn_name_str = fn_name.to_string();
  let vis = &input.vis;
  let block = &input.block;
  let attrs = &input.attrs;

  let point = &args.point;
  let order = args.order;

  let hook_point = match point.as_str() {
    "all" => {
      if prefix == "Before" {
        quote! { ferridriver_bdd::hook::HookPoint::BeforeAll }
      } else {
        quote! { ferridriver_bdd::hook::HookPoint::AfterAll }
      }
    }
    "feature" => {
      if prefix == "Before" {
        quote! { ferridriver_bdd::hook::HookPoint::BeforeFeature }
      } else {
        quote! { ferridriver_bdd::hook::HookPoint::AfterFeature }
      }
    }
    "scenario" => {
      if prefix == "Before" {
        quote! { ferridriver_bdd::hook::HookPoint::BeforeScenario }
      } else {
        quote! { ferridriver_bdd::hook::HookPoint::AfterScenario }
      }
    }
    "step" => {
      if prefix == "Before" {
        quote! { ferridriver_bdd::hook::HookPoint::BeforeStep }
      } else {
        quote! { ferridriver_bdd::hook::HookPoint::AfterStep }
      }
    }
    _ => unreachable!(),
  };

  let tag_filter_expr = match &args.tags {
    Some(tags) => quote! { Some(#tags.to_string()) },
    None => quote! { None },
  };

  // Determine handler variant based on hook point.
  let is_global = point == "all";
  let has_world_param = input.sig.inputs.iter().any(|arg| {
    if let FnArg::Typed(_) = arg {
      return true;
    }
    false
  });

  let handler_name = syn::Ident::new(
    &format!("__bdd_hook_handler_{fn_name_str}"),
    proc_macro2::Span::call_site(),
  );
  let reg_name = syn::Ident::new(
    &format!("__bdd_hook_reg_{fn_name_str}"),
    proc_macro2::Span::call_site(),
  );

  let (fn_sig, handler_factory) = if is_global {
    (
      quote! {
        #vis async fn #fn_name() -> Result<(), String> {
          #block
          Ok(())
        }
      },
      quote! {
        fn #handler_name() -> ferridriver_bdd::hook::HookHandler {
          ferridriver_bdd::hook::HookHandler::Global(std::sync::Arc::new(|| {
            Box::pin(async { #fn_name().await })
          }))
        }
      },
    )
  } else if has_world_param {
    (
      quote! {
        #(#attrs)*
        #vis async fn #fn_name(
          world: &mut ferridriver_bdd::world::BrowserWorld,
        ) -> Result<(), String> {
          #block
          Ok(())
        }
      },
      quote! {
        fn #handler_name() -> ferridriver_bdd::hook::HookHandler {
          ferridriver_bdd::hook::HookHandler::Scenario(std::sync::Arc::new(|world| {
            Box::pin(async move { #fn_name(world).await })
          }))
        }
      },
    )
  } else {
    (
      quote! {
        #(#attrs)*
        #vis async fn #fn_name() -> Result<(), String> {
          #block
          Ok(())
        }
      },
      quote! {
        fn #handler_name() -> ferridriver_bdd::hook::HookHandler {
          ferridriver_bdd::hook::HookHandler::Global(std::sync::Arc::new(|| {
            Box::pin(async { #fn_name().await })
          }))
        }
      },
    )
  };

  let expanded = quote! {
    #fn_sig
    #handler_factory

    ferridriver_bdd::submit_hook! {
      #reg_name,
      #hook_point,
      #tag_filter_expr,
      #order,
      #handler_name,
    }
  };

  expanded.into()
}

// ── Custom parameter type macro ──

struct ParamTypeArgs {
  name: String,
  regex: String,
}

impl Parse for ParamTypeArgs {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    let metas = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
    let mut name = None;
    let mut regex = None;

    for meta in metas {
      if let Meta::NameValue(nv) = &meta {
        let ident = nv.path.get_ident().map(ToString::to_string).unwrap_or_default();
        if let syn::Expr::Lit(lit) = &nv.value {
          if let Lit::Str(s) = &lit.lit {
            match ident.as_str() {
              "name" => name = Some(s.value()),
              "regex" => regex = Some(s.value()),
              _ => {
                return Err(syn::Error::new_spanned(
                  &nv.path,
                  format!("unknown param_type attribute: {ident} (expected name, regex)"),
                ));
              }
            }
          }
        }
      }
    }

    Ok(Self {
      name: name.ok_or_else(|| syn::Error::new(input.span(), "missing `name` attribute"))?,
      regex: regex.ok_or_else(|| syn::Error::new(input.span(), "missing `regex` attribute"))?,
    })
  }
}

// ── Public proc macro attributes ──

/// Register a Given step definition.
///
/// ```ignore
/// #[given("I navigate to {string}")]
/// async fn navigate(world: &mut BrowserWorld, url: String) {
///     world.page().goto(&url, None).await.map_err(|e| step_err!("{e}"))?;
/// }
/// ```
#[proc_macro_attribute]
pub fn given(attr: TokenStream, item: TokenStream) -> TokenStream {
  generate_step("Given", attr, item)
}

/// Register a When step definition.
///
/// ```ignore
/// #[when("I click {string}")]
/// async fn click(world: &mut BrowserWorld, selector: String) {
///     world.page().locator(&selector).click(None).await.map_err(|e| step_err!("{e}"))?;
/// }
/// ```
#[proc_macro_attribute]
pub fn when(attr: TokenStream, item: TokenStream) -> TokenStream {
  generate_step("When", attr, item)
}

/// Register a Then step definition.
///
/// ```ignore
/// #[then("the page title should be {string}")]
/// async fn check_title(world: &mut BrowserWorld, expected: String) {
///     let title = world.page().title().await.map_err(|e| step_err!("{e}"))?;
///     assert_eq!(title, expected);
/// }
/// ```
#[proc_macro_attribute]
pub fn then(attr: TokenStream, item: TokenStream) -> TokenStream {
  generate_step("Then", attr, item)
}

/// Register a keyword-agnostic step definition (matches Given/When/Then).
///
/// ```ignore
/// #[step("I wait {int} second(s)")]
/// async fn wait_seconds(world: &mut BrowserWorld, seconds: i64) {
///     tokio::time::sleep(Duration::from_secs(seconds as u64)).await;
/// }
/// ```
#[proc_macro_attribute]
pub fn step(attr: TokenStream, item: TokenStream) -> TokenStream {
  generate_step("Step", attr, item)
}

/// Register a Before hook.
///
/// ```ignore
/// #[before(scenario)]
/// async fn setup(world: &mut BrowserWorld) { ... }
///
/// #[before(scenario, tags = "@cleanup")]
/// async fn cleanup(world: &mut BrowserWorld) { ... }
///
/// #[before(all)]
/// async fn global_setup() { ... }
/// ```
#[proc_macro_attribute]
pub fn before(attr: TokenStream, item: TokenStream) -> TokenStream {
  generate_hook("Before", attr, item)
}

/// Register an After hook.
///
/// ```ignore
/// #[after(scenario)]
/// async fn teardown(world: &mut BrowserWorld) { ... }
///
/// #[after(scenario, tags = "@cleanup")]
/// async fn cleanup(world: &mut BrowserWorld) { ... }
///
/// #[after(all)]
/// async fn global_teardown() { ... }
/// ```
#[proc_macro_attribute]
pub fn after(attr: TokenStream, item: TokenStream) -> TokenStream {
  generate_hook("After", attr, item)
}

/// Register a custom parameter type for Cucumber expressions.
///
/// Defines a new `{name}` placeholder that matches the given regex.
/// Use it on a dummy function whose body is discarded — only the
/// attribute arguments matter.
///
/// ```ignore
/// // Simple: defines {color} matching red|green|blue
/// #[param_type(name = "color", regex = "red|green|blue")]
/// fn color_type() {}
///
/// // Then use in step definitions:
/// #[given("I pick a {color} item")]
/// async fn pick(world: &mut BrowserWorld, color: String) {
///     // color = "red", "green", or "blue"
/// }
/// ```
#[proc_macro_attribute]
pub fn param_type(attr: TokenStream, item: TokenStream) -> TokenStream {
  let args = parse_macro_input!(attr as ParamTypeArgs);
  let input = parse_macro_input!(item as ItemFn);

  let fn_name = &input.sig.ident;
  let fn_name_str = fn_name.to_string();
  let name = &args.name;
  let regex = &args.regex;

  let _reg_name = syn::Ident::new(
    &format!("__bdd_param_type_reg_{fn_name_str}"),
    proc_macro2::Span::call_site(),
  );

  let expanded = quote! {
    ferridriver_bdd::inventory::submit! {
      ferridriver_bdd::param_type::ParameterTypeRegistration {
        name: #name,
        regex: #regex,
        transformer_factory: None,
      }
    }

    // Keep the function (but it's a no-op marker).
    #[allow(dead_code)]
    fn #fn_name() {}
  };

  expanded.into()
}

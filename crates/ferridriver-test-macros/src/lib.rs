//! Proc macros for the ferridriver test framework.
//!
//! Provides `#[ferritest]` to register async browser test functions. Test
//! parameters are fixtures: declare what the test needs and the runner
//! injects it. `TestContext` gives dynamic access; `Arc<T>` parameters
//! resolve the fixture whose name matches the parameter name.
//!
//! ```ignore
//! use ferridriver_test::prelude::*;
//!
//! #[ferritest]
//! async fn basic_navigation(page: Arc<Page>) {
//!     page.goto("https://example.com").await?;
//!     expect(&page).to_have_title("Example").await?;
//! }
//!
//! #[ferritest(retries = 2, timeout = "30s", tag = "smoke", viewport = "1280x720")]
//! async fn flaky_test(page: Arc<Page>, ctx: TestContext) {
//!     let context = ctx.browser_context().await?;
//!     // ...
//! }
//! ```

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Expr, FnArg, ItemFn, ItemMod, Lit, Meta, Pat, Token, Type, parse_macro_input, parse_quote};

/// Attribute arguments shared by `#[ferritest]` and `#[ferritest_each]`:
/// `retries`, `timeout`, `tag`, `skip`/`slow`/`fixme`/`fail`, `only`,
/// `info`, plus context overrides (`viewport`, `locale`, ... or raw
/// `use_options` JSON).
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
  /// Raw `use_options` JSON, parsed for validation and merged with the
  /// structured keys below (structured keys win).
  use_raw: Option<serde_json::Map<String, serde_json::Value>>,
  /// Context overrides from structured attribute keys (`viewport`,
  /// `locale`, ...), stored under their camelCase wire names.
  use_structured: serde_json::Map<String, serde_json::Value>,
}

impl Default for FerritestArgs {
  fn default() -> Self {
    Self {
      retries: None,
      timeout_ms: None,
      tags: Vec::new(),
      skip: None,
      slow: None,
      fixme: None,
      fail: None,
      only: false,
      infos: Vec::new(),
      use_raw: None,
      use_structured: serde_json::Map::new(),
    }
  }
}

fn lit_str(nv: &syn::MetaNameValue) -> syn::Result<String> {
  if let Expr::Lit(lit) = &nv.value {
    if let Lit::Str(s) = &lit.lit {
      return Ok(s.value());
    }
  }
  Err(syn::Error::new(nv.value.span(), "expected a string literal"))
}

fn lit_bool(nv: &syn::MetaNameValue) -> syn::Result<bool> {
  if let Expr::Lit(lit) = &nv.value {
    if let Lit::Bool(b) = &lit.lit {
      return Ok(b.value());
    }
  }
  Err(syn::Error::new(nv.value.span(), "expected a bool literal"))
}

fn lit_f64(nv: &syn::MetaNameValue) -> syn::Result<f64> {
  if let Expr::Lit(lit) = &nv.value {
    match &lit.lit {
      Lit::Float(f) => return f.base10_parse(),
      Lit::Int(i) => return i.base10_parse::<i64>().map(|v| v as f64),
      _ => {},
    }
  }
  Err(syn::Error::new(nv.value.span(), "expected a numeric literal"))
}

/// Structured context-override keys: snake_case attribute name to the
/// camelCase wire key used by the runner's `use_options` JSON.
const USE_STR_KEYS: &[(&str, &str)] = &[
  ("locale", "locale"),
  ("color_scheme", "colorScheme"),
  ("timezone_id", "timezoneId"),
  ("user_agent", "userAgent"),
  ("reduced_motion", "reducedMotion"),
  ("forced_colors", "forcedColors"),
  ("service_workers", "serviceWorkers"),
  ("storage_state", "storageState"),
  ("base_url", "baseURL"),
];

const USE_BOOL_KEYS: &[(&str, &str)] = &[
  ("is_mobile", "isMobile"),
  ("has_touch", "hasTouch"),
  ("offline", "offline"),
  ("java_script_enabled", "javaScriptEnabled"),
  ("bypass_csp", "bypassCSP"),
  ("accept_downloads", "acceptDownloads"),
  ("ignore_https_errors", "ignoreHTTPSErrors"),
];

impl FerritestArgs {
  /// Apply one attribute meta. Shared by `#[ferritest]` and
  /// `#[ferritest_each]` (which additionally handles `data`).
  fn apply_meta(&mut self, meta: &Meta) -> syn::Result<()> {
    match meta {
      Meta::NameValue(nv) => {
        let ident = nv.path.get_ident().map(ToString::to_string).unwrap_or_default();
        match ident.as_str() {
          "retries" => {
            if let Expr::Lit(lit) = &nv.value {
              if let Lit::Int(i) = &lit.lit {
                self.retries = Some(i.base10_parse()?);
                return Ok(());
              }
            }
            return Err(syn::Error::new(nv.value.span(), "expected an integer literal"));
          },
          "timeout" => self.timeout_ms = Some(parse_duration_str(&lit_str(nv)?)?),
          "tag" => self.tags.push(lit_str(nv)?),
          "skip" => self.skip = Some(Some(lit_str(nv)?)),
          "slow" => self.slow = Some(Some(lit_str(nv)?)),
          "fixme" => self.fixme = Some(Some(lit_str(nv)?)),
          "fail" => self.fail = Some(Some(lit_str(nv)?)),
          "info" => {
            let val = lit_str(nv)?;
            if let Some((type_name, desc)) = val.split_once(':') {
              self.infos.push((type_name.trim().to_string(), desc.trim().to_string()));
            } else {
              self.infos.push((val, String::new()));
            }
          },
          "use_options" => {
            let raw = lit_str(nv)?;
            let parsed: serde_json::Value = serde_json::from_str(&raw)
              .map_err(|e| syn::Error::new(nv.value.span(), format!("use_options is not valid JSON: {e}")))?;
            let serde_json::Value::Object(map) = parsed else {
              return Err(syn::Error::new(nv.value.span(), "use_options must be a JSON object"));
            };
            self.use_raw = Some(map);
          },
          "viewport" => {
            let val = lit_str(nv)?;
            let parsed = val.split_once('x').and_then(|(w, h)| {
              let w = w.trim().parse::<i64>().ok()?;
              let h = h.trim().parse::<i64>().ok()?;
              Some((w, h))
            });
            let Some((w, h)) = parsed else {
              return Err(syn::Error::new(
                nv.value.span(),
                "viewport must be \"<width>x<height>\", e.g. \"1280x720\"",
              ));
            };
            self
              .use_structured
              .insert("viewport".into(), serde_json::json!({ "width": w, "height": h }));
          },
          "device_scale_factor" => {
            self
              .use_structured
              .insert("deviceScaleFactor".into(), serde_json::json!(lit_f64(nv)?));
          },
          other => {
            if let Some((_, wire)) = USE_STR_KEYS.iter().find(|(attr, _)| *attr == other) {
              self
                .use_structured
                .insert((*wire).to_string(), serde_json::Value::String(lit_str(nv)?));
            } else if let Some((_, wire)) = USE_BOOL_KEYS.iter().find(|(attr, _)| *attr == other) {
              self
                .use_structured
                .insert((*wire).to_string(), serde_json::Value::Bool(lit_bool(nv)?));
            } else {
              return Err(syn::Error::new_spanned(
                &nv.path,
                format!("unknown ferritest attribute: {other}"),
              ));
            }
          },
        }
      },
      Meta::Path(p) => {
        let ident = p.get_ident().map(ToString::to_string).unwrap_or_default();
        match ident.as_str() {
          "skip" => self.skip = Some(None),
          "slow" => self.slow = Some(None),
          "fixme" => self.fixme = Some(None),
          "fail" => self.fail = Some(None),
          "only" => self.only = true,
          other => {
            if let Some((_, wire)) = USE_BOOL_KEYS.iter().find(|(attr, _)| *attr == other) {
              self
                .use_structured
                .insert((*wire).to_string(), serde_json::Value::Bool(true));
            } else {
              return Err(syn::Error::new_spanned(p, format!("unknown ferritest flag: {other}")));
            }
          },
        }
      },
      Meta::List(_) => {
        return Err(syn::Error::new_spanned(meta, "unexpected nested attribute"));
      },
    }
    Ok(())
  }

  /// Merge raw `use_options` JSON with structured keys (structured wins)
  /// into the final wire string, or `None` when no overrides were given.
  fn use_options_json(&self) -> Option<String> {
    if self.use_raw.is_none() && self.use_structured.is_empty() {
      return None;
    }
    let mut merged = self.use_raw.clone().unwrap_or_default();
    for (k, v) in &self.use_structured {
      merged.insert(k.clone(), v.clone());
    }
    Some(serde_json::Value::Object(merged).to_string())
  }

  fn annotations(&self) -> Vec<proc_macro2::TokenStream> {
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
    annotation_tokens("Skip", &self.skip, &mut annotations);
    annotation_tokens("Slow", &self.slow, &mut annotations);
    annotation_tokens("Fixme", &self.fixme, &mut annotations);
    annotation_tokens("Fail", &self.fail, &mut annotations);
    if self.only {
      annotations.push(quote! { ferridriver_test::model::TestAnnotation::Only });
    }
    for tag in &self.tags {
      annotations.push(quote! { ferridriver_test::model::TestAnnotation::Tag(#tag.to_string()) });
    }
    for (type_name, desc) in &self.infos {
      annotations.push(
        quote! { ferridriver_test::model::TestAnnotation::Info { type_name: #type_name.to_string(), description: #desc.to_string() } },
      );
    }
    annotations
  }
}

impl Parse for FerritestArgs {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    let mut args = Self::default();
    let metas = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
    for meta in metas {
      args.apply_meta(&meta)?;
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

// ── Fixture parameter injection ──

/// How one test-function parameter is bound.
enum ParamBinding {
  /// `ctx: TestContext` — dynamic fixture access.
  Context,
  /// `page: Arc<Page>` — resolves the fixture named after the parameter.
  Fixture { inner: Box<Type>, name: String },
  /// Anything else — only valid as a `#[ferritest_each]` data parameter.
  Data,
}

struct BoundParam {
  pat: syn::PatIdent,
  ty: Type,
  binding: ParamBinding,
}

fn classify_param(arg: &FnArg) -> syn::Result<BoundParam> {
  let FnArg::Typed(pt) = arg else {
    return Err(syn::Error::new(arg.span(), "test functions cannot take self"));
  };
  let Pat::Ident(pat) = pt.pat.as_ref() else {
    return Err(syn::Error::new(
      pt.pat.span(),
      "test parameters must be plain identifiers",
    ));
  };
  let binding = match pt.ty.as_ref() {
    Type::Path(tp) => {
      let last = tp.path.segments.last();
      match last {
        Some(seg) if seg.ident == "TestContext" => ParamBinding::Context,
        Some(seg) if seg.ident == "Arc" => {
          let inner = match &seg.arguments {
            syn::PathArguments::AngleBracketed(ab) => ab.args.iter().find_map(|a| {
              if let syn::GenericArgument::Type(t) = a {
                Some(t.clone())
              } else {
                None
              }
            }),
            _ => None,
          };
          let Some(inner) = inner else {
            return Err(syn::Error::new(
              pt.ty.span(),
              "Arc fixture parameter needs a type argument",
            ));
          };
          let name = pat.ident.to_string();
          let name = name.trim_start_matches('_').to_string();
          ParamBinding::Fixture {
            inner: Box::new(inner),
            name,
          }
        },
        _ => ParamBinding::Data,
      }
    },
    _ => ParamBinding::Data,
  };
  Ok(BoundParam {
    pat: pat.clone(),
    ty: pt.ty.as_ref().clone(),
    binding,
  })
}

/// Generate the `let` bindings that resolve fixture parameters, plus the
/// list of fixture names for the registration's `fixture_requests`.
fn fixture_binding_stmts(params: &[BoundParam]) -> (Vec<proc_macro2::TokenStream>, Vec<String>) {
  let mut stmts = Vec::new();
  let mut names = Vec::new();
  let needs_ctx = params
    .iter()
    .any(|p| matches!(p.binding, ParamBinding::Context | ParamBinding::Fixture { .. }));
  if needs_ctx {
    stmts.push(quote! { let __ferri_ctx = ferridriver_test::TestContext::new(__pool); });
  }
  for param in params {
    let pat = &param.pat;
    match &param.binding {
      ParamBinding::Context => stmts.push(quote! { let #pat = __ferri_ctx.clone(); }),
      ParamBinding::Fixture { inner, name } => {
        names.push(name.clone());
        stmts.push(quote! { let #pat = __ferri_ctx.get::<#inner>(#name).await?; });
      },
      ParamBinding::Data => {},
    }
  }
  (stmts, names)
}

/// `#[ferritest]` attribute macro.
///
/// Transforms an async function into a registered test case with automatic
/// fixture injection based on parameters:
///
/// - `ctx: TestContext` — dynamic access to any fixture (`ctx.page()`,
///   `ctx.get::<T>("name")`).
/// - `page: Arc<Page>`, `context: Arc<BrowserContext>`, `browser:
///   Arc<Browser>`, `request: Arc<HttpClient>`, `test_info: Arc<TestInfo>`
///   — built-in fixtures, resolved by parameter name.
/// - `seeded_users: Arc<Vec<User>>` — a custom `#[fixture]`, resolved by
///   parameter name.
///
/// ```ignore
/// #[ferritest]
/// async fn shows_dashboard(page: Arc<Page>, seeded_users: Arc<Vec<User>>) {
///     page.goto("/dashboard").await?;
///     expect(&page.locator("h1")).to_have_text(&seeded_users[0].name).await?;
/// }
/// ```
#[proc_macro_attribute]
pub fn ferritest(attr: TokenStream, item: TokenStream) -> TokenStream {
  let args = parse_macro_input!(attr as FerritestArgs);
  let input = parse_macro_input!(item as ItemFn);

  let fn_name = &input.sig.ident;
  let fn_name_str = fn_name.to_string();
  let vis = &input.vis;
  let block = &input.block;
  let attrs = &input.attrs;

  let params: Vec<BoundParam> = match input.sig.inputs.iter().map(classify_param).collect() {
    Ok(p) => p,
    Err(e) => return e.to_compile_error().into(),
  };
  if let Some(bad) = params.iter().find(|p| matches!(p.binding, ParamBinding::Data)) {
    return syn::Error::new(
      bad.ty.span(),
      "ferritest parameters must be `TestContext` or `Arc<T>` (fixtures are shared; \
       the fixture name is the parameter name)",
    )
    .to_compile_error()
    .into();
  }

  let (binding_stmts, fixture_names) = fixture_binding_stmts(&params);
  let fixture_array = fixture_names.iter().map(|f| quote! { #f });

  let annotations = args.annotations();
  let retries_expr = match args.retries {
    Some(r) => quote! { Some(#r) },
    None => quote! { None },
  };
  let timeout_ms_expr = match args.timeout_ms {
    Some(ms) => quote! { Some(#ms) },
    None => quote! { None },
  };
  let use_options_expr = match args.use_options_json() {
    Some(json) => quote! { Some(#json) },
    None => quote! { None },
  };

  let expanded = quote! {
    #(#attrs)*
    #[allow(clippy::unused_async)]
    #vis async fn #fn_name(__pool: ferridriver_test::fixture::FixturePool) -> Result<(), ferridriver_test::model::TestFailure> {
      #(#binding_stmts)*
      #block
      Ok(())
    }

    ferridriver_test::inventory::submit! {
      ferridriver_test::discovery::TestRegistration {
        file: file!(),
        module_path: module_path!(),
        name: #fn_name_str,
        fixture_requests: &[#(#fixture_array),*],
        annotations: || vec![#(#annotations),*],
        timeout_ms: #timeout_ms_expr,
        retries: #retries_expr,
        use_options: #use_options_expr,
        test_fn: |pool| Box::pin(#fn_name(pool)),
      }
    }
  };

  expanded.into()
}

/// Arguments for `#[ferritest_each]`: `data = [...]` plus every common
/// `#[ferritest]` argument (`retries`, `tag`, `skip`, `viewport`, ...).
struct FerritestEachArgs {
  data: Vec<Vec<Expr>>,
  /// Optional per-row display names (`names = ["admin", "guest"]`),
  /// used instead of the value-derived suffix.
  names: Option<Vec<String>>,
  common: FerritestArgs,
}

impl Parse for FerritestEachArgs {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    let mut data: Option<Vec<Vec<Expr>>> = None;
    let mut names: Option<Vec<String>> = None;
    let mut common = FerritestArgs::default();

    let metas = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
    for meta in metas {
      if let Meta::NameValue(nv) = &meta {
        if nv.path.is_ident("data") {
          let Expr::Array(arr) = &nv.value else {
            return Err(syn::Error::new(nv.value.span(), "expected `data = [(...), (...)]`"));
          };
          let mut rows = Vec::new();
          for elem in &arr.elems {
            match elem {
              Expr::Tuple(t) => rows.push(t.elems.iter().cloned().collect()),
              Expr::Paren(p) => rows.push(vec![(*p.expr).clone()]),
              other => rows.push(vec![other.clone()]),
            }
          }
          data = Some(rows);
          continue;
        }
        if nv.path.is_ident("names") {
          let Expr::Array(arr) = &nv.value else {
            return Err(syn::Error::new(nv.value.span(), "expected `names = [\"a\", \"b\"]`"));
          };
          let mut list = Vec::new();
          for elem in &arr.elems {
            if let Expr::Lit(lit) = elem {
              if let Lit::Str(s) = &lit.lit {
                list.push(s.value());
                continue;
              }
            }
            return Err(syn::Error::new(elem.span(), "names entries must be string literals"));
          }
          names = Some(list);
          continue;
        }
      }
      common.apply_meta(&meta)?;
    }

    let Some(data) = data else {
      return Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        "ferritest_each requires `data = [...]`",
      ));
    };
    if let Some(names) = &names {
      if names.len() != data.len() {
        return Err(syn::Error::new(
          proc_macro2::Span::call_site(),
          format!("names has {} entries but data has {} rows", names.len(), data.len()),
        ));
      }
    }
    Ok(Self { data, names, common })
  }
}

/// `#[ferritest_each(data = [("a", 1), ("b", 2)])]` — parameterized test macro.
///
/// Expands a single async test function into N registered tests, one per data
/// row. Fixture parameters (`TestContext`, `Arc<T>`) come first; the remaining
/// parameters receive the row values. Accepts every `#[ferritest]` argument
/// (`retries`, `tag`, `skip`, `viewport`, ...), applied to every generated test.
///
/// ```ignore
/// #[ferritest_each(data = [("admin", "admin@example.com"), ("guest", "guest@example.com")], tag = "auth")]
/// async fn login(page: Arc<Page>, role: &str, email: &str) {
///     page.goto(&format!("/login?role={role}")).await?;
/// }
/// ```
/// Registers: `login (admin, admin@example.com)` and `login (guest, guest@example.com)`.
#[proc_macro_attribute]
pub fn ferritest_each(attr: TokenStream, item: TokenStream) -> TokenStream {
  let args = parse_macro_input!(attr as FerritestEachArgs);
  let input = parse_macro_input!(item as ItemFn);

  let fn_name = &input.sig.ident;
  let fn_name_str = fn_name.to_string();
  let attrs = &input.attrs;
  let block = &input.block;

  let params: Vec<BoundParam> = match input.sig.inputs.iter().map(classify_param).collect() {
    Ok(p) => p,
    Err(e) => return e.to_compile_error().into(),
  };
  let (binding_stmts, fixture_names) = fixture_binding_stmts(&params);
  let data_params: Vec<&BoundParam> = params
    .iter()
    .filter(|p| matches!(p.binding, ParamBinding::Data))
    .collect();

  let annotations = args.common.annotations();
  let retries_expr = match args.common.retries {
    Some(r) => quote! { Some(#r) },
    None => quote! { None },
  };
  let timeout_ms_expr = match args.common.timeout_ms {
    Some(ms) => quote! { Some(#ms) },
    None => quote! { None },
  };
  let use_options_expr = match args.common.use_options_json() {
    Some(json) => quote! { Some(#json) },
    None => quote! { None },
  };

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

    // Name suffix: explicit `names = [...]` entry, or "(val1, val2)".
    let suffix = args.names.as_ref().map_or_else(
      || {
        row
          .iter()
          .map(|e| quote!(#e).to_string().replace('"', ""))
          .collect::<Vec<_>>()
          .join(", ")
      },
      |names| names[row_idx].clone(),
    );
    let test_name = format!("{fn_name_str} ({suffix})");

    // Build let bindings for data params.
    let data_bindings: Vec<_> = data_params
      .iter()
      .zip(row.iter())
      .map(|(param, value)| {
        let pat = &param.pat;
        let ty = &param.ty;
        quote! { let #pat: #ty = #value; }
      })
      .collect();

    let inner_fn_name = format_ident!("__ferritest_each_{}_{}", fn_name, row_idx);
    let fixture_array = fixture_names.iter().map(|f| quote! { #f });
    let binding_stmts = binding_stmts.clone();
    let annotations = annotations.clone();
    let retries_expr = retries_expr.clone();
    let timeout_ms_expr = timeout_ms_expr.clone();
    let use_options_expr = use_options_expr.clone();

    submissions.push(quote! {
      #[allow(clippy::unused_async)]
      async fn #inner_fn_name(__pool: ferridriver_test::fixture::FixturePool) -> Result<(), ferridriver_test::model::TestFailure> {
        #(#binding_stmts)*
        #(#data_bindings)*
        #block
        Ok(())
      }

      ferridriver_test::inventory::submit! {
        ferridriver_test::discovery::TestRegistration {
          file: file!(),
          module_path: module_path!(),
          name: #test_name,
          fixture_requests: &[#(#fixture_array),*],
          annotations: || vec![#(#annotations),*],
          timeout_ms: #timeout_ms_expr,
          retries: #retries_expr,
          use_options: #use_options_expr,
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

// ── Fixture macro ──

/// Fixture lifecycle scope, parsed from `scope = "..."`.
enum FixtureScopeArg {
  Test,
  Worker,
  Global,
}

/// Attribute arguments: `#[fixture(scope = "worker", auto, timeout = "10s")]`.
struct FixtureArgs {
  scope: FixtureScopeArg,
  auto: bool,
  timeout_ms: Option<u64>,
}

impl Parse for FixtureArgs {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    let mut args = Self {
      scope: FixtureScopeArg::Test,
      auto: false,
      timeout_ms: None,
    };
    let metas = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
    for meta in metas {
      match &meta {
        Meta::NameValue(nv) => {
          let ident = nv.path.get_ident().map(ToString::to_string).unwrap_or_default();
          match ident.as_str() {
            "scope" => {
              if let syn::Expr::Lit(lit) = &nv.value {
                if let Lit::Str(s) = &lit.lit {
                  args.scope = match s.value().as_str() {
                    "test" => FixtureScopeArg::Test,
                    "worker" => FixtureScopeArg::Worker,
                    "global" => FixtureScopeArg::Global,
                    other => {
                      return Err(syn::Error::new_spanned(
                        &nv.value,
                        format!("unknown fixture scope '{other}' (use \"test\", \"worker\", or \"global\")"),
                      ));
                    },
                  };
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
            _ => {
              return Err(syn::Error::new_spanned(
                &nv.path,
                format!("unknown fixture attribute: {ident}"),
              ));
            },
          }
        },
        Meta::Path(p) => {
          let ident = p.get_ident().map(ToString::to_string).unwrap_or_default();
          match ident.as_str() {
            "auto" => args.auto = true,
            _ => return Err(syn::Error::new_spanned(p, format!("unknown fixture flag: {ident}"))),
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

/// Whether a `#[fixture]` fn's return type is
/// `Result<Fixture<T>>` — the teardown-carrying guard form.
fn returns_fixture_guard(output: &syn::ReturnType) -> bool {
  let syn::ReturnType::Type(_, ty) = output else {
    return false;
  };
  let Type::Path(tp) = ty.as_ref() else {
    return false;
  };
  let Some(result_seg) = tp.path.segments.last() else {
    return false;
  };
  if result_seg.ident != "Result" {
    return false;
  }
  let syn::PathArguments::AngleBracketed(ab) = &result_seg.arguments else {
    return false;
  };
  ab.args.iter().any(|a| {
    if let syn::GenericArgument::Type(Type::Path(inner)) = a {
      inner.path.segments.last().is_some_and(|s| s.ident == "Fixture")
    } else {
      false
    }
  })
}

/// `#[fixture]` — register a custom, dependency-injected, scoped fixture.
///
/// The function takes fixture parameters like a test (`TestContext` for
/// dynamic access, `Arc<T>` to resolve another fixture by parameter name)
/// and returns `ferridriver_test::Result<T>`. The resolved value is shared
/// as `Arc<T>` and retrieved from a test (or another fixture) via a typed
/// `Arc<T>` parameter or `ctx.get::<T>("fixture_name")`.
///
/// ```ignore
/// use ferridriver_test::prelude::*;
/// use std::sync::Arc;
///
/// #[fixture(scope = "test")]
/// async fn authed_page(page: Arc<Page>) -> ferridriver_test::Result<Arc<Page>> {
///     page.goto("https://app.example.com/login").await?;
///     page.locator("#email").fill("user@example.com").await?;
///     page.locator("button[type=submit]").click().await?;
///     Ok(page)
/// }
///
/// #[ferritest]
/// async fn shows_dashboard(authed_page: Arc<Arc<Page>>) {
///     expect(&authed_page.locator("h1")).to_have_text("Dashboard").await?;
/// }
/// ```
#[proc_macro_attribute]
pub fn fixture(attr: TokenStream, item: TokenStream) -> TokenStream {
  let args = parse_macro_input!(attr as FixtureArgs);
  let input = parse_macro_input!(item as ItemFn);

  let params: Vec<BoundParam> = match input.sig.inputs.iter().map(classify_param).collect() {
    Ok(p) => p,
    Err(e) => return e.to_compile_error().into(),
  };
  if let Some(bad) = params.iter().find(|p| matches!(p.binding, ParamBinding::Data)) {
    return syn::Error::new(
      bad.ty.span(),
      "fixture parameters must be `TestContext` or `Arc<T>` (the fixture name is the parameter name)",
    )
    .to_compile_error()
    .into();
  }

  let fn_name = &input.sig.ident;
  let fn_name_str = fn_name.to_string();
  let builder_ident = format_ident!("__ferridriver_fixture_build_{}", fn_name);

  let scope_tok = match args.scope {
    FixtureScopeArg::Test => quote! { ferridriver_test::fixture::FixtureScope::Test },
    FixtureScopeArg::Worker => quote! { ferridriver_test::fixture::FixtureScope::Worker },
    FixtureScopeArg::Global => quote! { ferridriver_test::fixture::FixtureScope::Global },
  };
  let timeout_ms = args.timeout_ms.unwrap_or(10_000);
  let auto = args.auto;

  // The setup closure resolves this fixture's own parameters the same way
  // tests do, then calls the user's function. Fresh idents (not the user's
  // parameter names) so an `_`-prefixed parameter doesn't become a used
  // underscore binding in the expansion.
  let call_args: Vec<syn::Ident> = (0..params.len()).map(|i| format_ident!("__ferri_arg{}", i)).collect();
  let mut setup_stmts = Vec::new();
  let needs_ctx = !params.is_empty();
  if needs_ctx {
    setup_stmts.push(quote! { let __ferri_ctx = ferridriver_test::TestContext::new(__pool.clone()); });
  }
  let mut dependency_names = Vec::new();
  for (p, arg_ident) in params.iter().zip(&call_args) {
    match &p.binding {
      ParamBinding::Context => setup_stmts.push(quote! { let #arg_ident = __ferri_ctx.clone(); }),
      ParamBinding::Fixture { inner, name } => {
        dependency_names.push(name.clone());
        setup_stmts.push(quote! {
          let #arg_ident = __ferri_ctx.get::<#inner>(#name).await.map_err(|__e| {
            ferridriver::error::FerriError::backend(::std::format!(
              "fixture '{}' dependency '{}' failed: {}", #fn_name_str, #name, __e
            ))
          })?;
        });
      },
      ParamBinding::Data => {},
    }
  }
  let dependency_array = dependency_names.iter().map(|d| quote! { #d.to_string() });

  // `Result<Fixture<T>>` bodies carry a teardown: unwrap the guard and
  // register the teardown on the resolving pool (runs at scope end).
  let unwrap_guard = if returns_fixture_guard(&input.sig.output) {
    quote! {
      let (__value, __teardown) = __value.into_parts();
      if let ::std::option::Option::Some(__td) = __teardown {
        __pool.register_teardown(#fn_name_str, __td);
      }
    }
  } else {
    quote! {}
  };

  let expanded = quote! {
    // Keep the user's function callable. Fixtures are async by contract
    // (most await a built-in or another fixture); a data-only fixture that
    // never awaits is still valid, so silence the no-await lint here rather
    // than forcing every author to add a workaround.
    #[allow(clippy::unused_async)]
    #input

    #[doc(hidden)]
    fn #builder_ident() -> ferridriver_test::fixture::FixtureDef {
      ferridriver_test::fixture::FixtureDef {
        name: #fn_name_str.to_string(),
        scope: #scope_tok,
        dependencies: ::std::vec![#(#dependency_array),*],
        setup: ::std::sync::Arc::new(|__pool: ferridriver_test::fixture::FixturePool| {
          ::std::boxed::Box::pin(async move {
            #(#setup_stmts)*
            let __value = #fn_name(#(#call_args),*).await.map_err(|__e| {
              ferridriver::error::FerriError::backend(::std::format!("fixture '{}' failed: {}", #fn_name_str, __e))
            })?;
            #unwrap_guard
            ::std::result::Result::Ok(
              ::std::sync::Arc::new(__value)
                as ::std::sync::Arc<dyn ::std::any::Any + ::std::marker::Send + ::std::marker::Sync>,
            )
          })
        }),
        teardown: ::std::option::Option::None,
        timeout: ::std::time::Duration::from_millis(#timeout_ms),
        auto: #auto,
      }
    }

    ferridriver_test::inventory::submit! {
      ferridriver_test::discovery::FixtureRegistration {
        name: #fn_name_str,
        module_path: ::core::module_path!(),
        build: #builder_ident,
      }
    }
  };

  expanded.into()
}

// ── Suite mode macro ──

/// Suite execution mode parsed from `mode = "..."`.
enum SuiteModeArg {
  Serial,
  Parallel,
}

struct SuiteArgs {
  mode: SuiteModeArg,
}

impl Parse for SuiteArgs {
  fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
    let mut mode = SuiteModeArg::Parallel;
    let metas = Punctuated::<Meta, Token![,]>::parse_terminated(input)?;
    for meta in metas {
      match &meta {
        Meta::NameValue(nv) if nv.path.is_ident("mode") => {
          if let syn::Expr::Lit(lit) = &nv.value {
            if let Lit::Str(s) = &lit.lit {
              mode = match s.value().as_str() {
                "serial" => SuiteModeArg::Serial,
                "parallel" => SuiteModeArg::Parallel,
                other => {
                  return Err(syn::Error::new_spanned(
                    &nv.value,
                    format!("unknown suite mode '{other}' (use \"serial\" or \"parallel\")"),
                  ));
                },
              };
            }
          }
        },
        _ => {
          return Err(syn::Error::new_spanned(
            &meta,
            "expected `mode = \"serial\" | \"parallel\"`",
          ));
        },
      }
    }
    Ok(Self { mode })
  }
}

/// `#[ferritest_suite(mode = "serial")]` — set the execution mode of every
/// `#[ferritest]` in the annotated module. A serial suite is dispatched as
/// one batch to a single worker, runs in source order, and skips the rest
/// on first failure. The default (no attribute) is parallel.
///
/// ```ignore
/// #[ferritest_suite(mode = "serial")]
/// mod payment_flow {
///     use ferridriver_test::prelude::*;
///
///     #[ferritest]
///     async fn initiate(page: Arc<Page>) { /* ... */ }
///     #[ferritest]
///     async fn verify_receipt(page: Arc<Page>) { /* runs only if initiate passed */ }
/// }
/// ```
#[proc_macro_attribute]
pub fn ferritest_suite(attr: TokenStream, item: TokenStream) -> TokenStream {
  let args = parse_macro_input!(attr as SuiteArgs);
  let mut module = parse_macro_input!(item as ItemMod);

  let Some((_, ref mut items)) = module.content else {
    return syn::Error::new_spanned(
      &module,
      "#[ferritest_suite] requires an inline module body `mod name { ... }`",
    )
    .to_compile_error()
    .into();
  };

  let mode_tok = match args.mode {
    SuiteModeArg::Serial => quote! { ferridriver_test::model::SuiteMode::Serial },
    SuiteModeArg::Parallel => quote! { ferridriver_test::model::SuiteMode::Parallel },
  };

  // Inject the registration INSIDE the module so `module_path!()` resolves
  // to this module's path — the same key `#[ferritest]` registrations derive
  // their suite name from.
  let submit: syn::Item = parse_quote! {
    ferridriver_test::inventory::submit! {
      ferridriver_test::discovery::SuiteModeRegistration {
        module_path: ::core::module_path!(),
        mode: #mode_tok,
      }
    }
  };
  items.push(submit);

  quote! { #module }.into()
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

  let params: Vec<BoundParam> = match input.sig.inputs.iter().map(classify_param).collect() {
    Ok(p) => p,
    Err(e) => return e.to_compile_error().into(),
  };
  if let Some(bad) = params.iter().find(|p| matches!(p.binding, ParamBinding::Data)) {
    return syn::Error::new(
      bad.ty.span(),
      "hook parameters must be `TestContext` or `Arc<T>` (the fixture name is the parameter name)",
    )
    .to_compile_error()
    .into();
  }
  let (binding_stmts, _) = fixture_binding_stmts(&params);

  if is_suite_hook {
    // before_all / after_all: fn(FixturePool) -> Result
    let expanded = quote! {
      #(#attrs)*
      #vis fn #fn_name(__pool: ferridriver_test::fixture::FixturePool)
        -> ::std::pin::Pin<Box<dyn ::std::future::Future<Output = Result<(), ferridriver_test::model::TestFailure>> + Send>>
      {
        Box::pin(async move {
          #(#binding_stmts)*
          #block
          Ok(())
        })
      }

      ferridriver_test::inventory::submit! {
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
          #(#binding_stmts)*
          #block
          Ok(())
        })
      }

      ferridriver_test::inventory::submit! {
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
///     async fn test_one(page: Arc<Page>) { ... }
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
///     async fn login(page: Arc<Page>) {
///         page.goto("/login").await?;
///     }
///
///     #[ferritest]
///     async fn dashboard_test(page: Arc<Page>) { ... }
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

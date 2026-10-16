//! Locator web-first matchers — single source of truth shared by the
//! test runner (`ferridriver-test`) and the QuickJS binding
//! (`ferridriver-script`). Screenshot / aria-snapshot / value-snapshot
//! matchers stay in `ferridriver-test` because they pull in image and
//! aria-YAML infrastructure that does not belong in this lightweight
//! crate.
//!
//! Each matcher is Playwright's `matchers.ts` entry: it names the
//! expression the page's engine judges and how a failure prints. The
//! judging and the retrying are in the `web` module.

use std::borrow::Borrow;

use ferridriver::Locator;
use ferridriver::expect::ExpectParams;

use crate::AssertionFailure;
use crate::builder::{CheckedOptions, Expect, HaveCssOptions, InViewportOptions};
use crate::value::StringOrRegex;
use crate::web::{Printed, Receiver, Shown, WebAssert, expected_text};

/// Options shared by the text matchers (`toHaveText` / `toContainText`).
/// Mirrors Playwright's `{ ignoreCase?, useInnerText? }`.
#[derive(Debug, Clone, Copy, Default)]
pub struct TextMatchOptions {
  pub ignore_case: bool,
  pub use_inner_text: bool,
}

impl<L: Borrow<Locator>> Expect<'_, L> {
  fn web(&self, method: &'static str, params: ExpectParams, printed: Printed) -> WebAssert<'_> {
    WebAssert {
      method,
      receiver: Receiver::Locator(self.subject.borrow()),
      params,
      printed,
      timeout: self.timeout,
      is_soft: self.is_soft,
    }
  }

  fn params(&self, expression: &str) -> ExpectParams {
    ExpectParams::new(expression, self.is_not)
  }

  async fn truthy(
    &self,
    method: &'static str,
    params: ExpectParams,
    expected: &str,
    arg: &'static str,
  ) -> Result<(), AssertionFailure> {
    self
      .web(
        method,
        params,
        Printed::Truthy {
          expected: expected.to_owned(),
          arg,
        },
      )
      .run()
      .await
  }

  async fn text(
    &self,
    method: &'static str,
    mut params: ExpectParams,
    expected: StringOrRegex,
    substring: bool,
    normalize: bool,
    ignore_case: Option<bool>,
  ) -> Result<(), AssertionFailure> {
    params.expected_text = Some(vec![expected_text(&expected, substring, normalize, ignore_case)]);
    self
      .web(method, params, Printed::Text { expected, substring })
      .run()
      .await
  }

  async fn texts(
    &self,
    method: &'static str,
    mut params: ExpectParams,
    expected: Vec<StringOrRegex>,
    substring: bool,
    normalize: bool,
    ignore_case: Option<bool>,
  ) -> Result<(), AssertionFailure> {
    params.expected_text = Some(
      expected
        .iter()
        .map(|e| expected_text(e, substring, normalize, ignore_case))
        .collect(),
    );
    self
      .web(method, params, Printed::Equal(Shown::Texts(expected)))
      .run()
      .await
  }

  // ── Visibility / State ──

  pub async fn to_be_visible(&self) -> Result<(), AssertionFailure> {
    self.to_be_visible_with(true).await
  }

  /// Playwright: `toBeVisible(options?: { visible?, timeout? })`.
  pub async fn to_be_visible_with(&self, visible: bool) -> Result<(), AssertionFailure> {
    let (expression, expected, arg) = if visible {
      ("to.be.visible", "visible", "")
    } else {
      ("to.be.hidden", "hidden", "{ visible: false }")
    };
    self.truthy("toBeVisible", self.params(expression), expected, arg).await
  }

  pub async fn to_be_hidden(&self) -> Result<(), AssertionFailure> {
    self
      .truthy("toBeHidden", self.params("to.be.hidden"), "hidden", "")
      .await
  }

  pub async fn to_be_enabled(&self) -> Result<(), AssertionFailure> {
    self.to_be_enabled_with(true).await
  }

  /// Playwright: `toBeEnabled(options?: { enabled?, timeout? })`.
  pub async fn to_be_enabled_with(&self, enabled: bool) -> Result<(), AssertionFailure> {
    let (expression, expected, arg) = if enabled {
      ("to.be.enabled", "enabled", "")
    } else {
      ("to.be.disabled", "disabled", "{ enabled: false }")
    };
    self.truthy("toBeEnabled", self.params(expression), expected, arg).await
  }

  pub async fn to_be_disabled(&self) -> Result<(), AssertionFailure> {
    self
      .truthy("toBeDisabled", self.params("to.be.disabled"), "disabled", "")
      .await
  }

  pub async fn to_be_checked(&self) -> Result<(), AssertionFailure> {
    self.to_be_checked_with(CheckedOptions::default()).await
  }

  /// Playwright: `toBeChecked(options?: { checked?, indeterminate?, timeout? })`.
  /// The engine refuses `indeterminate` together with `checked`.
  pub async fn to_be_checked_with(&self, options: CheckedOptions) -> Result<(), AssertionFailure> {
    let mut params = self.params("to.be.checked");
    let mut expected_value = serde_json::Map::new();
    if let Some(checked) = options.checked {
      expected_value.insert("checked".into(), checked.into());
    }
    if let Some(indeterminate) = options.indeterminate {
      expected_value.insert("indeterminate".into(), indeterminate.into());
    }
    params.expected_value = Some(expected_value.into());
    let (expected, arg) = if options.indeterminate == Some(true) {
      ("indeterminate", "{ indeterminate: true }")
    } else if options.checked == Some(false) {
      ("unchecked", "{ checked: false }")
    } else {
      ("checked", "")
    };
    self.truthy("toBeChecked", params, expected, arg).await
  }

  pub async fn to_be_editable(&self) -> Result<(), AssertionFailure> {
    self.to_be_editable_with(true).await
  }

  /// Playwright: `toBeEditable(options?: { editable?, timeout? })`.
  /// `editable: false` is `to.be.readonly`, which the engine judges as
  /// "not editable", so a disabled input satisfies it too.
  pub async fn to_be_editable_with(&self, editable: bool) -> Result<(), AssertionFailure> {
    let (expression, expected, arg) = if editable {
      ("to.be.editable", "editable", "")
    } else {
      ("to.be.readonly", "readOnly", "{ editable: false }")
    };
    self
      .truthy("toBeEditable", self.params(expression), expected, arg)
      .await
  }

  pub async fn to_be_readonly(&self) -> Result<(), AssertionFailure> {
    self.to_be_editable_with(false).await
  }

  pub async fn to_be_attached(&self) -> Result<(), AssertionFailure> {
    self.to_be_attached_with(true).await
  }

  /// Playwright: `toBeAttached(options?: { attached?, timeout? })`.
  pub async fn to_be_attached_with(&self, attached: bool) -> Result<(), AssertionFailure> {
    let (expression, expected, arg) = if attached {
      ("to.be.attached", "attached", "")
    } else {
      ("to.be.detached", "detached", "{ attached: false }")
    };
    self
      .truthy("toBeAttached", self.params(expression), expected, arg)
      .await
  }

  pub async fn to_be_empty(&self) -> Result<(), AssertionFailure> {
    self.truthy("toBeEmpty", self.params("to.be.empty"), "empty", "").await
  }

  pub async fn to_be_focused(&self) -> Result<(), AssertionFailure> {
    self
      .truthy("toBeFocused", self.params("to.be.focused"), "focused", "")
      .await
  }

  pub async fn to_be_in_viewport(&self) -> Result<(), AssertionFailure> {
    self.to_be_in_viewport_with(InViewportOptions::default()).await
  }

  pub async fn to_be_in_viewport_with(&self, options: InViewportOptions) -> Result<(), AssertionFailure> {
    let mut params = self.params("to.be.in.viewport");
    params.expected_number = options.ratio;
    self.truthy("toBeInViewport", params, "in viewport", "").await
  }

  // ── Text / Value ──

  pub async fn to_have_text(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    self.to_have_text_with(expected, TextMatchOptions::default()).await
  }

  pub async fn to_have_text_with(
    &self,
    expected: impl Into<StringOrRegex>,
    options: TextMatchOptions,
  ) -> Result<(), AssertionFailure> {
    let mut params = self.params("to.have.text");
    params.use_inner_text = options.use_inner_text.then_some(true);
    self
      .text(
        "toHaveText",
        params,
        expected.into(),
        false,
        true,
        options.ignore_case.then_some(true),
      )
      .await
  }

  pub async fn to_contain_text(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    self.to_contain_text_with(expected, TextMatchOptions::default()).await
  }

  pub async fn to_contain_text_with(
    &self,
    expected: impl Into<StringOrRegex>,
    options: TextMatchOptions,
  ) -> Result<(), AssertionFailure> {
    let mut params = self.params("to.have.text");
    params.use_inner_text = options.use_inner_text.then_some(true);
    self
      .text(
        "toContainText",
        params,
        expected.into(),
        true,
        true,
        options.ignore_case.then_some(true),
      )
      .await
  }

  /// Playwright: `toHaveText(expected: (string | RegExp)[], …)`: as many
  /// elements as expectations, matched in order.
  pub async fn to_have_text_array_with(
    &self,
    expected: &[StringOrRegex],
    options: TextMatchOptions,
  ) -> Result<(), AssertionFailure> {
    let mut params = self.params("to.have.text.array");
    params.use_inner_text = options.use_inner_text.then_some(true);
    self
      .texts(
        "toHaveText",
        params,
        expected.to_vec(),
        false,
        true,
        options.ignore_case.then_some(true),
      )
      .await
  }

  /// Playwright: `toContainText(expected: (string | RegExp)[], …)`: the
  /// expectations appear, each as a substring, in order among the
  /// elements.
  pub async fn to_contain_text_array_with(
    &self,
    expected: &[StringOrRegex],
    options: TextMatchOptions,
  ) -> Result<(), AssertionFailure> {
    let mut params = self.params("to.contain.text.array");
    params.use_inner_text = options.use_inner_text.then_some(true);
    self
      .texts(
        "toContainText",
        params,
        expected.to_vec(),
        true,
        true,
        options.ignore_case.then_some(true),
      )
      .await
  }

  pub async fn to_have_value(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    self
      .text(
        "toHaveValue",
        self.params("to.have.value"),
        expected.into(),
        false,
        false,
        None,
      )
      .await
  }

  /// Playwright: `toHaveValues(values: (string | RegExp)[], options?)` on a
  /// `<select multiple>`.
  pub async fn to_have_values(&self, expected: &[impl Into<StringOrRegex> + Clone]) -> Result<(), AssertionFailure> {
    let expected = expected.iter().map(|e| e.clone().into()).collect();
    self
      .texts(
        "toHaveValues",
        self.params("to.have.values"),
        expected,
        false,
        false,
        None,
      )
      .await
  }

  // ── Attributes ──

  pub async fn to_have_attribute(&self, name: &str, value: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    self.to_have_attribute_with(name, value, false).await
  }

  pub async fn to_have_attribute_with(
    &self,
    name: &str,
    value: impl Into<StringOrRegex>,
    ignore_case: bool,
  ) -> Result<(), AssertionFailure> {
    let mut params = self.params("to.have.attribute.value");
    params.expression_arg = Some(name.into());
    self
      .text(
        "toHaveAttribute",
        params,
        value.into(),
        false,
        false,
        ignore_case.then_some(true),
      )
      .await
  }

  /// Playwright: `toHaveAttribute(name, options?)`, the presence check.
  pub async fn to_have_attribute_exists(&self, name: &str) -> Result<(), AssertionFailure> {
    let mut params = self.params("to.have.attribute");
    params.expression_arg = Some(name.into());
    self.truthy("toHaveAttribute", params, "have attribute", "").await
  }

  pub async fn to_have_class(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    self
      .text(
        "toHaveClass",
        self.params("to.have.class"),
        expected.into(),
        false,
        false,
        None,
      )
      .await
  }

  /// Playwright: `toHaveClass(expected: (string | RegExp)[])`, one class
  /// list per element.
  pub async fn to_have_class_array(&self, expected: &[StringOrRegex]) -> Result<(), AssertionFailure> {
    self
      .texts(
        "toHaveClass",
        self.params("to.have.class.array"),
        expected.to_vec(),
        false,
        false,
        None,
      )
      .await
  }

  pub async fn to_contain_class(&self, expected: &str) -> Result<(), AssertionFailure> {
    self
      .text(
        "toContainClass",
        self.params("to.contain.class"),
        expected.into(),
        false,
        false,
        None,
      )
      .await
  }

  /// Playwright: `toContainClass(expected: string[])`, one entry per
  /// element, each a set of classes the element must carry.
  pub async fn to_contain_class_array(&self, expected: &[impl AsRef<str>]) -> Result<(), AssertionFailure> {
    let expected = expected
      .iter()
      .map(|e| StringOrRegex::String(e.as_ref().to_owned()))
      .collect();
    self
      .texts(
        "toContainClass",
        self.params("to.contain.class.array"),
        expected,
        false,
        false,
        None,
      )
      .await
  }

  pub async fn to_have_css(&self, property: &str, value: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    self.to_have_css_with(property, value, HaveCssOptions::default()).await
  }

  pub async fn to_have_css_with(
    &self,
    property: &str,
    value: impl Into<StringOrRegex>,
    options: HaveCssOptions,
  ) -> Result<(), AssertionFailure> {
    let mut params = self.params("to.have.css");
    params.expression_arg = Some(property.into());
    params.pseudo = options.pseudo.map(|p| p.as_str().to_owned());
    self.text("toHaveCSS", params, value.into(), false, false, None).await
  }

  pub async fn to_have_id(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    self
      .text(
        "toHaveId",
        self.params("to.have.id"),
        expected.into(),
        false,
        false,
        None,
      )
      .await
  }

  /// Playwright: `toHaveRole(role: string, options?)`. The role is a
  /// string; a pattern is refused as upstream refuses it.
  pub async fn to_have_role(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    let expected = expected.into();
    if matches!(expected, StringOrRegex::Regex(_)) {
      return Err(AssertionFailure::new(
        "\"role\" argument in toHaveRole must be a string",
        None,
      ));
    }
    self
      .text("toHaveRole", self.params("to.have.role"), expected, false, false, None)
      .await
  }

  pub async fn to_have_accessible_name(&self, expected: impl Into<StringOrRegex>) -> Result<(), AssertionFailure> {
    self.to_have_accessible_name_with(expected, false).await
  }

  /// Playwright: `toHaveAccessibleName(name, options?: { ignoreCase?, timeout? })`.
  pub async fn to_have_accessible_name_with(
    &self,
    expected: impl Into<StringOrRegex>,
    ignore_case: bool,
  ) -> Result<(), AssertionFailure> {
    self
      .text(
        "toHaveAccessibleName",
        self.params("to.have.accessible.name"),
        expected.into(),
        false,
        true,
        ignore_case.then_some(true),
      )
      .await
  }

  pub async fn to_have_accessible_description(
    &self,
    expected: impl Into<StringOrRegex>,
  ) -> Result<(), AssertionFailure> {
    self.to_have_accessible_description_with(expected, false).await
  }

  /// Playwright: `toHaveAccessibleDescription(description, options?: { ignoreCase?, timeout? })`.
  pub async fn to_have_accessible_description_with(
    &self,
    expected: impl Into<StringOrRegex>,
    ignore_case: bool,
  ) -> Result<(), AssertionFailure> {
    self
      .text(
        "toHaveAccessibleDescription",
        self.params("to.have.accessible.description"),
        expected.into(),
        false,
        true,
        ignore_case.then_some(true),
      )
      .await
  }

  pub async fn to_have_accessible_error_message(
    &self,
    expected: impl Into<StringOrRegex>,
  ) -> Result<(), AssertionFailure> {
    self.to_have_accessible_error_message_with(expected, false).await
  }

  /// Playwright: `toHaveAccessibleErrorMessage(message, options?: { ignoreCase?, timeout? })`.
  pub async fn to_have_accessible_error_message_with(
    &self,
    expected: impl Into<StringOrRegex>,
    ignore_case: bool,
  ) -> Result<(), AssertionFailure> {
    self
      .text(
        "toHaveAccessibleErrorMessage",
        self.params("to.have.accessible.error.message"),
        expected.into(),
        false,
        true,
        ignore_case.then_some(true),
      )
      .await
  }

  /// Playwright: `toHaveJSProperty(name, value)`. A dotted name walks
  /// into nested objects.
  pub async fn to_have_js_property(&self, name: &str, value: serde_json::Value) -> Result<(), AssertionFailure> {
    let mut params = self.params("to.have.property");
    params.expression_arg = Some(name.into());
    params.expected_value = Some(value.clone());
    self
      .web("toHaveJSProperty", params, Printed::Equal(Shown::Json(value)))
      .run()
      .await
  }

  // ── Array text matchers ──

  /// ferridriver-only alias kept for the Rust E2E surface. Playwright
  /// spells this `toHaveText([...])`, so it delegates rather than
  /// carrying a second implementation.
  pub async fn to_have_texts(&self, expected: &[impl Into<StringOrRegex> + Clone]) -> Result<(), AssertionFailure> {
    let expected: Vec<StringOrRegex> = expected.iter().map(|e| e.clone().into()).collect();
    self
      .to_have_text_array_with(&expected, TextMatchOptions::default())
      .await
  }

  /// ferridriver-only alias for `toContainText([...])`.
  pub async fn to_contain_texts(&self, expected: &[impl Into<StringOrRegex> + Clone]) -> Result<(), AssertionFailure> {
    let expected: Vec<StringOrRegex> = expected.iter().map(|e| e.clone().into()).collect();
    self
      .to_contain_text_array_with(&expected, TextMatchOptions::default())
      .await
  }

  // ── Count ──

  pub async fn to_have_count(&self, expected: usize) -> Result<(), AssertionFailure> {
    let mut params = self.params("to.have.count");
    let expected = f64::from(u32::try_from(expected).unwrap_or(u32::MAX));
    params.expected_number = Some(expected);
    self
      .web("toHaveCount", params, Printed::Equal(Shown::Number(expected)))
      .run()
      .await
  }
}

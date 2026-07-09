//! Lazy builders for the sync locator getters (`get_by_role`,
//! `get_by_text`, ...). The builder derefs to [`Locator`], so it can be
//! used anywhere a locator can — chaining a setter refines the options
//! before the selector materializes:
//!
//! ```ignore
//! page.get_by_role("button").name("Save").click().await?;
//! expect(&page.get_by_text("Welcome").exact(true)).to_be_visible().await?;
//! let submit: Locator = page.get_by_role("button").name("Submit").into_locator();
//! ```

use std::sync::OnceLock;

use crate::locator::Locator;
use crate::options::{RoleOptions, TextOptions};

/// A [`Locator`] whose selector is built lazily from an option bag `O`.
/// Setters refine the options; any locator use (via `Deref`) materializes
/// the selector.
pub struct LocatorBuilder<O> {
  opts: O,
  build: Box<dyn Fn(&O) -> Locator + Send + Sync>,
  cell: OnceLock<Locator>,
}

impl<O: Default> LocatorBuilder<O> {
  pub(crate) fn new(build: impl Fn(&O) -> Locator + Send + Sync + 'static) -> Self {
    Self {
      opts: O::default(),
      build: Box::new(build),
      cell: OnceLock::new(),
    }
  }
}

impl<O> LocatorBuilder<O> {
  /// Replace the whole option bag.
  #[must_use]
  pub fn options(mut self, opts: O) -> Self {
    self.opts = opts;
    self.cell = OnceLock::new();
    self
  }

  /// Replace the option bag when `Some`; keep defaults when `None`.
  #[must_use]
  pub fn maybe_options(self, opts: Option<O>) -> Self {
    match opts {
      Some(o) => self.options(o),
      None => self,
    }
  }

  /// Materialize the underlying [`Locator`].
  #[must_use]
  pub fn into_locator(self) -> Locator {
    let Self { opts, build, cell } = self;
    cell.into_inner().unwrap_or_else(|| build(&opts))
  }
}

impl<O> std::ops::Deref for LocatorBuilder<O> {
  type Target = Locator;

  fn deref(&self) -> &Locator {
    self.cell.get_or_init(|| (self.build)(&self.opts))
  }
}

impl<O> std::borrow::Borrow<Locator> for LocatorBuilder<O> {
  fn borrow(&self) -> &Locator {
    self
  }
}

impl<O> From<LocatorBuilder<O>> for Locator {
  fn from(builder: LocatorBuilder<O>) -> Self {
    builder.into_locator()
  }
}

macro_rules! lazy_setter {
  ($opts:path, $name:ident, $ty:ty) => {
    impl LocatorBuilder<$opts> {
      #[doc = concat!("Set the `", stringify!($name), "` option.")]
      #[must_use]
      pub fn $name(mut self, value: impl Into<$ty>) -> Self {
        self.opts.$name = Some(value.into());
        self.cell = OnceLock::new();
        self
      }
    }
  };
}

lazy_setter!(RoleOptions, name, crate::options::StringOrRegex);
lazy_setter!(RoleOptions, description, crate::options::StringOrRegex);
lazy_setter!(RoleOptions, exact, bool);
lazy_setter!(RoleOptions, checked, bool);
lazy_setter!(RoleOptions, disabled, bool);
lazy_setter!(RoleOptions, expanded, bool);
lazy_setter!(RoleOptions, level, i32);
lazy_setter!(RoleOptions, pressed, bool);
lazy_setter!(RoleOptions, selected, bool);
lazy_setter!(RoleOptions, include_hidden, bool);

lazy_setter!(TextOptions, exact, bool);

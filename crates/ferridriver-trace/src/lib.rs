//! Playwright trace archives, outside the browser: read one back into a
//! model ([`model`]), rewrite one without the secrets it recorded
//! ([`redact`]), and print a selector the way a script would have written
//! it ([`locator_generator`]).

pub mod locator_generator;
pub mod model;
pub mod redact;

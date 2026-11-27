//! Header formatting, splitting, case-folding, and form URL decoding helpers.

use std::sync::Arc;

use rustc_hash::FxHashMap;

use super::{HeaderEntry, Headers, RawHeadersFn};

#[must_use]
pub fn headers_to_array(map: &Headers) -> Vec<HeaderEntry> {
  map
    .iter()
    .map(|(k, v)| HeaderEntry {
      name: k.clone(),
      value: v.clone(),
    })
    .collect()
}

/// Playwright's `headersObjectToArray` (`packages/isomorphic/headers.ts`).
///
/// A protocol that reports response headers as a MAP has already joined
/// a repeated header's values, and `separator` is what it joined them
/// with. Splitting is the only way a header sent twice survives at all:
/// `X-Robots-Tag` and `Set-Cookie` are both headers a server repeats,
/// and each copy means something on its own.
///
/// The cost is a header whose single value contains the separator --
/// `Date: Mon, 06 Nov 1994 08:49:37 GMT` becomes two entries. Nothing
/// in the map distinguishes the two cases, which is why Playwright's
/// own `WebKit` backend splits the same way and lives with it.
#[must_use]
pub fn headers_map_to_split_array(map: &Headers, separator: &str, set_cookie_separator: &str) -> Vec<HeaderEntry> {
  let mut out = Vec::with_capacity(map.len());
  for (name, values) in map {
    let sep = if name.eq_ignore_ascii_case("set-cookie") {
      set_cookie_separator
    } else {
      separator
    };
    for value in values.split(sep) {
      out.push(HeaderEntry {
        name: name.clone(),
        value: value.trim().to_string(),
      });
    }
  }
  out
}

/// A [`RawHeadersFn`] that answers with headers already in hand.
#[must_use]
pub fn known_raw_headers(headers: Vec<HeaderEntry>) -> RawHeadersFn {
  Arc::new(move || {
    let headers = headers.clone();
    Box::pin(async move { Ok(headers) })
  })
}

/// Playwright's `headers()`: names lower-cased, a name the protocol
/// reported under two casings joined like a repeated header.
pub(crate) fn lowercase_names(map: &Headers) -> Headers {
  let mut out: Headers = FxHashMap::with_capacity_and_hasher(map.len(), rustc_hash::FxBuildHasher);
  for (name, value) in map {
    let name = name.to_ascii_lowercase();
    let separator = if name == "set-cookie" { "\n" } else { ", " };
    out
      .entry(name)
      .and_modify(|joined| {
        joined.push_str(separator);
        joined.push_str(value);
      })
      .or_insert_with(|| value.clone());
  }
  out
}

#[must_use]
pub fn headers_array_to_map(arr: &[HeaderEntry]) -> Headers {
  let mut out: Headers = FxHashMap::with_capacity_and_hasher(arr.len(), rustc_hash::FxBuildHasher);
  for h in arr {
    let lc = h.name.to_ascii_lowercase();
    let sep = if lc == "set-cookie" { "\n" } else { ", " };
    out
      .entry(lc)
      .and_modify(|joined| {
        joined.push_str(sep);
        joined.push_str(&h.value);
      })
      .or_insert_with(|| h.value.clone());
  }
  out
}

#[must_use]
pub fn get_header_value(arr: &[HeaderEntry], name: &str) -> Option<String> {
  let sep = if name.eq_ignore_ascii_case("set-cookie") {
    "\n"
  } else {
    ", "
  };
  let mut out: Option<String> = None;
  for h in arr {
    if h.name.eq_ignore_ascii_case(name) {
      match out {
        Some(ref mut joined) => {
          joined.push_str(sep);
          joined.push_str(&h.value);
        },
        None => out = Some(h.value.clone()),
      }
    }
  }
  out
}

pub(super) fn url_decode_form(body: &str) -> Vec<(String, String)> {
  body
    .split('&')
    .filter_map(|pair| {
      let mut it = pair.splitn(2, '=');
      let k = it.next()?;
      let v = it.next().unwrap_or("");
      Some((decode(k), decode(v)))
    })
    .collect()
}

fn decode(s: &str) -> String {
  let mut out = Vec::with_capacity(s.len());
  let bytes = s.as_bytes();
  let mut i = 0;
  while i < bytes.len() {
    match bytes[i] {
      b'+' => {
        out.push(b' ');
        i += 1;
      },
      b'%' if i + 2 < bytes.len() => {
        if let (Some(hi), Some(lo)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2])) {
          out.push(hi * 16 + lo);
          i += 3;
        } else {
          out.push(bytes[i]);
          i += 1;
        }
      },
      b => {
        out.push(b);
        i += 1;
      },
    }
  }
  String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(b: u8) -> Option<u8> {
  match b {
    b'0'..=b'9' => Some(b - b'0'),
    b'a'..=b'f' => Some(b - b'a' + 10),
    b'A'..=b'F' => Some(b - b'A' + 10),
    _ => None,
  }
}

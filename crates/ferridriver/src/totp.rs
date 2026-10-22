//! Time-based one-time passwords (RFC 6238), so a test can sign in to an
//! account protected by an authenticator app: the shared secret goes in
//! the environment, the code is derived at the moment it is typed.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{FerriError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
  Sha1,
  Sha256,
  Sha512,
}

impl Algorithm {
  /// # Errors
  ///
  /// A name other than SHA1, SHA256 or SHA512 (any case, dash optional).
  pub fn parse(name: &str) -> Result<Self> {
    match name.to_ascii_uppercase().replace('-', "").as_str() {
      "SHA1" => Ok(Self::Sha1),
      "SHA256" => Ok(Self::Sha256),
      "SHA512" => Ok(Self::Sha512),
      _ => Err(FerriError::invalid_argument(
        "algorithm",
        format!("{name:?} is not SHA1, SHA256 or SHA512"),
      )),
    }
  }

  fn ring(self) -> ring::hmac::Algorithm {
    match self {
      Self::Sha1 => ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
      Self::Sha256 => ring::hmac::HMAC_SHA256,
      Self::Sha512 => ring::hmac::HMAC_SHA512,
    }
  }
}

/// What a code is derived from. The defaults are what authenticator apps
/// assume when an `otpauth://` link says nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Totp {
  pub secret: Vec<u8>,
  pub digits: u32,
  pub period: u64,
  pub algorithm: Algorithm,
}

impl Totp {
  /// A base32 secret as authenticator apps show it (spaces, padding and
  /// case ignored), or a whole `otpauth://totp/...` link, whose `digits`,
  /// `period` and `algorithm` parameters then apply.
  ///
  /// # Errors
  ///
  /// A secret that is not base32, or a link that is not a TOTP link.
  pub fn parse(text: &str) -> Result<Self> {
    let text = text.trim();
    if !text.starts_with("otpauth://") {
      return Ok(Self {
        secret: base32(text)?,
        digits: 6,
        period: 30,
        algorithm: Algorithm::Sha1,
      });
    }
    let rest = &text["otpauth://".len()..];
    let (kind, rest) = rest.split_once('/').unwrap_or((rest, ""));
    if !kind.eq_ignore_ascii_case("totp") {
      return Err(FerriError::invalid_argument(
        "secret",
        "only otpauth://totp/ links describe a time-based code",
      ));
    }
    let mut totp = Self {
      secret: Vec::new(),
      digits: 6,
      period: 30,
      algorithm: Algorithm::Sha1,
    };
    // The parameters that matter are base32, digits and algorithm names,
    // none of which needs percent-decoding; the label and issuer are
    // skipped.
    let query = rest.split_once('?').map_or("", |(_, query)| query);
    for pair in query.split('&') {
      let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
      match key {
        "secret" => totp.secret = base32(value)?,
        "digits" => totp.digits = number(value, "digits")?,
        "period" => totp.period = number(value, "period")?,
        "algorithm" => totp.algorithm = Algorithm::parse(value)?,
        _ => {},
      }
    }
    if totp.secret.is_empty() {
      return Err(FerriError::invalid_argument("secret", "the link has no secret"));
    }
    Ok(totp)
  }

  /// The code for `unix_seconds`.
  ///
  /// # Errors
  ///
  /// Digits outside 6 to 10, or a period of zero.
  pub fn code_at(&self, unix_seconds: u64) -> Result<String> {
    if !(6..=10).contains(&self.digits) {
      return Err(FerriError::invalid_argument("digits", "a code has 6 to 10 digits"));
    }
    if self.period == 0 {
      return Err(FerriError::invalid_argument(
        "period",
        "the period is at least one second",
      ));
    }
    let key = ring::hmac::Key::new(self.algorithm.ring(), &self.secret);
    let counter = unix_seconds / self.period;
    let mac = ring::hmac::sign(&key, &counter.to_be_bytes());
    let mac = mac.as_ref();
    // RFC 4226 dynamic truncation: the low nibble of the last byte picks
    // where the 31-bit number starts.
    let offset = usize::from(mac[mac.len() - 1] & 0x0f);
    let number = u32::from_be_bytes([mac[offset], mac[offset + 1], mac[offset + 2], mac[offset + 3]]) & 0x7fff_ffff;
    let code = u64::from(number) % 10_u64.pow(self.digits);
    Ok(format!("{code:0width$}", width = self.digits as usize))
  }

  /// The code for now.
  ///
  /// # Errors
  ///
  /// As [`Self::code_at`].
  pub fn code(&self) -> Result<String> {
    let now = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .map_or(0, |elapsed| elapsed.as_secs());
    self.code_at(now)
  }
}

fn number<T: std::str::FromStr>(value: &str, name: &str) -> Result<T> {
  value
    .parse()
    .map_err(|_| FerriError::invalid_argument(name, format!("{value:?} is not a number")))
}

/// RFC 4648 base32, the alphabet authenticator secrets are written in.
fn base32(text: &str) -> Result<Vec<u8>> {
  let mut bytes = Vec::with_capacity(text.len() * 5 / 8);
  let (mut buffer, mut bits) = (0_u32, 0_u32);
  for c in text.chars().filter(|c| !c.is_whitespace() && *c != '=' && *c != '-') {
    let value = match c.to_ascii_uppercase() {
      upper @ 'A'..='Z' => u32::from(upper) - u32::from('A'),
      digit @ '2'..='7' => u32::from(digit) - u32::from('2') + 26,
      _ => {
        return Err(FerriError::invalid_argument(
          "secret",
          format!("{c:?} is not base32 (A to Z and 2 to 7)"),
        ));
      },
    };
    buffer = (buffer << 5) | value;
    bits += 5;
    if bits >= 8 {
      bits -= 8;
      bytes.push(u8::try_from((buffer >> bits) & 0xff).unwrap_or_default());
    }
  }
  if bytes.is_empty() {
    return Err(FerriError::invalid_argument("secret", "the secret is empty"));
  }
  Ok(bytes)
}

#[cfg(test)]
mod tests {
  #![allow(clippy::unwrap_used)]

  use super::*;

  fn totp(secret: &[u8], digits: u32, algorithm: Algorithm) -> Totp {
    Totp {
      secret: secret.to_vec(),
      digits,
      period: 30,
      algorithm,
    }
  }

  /// RFC 6238 appendix B.
  #[test]
  fn matches_the_rfc_test_vectors() {
    let sha1 = totp(b"12345678901234567890", 8, Algorithm::Sha1);
    let sha256 = totp(b"12345678901234567890123456789012", 8, Algorithm::Sha256);
    let sha512 = totp(
      b"1234567890123456789012345678901234567890123456789012345678901234",
      8,
      Algorithm::Sha512,
    );
    for (at, one, two, five) in [
      (59, "94287082", "46119246", "90693936"),
      (1_111_111_109, "07081804", "68084774", "25091201"),
      (2_000_000_000, "69279037", "90698825", "38618901"),
    ] {
      assert_eq!(sha1.code_at(at).unwrap(), one);
      assert_eq!(sha256.code_at(at).unwrap(), two);
      assert_eq!(sha512.code_at(at).unwrap(), five);
    }
  }

  #[test]
  fn reads_secrets_as_authenticator_apps_show_them() {
    let plain = Totp::parse("GEZD GNBV GY3T QOJQ GEZD GNBV GY3T QOJQ").unwrap();
    assert_eq!(plain.secret, b"12345678901234567890");
    assert_eq!(plain.code_at(59).unwrap(), "287082");
    let link = Totp::parse(
      "otpauth://totp/Acme:salama@example.com?secret=gezdgnbvgy3tqojqgezdgnbvgy3tqojq&digits=8&algorithm=SHA1&issuer=Acme",
    )
    .unwrap();
    assert_eq!(link.code_at(59).unwrap(), "94287082");
    assert!(Totp::parse("not base32!").is_err());
    assert!(Totp::parse("otpauth://hotp/x?secret=GEZD").is_err());
  }
}

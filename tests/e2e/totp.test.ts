// The RFC 6238 vectors through the script binding.

import { test, expect, totp } from '@ferridriver/test';

const SECRET = 'GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ';

test('totp_matches_the_rfc_vectors', async () => {
  expect(totp(SECRET, { timestamp: 59, digits: 8 })).toBe('94287082');
  expect(totp(SECRET, { timestamp: 1111111109, digits: 8 })).toBe('07081804');
  expect(totp('gezd gnbv gy3t qojq gezd gnbv gy3t qojq', { timestamp: 59 })).toBe('287082');
});

test('totp_reads_otpauth_links', async () => {
  const link = `otpauth://totp/Acme:salama@example.com?secret=${SECRET}&digits=8&issuer=Acme`;
  expect(totp(link, { timestamp: 59 })).toBe('94287082');
  expect(totp(SECRET)).toMatch(/^\d{6}$/);
});

test('totp_refuses_what_is_not_a_secret', async () => {
  expect(() => totp('not base32!')).toThrow(/base32/);
  expect(() => totp(SECRET, { algorithm: 'MD5' as never })).toThrow(/SHA1, SHA256 or SHA512/);
});

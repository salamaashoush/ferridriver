// Cluster 4 commit 1 — generic value matchers, asymmetric matchers,
// .resolves / .rejects / .not / .soft / .poll modifiers.
//
// Pure JS-side matchers; no backend involvement, so this runs in
// plain bun:test without the TestRunner harness.

import { test, expect as bunExpect } from 'bun:test';
import { expect } from '../../../packages/ferridriver-test/src/expect';

// ── Generic matchers ──

test('toBe / toEqual / toStrictEqual / toMatchObject', async () => {
  await expect(1).toBe(1);
  await expect({ a: 1 }).toEqual({ a: 1 });
  await expect({ a: 1, b: 2 }).toMatchObject({ a: 1 });
  await expect([1, 2, 3]).toStrictEqual([1, 2, 3]);
  await bunExpect(async () => expect(1).toBe(2)).toThrow();
});

test('truthiness matchers', async () => {
  await expect(true).toBeTruthy();
  await expect(0).toBeFalsy();
  await expect(undefined).toBeUndefined();
  await expect(null).toBeNull();
  await expect(NaN).toBeNaN();
  await expect('value').toBeDefined();
  await bunExpect(async () => expect(0).toBeTruthy()).toThrow();
});

test('numeric comparisons', async () => {
  await expect(5).toBeGreaterThan(3);
  await expect(5).toBeGreaterThanOrEqual(5);
  await expect(2).toBeLessThan(3);
  await expect(2).toBeLessThanOrEqual(2);
  await expect(0.1 + 0.2).toBeCloseTo(0.3);
  await bunExpect(async () => expect(2).toBeGreaterThan(5)).toThrow();
});

test('toContain / toContainEqual / toHaveLength', async () => {
  await expect([1, 2, 3]).toContain(2);
  await expect('hello world').toContain('world');
  await expect([{ a: 1 }, { a: 2 }]).toContainEqual({ a: 2 });
  await expect([1, 2, 3]).toHaveLength(3);
  await expect('abc').toHaveLength(3);
});

test('toHaveProperty (path + optional value)', async () => {
  await expect({ a: { b: { c: 42 } } }).toHaveProperty('a.b.c');
  await expect({ a: { b: { c: 42 } } }).toHaveProperty('a.b.c', 42);
  await expect({ a: { b: 1 } }).toHaveProperty(['a', 'b'], 1);
  await bunExpect(async () => expect({}).toHaveProperty('missing')).toThrow();
});

test('toMatch (string + RegExp)', async () => {
  await expect('hello world').toMatch('hello');
  await expect('foo123bar').toMatch(/foo\d+bar/);
  await bunExpect(async () => expect('abc').toMatch(/xyz/)).toThrow();
});

test('toBeInstanceOf', async () => {
  class MyError extends Error {}
  await expect(new MyError('x')).toBeInstanceOf(MyError);
  await expect(new MyError('x')).toBeInstanceOf(Error);
  await bunExpect(async () => expect({}).toBeInstanceOf(Error)).toThrow();
});

test('toThrow / toThrowError', async () => {
  await expect(() => { throw new Error('boom'); }).toThrow();
  await expect(() => { throw new Error('a specific boom'); }).toThrow('specific');
  await expect(() => { throw new TypeError('typed'); }).toThrow(TypeError);
  await expect(() => { throw new Error('regex me'); }).toThrow(/regex/);
  await bunExpect(async () => expect(() => 1).toThrow()).toThrow();
});

test('.not negates', async () => {
  await expect(1).not.toBe(2);
  await expect([1, 2, 3]).not.toContain(99);
  await expect('hello').not.toMatch(/xyz/);
});

// ── Asymmetric matchers ──

test('expect.any', async () => {
  await expect('x').toEqual(expect.any(String));
  await expect(42).toEqual(expect.any(Number));
  await expect(true).toEqual(expect.any(Boolean));
  await bunExpect(async () => expect('x').toEqual(expect.any(Number))).toThrow();
});

test('expect.anything', async () => {
  await expect('x').toEqual(expect.anything());
  await expect(0).toEqual(expect.anything());
  await bunExpect(async () => expect(null).toEqual(expect.anything())).toThrow();
  await bunExpect(async () => expect(undefined).toEqual(expect.anything())).toThrow();
});

test('expect.objectContaining', async () => {
  await expect({ a: 1, b: 2, c: 3 }).toEqual(expect.objectContaining({ a: 1, b: 2 }));
  await expect({ a: { nested: true }, b: 2 }).toEqual(expect.objectContaining({ a: expect.anything() }));
  await bunExpect(async () =>
    expect({ a: 1 }).toEqual(expect.objectContaining({ a: 1, b: 2 })),
  ).toThrow();
});

test('expect.arrayContaining', async () => {
  await expect([1, 2, 3]).toEqual(expect.arrayContaining([2, 3]));
  await expect([1, 2, 3]).toEqual(expect.arrayContaining([]));
  await bunExpect(async () => expect([1, 2]).toEqual(expect.arrayContaining([5]))).toThrow();
});

test('expect.stringContaining / stringMatching / closeTo', async () => {
  await expect('hello world').toEqual(expect.stringContaining('hello'));
  await expect('hello world').toEqual(expect.stringMatching(/world/));
  await expect(0.1 + 0.2).toEqual(expect.closeTo(0.3));
  await bunExpect(async () => expect('xyz').toEqual(expect.stringContaining('abc'))).toThrow();
});

test('asymmetric matchers nest in toMatchObject', async () => {
  await expect({ user: { id: 1, name: 'alice' } }).toMatchObject({
    user: { id: expect.any(Number), name: expect.stringContaining('lic') },
  });
});

// ── Promise modifiers ──

test('.resolves resolves promises', async () => {
  await expect(Promise.resolve(42)).resolves.toBe(42);
  await expect(Promise.resolve({ a: 1 })).resolves.toEqual({ a: 1 });
});

test('.resolves throws when promise rejects', async () => {
  await bunExpect(async () => expect(Promise.reject(new Error('boom'))).resolves.toBe(1)).toThrow();
});

test('.rejects unwraps rejection', async () => {
  await expect(Promise.reject(new Error('expected boom'))).rejects.toBeInstanceOf(Error);
});

test('.rejects throws when promise resolves', async () => {
  await bunExpect(async () => expect(Promise.resolve(1)).rejects.toBe(1)).toThrow();
});

// ── expect.poll ──

test('expect.poll resolves once probe matches', async () => {
  let count = 0;
  const start = Date.now();
  await expect.poll(() => ++count, { timeout: 1000, intervals: [10, 20, 40] }).toBeGreaterThan(3);
  expect(count).toBeGreaterThanOrEqual(4);
  expect(Date.now() - start).toBeLessThan(500);
});

test('expect.poll throws when probe never matches', async () => {
  await bunExpect(async () =>
    expect.poll(() => 1, { timeout: 200, intervals: [50] }).toBe(99),
  ).toThrow();
});

// ── expect.soft ──
//
// `.soft` requires a live testInfo to push to. Without one it falls
// back to no-op (per design). This file runs outside the TestRunner
// harness so we test the no-op path: soft.toBe failure does NOT
// throw (no testInfo to push to) instead of throwing.

test('expect.soft does not throw without testInfo', async () => {
  // No TestRunner around — the push-soft-error helper finds no
  // testInfo and silently drops the failure. The matcher's
  // .toBe() resolves without throwing.
  await expect.soft(1).toBe(2);
  // Sanity: hard expect still throws.
  await bunExpect(async () => expect(1).toBe(2)).toThrow();
});

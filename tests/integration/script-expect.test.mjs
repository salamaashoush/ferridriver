import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { repo, script, scriptError } from './support.mjs';

async function failure(source) {
  const error = await scriptError(source);
  return `${error.name}: ${error.message}`;
}

test("to be primitive pass", async () => {
  await script("expect(1).toBe(1); return 'ok'");
});

test("to be primitive fail throws", async () => {
  {
    const error = await failure("expect(1).toBe(2); return 'unreached'");
    assert.ok(error.includes("toBe"), error);
  }
});

test("js stack is captured on failure", async () => {
  const error = await scriptError("function inner() { expect(1).toBe(2); }\ninner();\nreturn 'unreached';");
  assert.ok(error.stack?.length > 0);
  assert.ok(error.stack.includes('at '));
});

test("to equal failure message has unified diff", async () => {
  {
    const error = await failure("expect({a: 1, b: 'x'}).toEqual({a: 2, b: 'x'}); return 'unreached'");
    assert.ok(error.includes("toEqual") && error.includes("Diff:"), error);
    assert.ok(error.includes('-'), error);
    assert.ok(error.includes('+'), error);
  }
});

test("to equal nested pass", async () => {
  await script("expect({a: [1, 2]}).toEqual({a: [1, 2]}); return 'ok'");
});

test("to equal with asymmetric any number", async () => {
  await script("expect({id: 7, name: 'n'}).toEqual({id: expect.any(Number), name: 'n'}); return 'ok'");
});

test("to equal with asymmetric object containing", async () => {
  await script("const actual = {a: 1, b: 2, c: 3}; expect(actual).toEqual(expect.objectContaining({a: 1, c: 3})); return 'ok'");
});

test("to equal with asymmetric array containing", async () => {
  await script("expect([1, 2, 3, 4]).toEqual(expect.arrayContaining([2, 3])); return 'ok'");
});

test("to equal with asymmetric string matching regex", async () => {
  await script("expect('Hello World').toEqual(expect.stringMatching(/hello/i)); return 'ok'");
});

test("to equal with asymmetric string containing", async () => {
  await script("expect('Hello World').toEqual(expect.stringContaining('World')); return 'ok'");
});

test("asymmetric not inverts", async () => {
  await script("expect('Hello').toEqual(expect.not.stringContaining('Bye')); return 'ok'");
});

test("to be close to default digits", async () => {
  await script("expect(0.1 + 0.2).toBeCloseTo(0.3); return 'ok'");
});

test("to be close to explicit digits", async () => {
  await script("expect(3.14159).toBeCloseTo(3.14, 2); return 'ok'");
});

test("not inverts to be", async () => {
  await script("expect(1).not.toBe(2); return 'ok'");
});

test("not invert fail throws", async () => {
  {
    const error = await failure("expect(1).not.toBe(1); return 'unreached'");
    assert.ok(error.includes("toBe"), error);
  }
});

test("to contain array and string", async () => {
  await script("expect([1, 2, 3]).toContain(2); return 'ok'");
  await script("expect('hello world').toContain('world'); return 'ok'");
});

test("to have length array and string", async () => {
  await script("expect([1, 2, 3]).toHaveLength(3); return 'ok'");
  await script("expect('abcd').toHaveLength(4); return 'ok'");
});

test("to have property dot path with value", async () => {
  await script("expect({a: {b: 42}}).toHaveProperty('a.b', 42); return 'ok'");
});

test("to have property array path index", async () => {
  await script("expect({arr: [10, 20]}).toHaveProperty(['arr', 1], 20); return 'ok'");
});

test("to match substring", async () => {
  await script("expect('hello world').toMatch('world'); return 'ok'");
});

test("to match regex", async () => {
  await script("expect('hello world').toMatch(/^hello/); return 'ok'");
});

test("to match object subset", async () => {
  await script("expect({a: 1, b: 2, c: 3}).toMatchObject({a: 1, c: 3}); return 'ok'");
});

test("to be instance of builtins", async () => {
  await script("expect([1, 2, 3]).toBeInstanceOf(Array); return 'ok'");
});

test("to be is object is", async () => {
  await script("const a = {v:1}; expect(a).toBe(a); return 'ok'");
  await script("expect({v:1}).not.toBe({v:1}); return 'ok'");
  await script("expect({v:1}).toEqual({v:1}); return 'ok'");
  await script("expect(NaN).toBe(NaN); expect(0).not.toBe(-0); return 'ok'");
  {
    const error = await failure("expect({v:1}).toBe({v:1}); return 'unreached'");
    assert.ok(error.includes("replace \\\"toBe\\\" with \\\"toEqual\\\"") || error.includes("replace \"toBe\" with \"toEqual\""), error);
  }
});

test("a function is a value subject", async () => {
  await script("const f = (a,b) => a; expect(f).toBe(f); return 'ok'");
  await script("const f = () => 1; expect(f).not.toBe(() => 1); return 'ok'");
  await script("expect((a,b) => a).toHaveLength(2); return 'ok'");
  await script("expect(() => 1).toBeInstanceOf(Function); return 'ok'");
  await script("expect(() => 1).toBeTruthy(); return 'ok'");
});

test("undefined is not null", async () => {
  await script("expect(undefined).not.toBeNull(); return 'ok'");
  await script("expect(null).not.toBeUndefined(); return 'ok'");
  await script("expect(null).toBeDefined(); return 'ok'");
  await script("expect(undefined).not.toBeDefined(); return 'ok'");
  await script("expect(undefined).not.toBe(null); return 'ok'");
});

test("to be nan needs a number", async () => {
  await script("expect(NaN).toBeNaN(); return 'ok'");
  await script("expect('NaN').not.toBeNaN(); return 'ok'");
  await script("expect(null).not.toBeNaN(); return 'ok'");
});

test("to be instance of walks the prototype chain", async () => {
  await script("class A {}; class B extends A {}; expect(new B()).toBeInstanceOf(A); return 'ok'");
  await script("class A {}; expect(new A()).toBeInstanceOf(Object); return 'ok'");
  await script("class A {}; expect(new A()).not.toBeInstanceOf(Error); return 'ok'");
  {
    const error = await failure("expect(1).toBeInstanceOf(5); return 'unreached'");
    assert.ok(error.includes("must be a function"), error);
  }
});

test("to contain compares items strictly", async () => {
  await script("const o = {}; expect([o]).toContain(o); return 'ok'");
  await script("expect([{a:1}]).not.toContain({a:1}); return 'ok'");
  await script("expect([{a:1}]).toContainEqual({a:1}); return 'ok'");
  await script("expect(new Set(['a'])).toContain('a'); return 'ok'");
});

test("to contain misuse is a type error under not", async () => {
  assert.match(await failure("expect(null).toContain(1); return 'unreached'"), /TypeError/);
  assert.match(await failure("expect(null).not.toContain(1); return 'unreached'"), /TypeError/);
  assert.match(await failure("expect('hi').toContain(1); return 'unreached'"), /TypeError/);
  assert.match(await failure("expect(7).toContain(1); return 'unreached'"), /TypeError/);
});

test("to have length reads the live length", async () => {
  await script("expect(new Uint8Array(4)).toHaveLength(4); return 'ok'");
  await script("expect('a\\u{1F600}').toHaveLength(3); return 'ok'");
  {
    const error = await failure("expect({a:1}).toHaveLength(1); return 'unreached'");
    assert.ok(error.includes("length property"), error);
  }
});

test("expect takes a custom message", async () => {
  {
    const error = await failure("expect(1, 'ids match').toBe(2); return 'unreached'");
    assert.ok(error.includes("ids match"), error);
  }
  {
    const error = await failure("expect(1, { message: 'ids match' }).toBe(2); return 'unreached'");
    assert.ok(error.includes("ids match"), error);
  }
});

test("the core builtin list matches the shipped matchers", async () => {
  const { value } = await script("const proto = Object.getPrototypeOf(expect(1));\n     return Object.getOwnPropertyNames(proto).filter(n => n.startsWith('to')).sort();");
  const source = await readFile(join(repo, 'crates/ferridriver-expect/src/extend.rs'), 'utf8');
  const declaration = source.match(/pub const BUILTIN_MATCHER_NAMES:[\s\S]*?= &\[([\s\S]*?)\];/);
  assert.ok(declaration);
  const listed = [...declaration[1].matchAll(/"([^"\n]+)"/g)].map(match => match[1]);
  assert.deepEqual(value, listed);
});

test("to equal compares maps and sets", async () => {
  await script("expect(new Map([['a', 1]])).toEqual(new Map([['a', 1]])); return 'ok'");
  await script("expect(new Map([['a', 1]])).not.toEqual(new Map([['a', 2]])); return 'ok'");
  await script("expect(new Map([['a', 1]])).not.toEqual(new Map()); return 'ok'");
  await script("expect(new Set([1, 2])).toEqual(new Set([2, 1])); return 'ok'");
  await script("expect(new Set([1, 2])).not.toEqual(new Set([1, 3])); return 'ok'");
  await script("expect(new Map([['a', 1]])).not.toEqual({ a: 1 }); return 'ok'");
  await script("expect({m: new Map([['a', {b: 1}]])}).toEqual({m: new Map([['a', {b: expect.any(Number)}]])}); return 'ok'");
});

test("to equal compares dates regexps and errors", async () => {
  await script("expect(new Date(5)).toEqual(new Date(5)); return 'ok'");
  await script("expect(new Date(5)).not.toEqual(new Date(6)); return 'ok'");
  await script("expect(new Date(NaN)).toEqual(new Date(NaN)); return 'ok'");
  await script("expect(/ab+/gi).toEqual(/ab+/gi); return 'ok'");
  await script("expect(/ab+/g).not.toEqual(/ab+/i); return 'ok'");
  await script("expect(new Error('boom')).toEqual(new Error('boom')); return 'ok'");
  await script("expect(new Error('boom')).not.toEqual(new Error('other')); return 'ok'");
  await script("expect(new RangeError('x')).not.toEqual(new Error('x')); return 'ok'");
});

test("to equal ignores undefined keys and to strict equal does not", async () => {
  await script("expect({ a: 1, b: undefined }).toEqual({ a: 1 }); return 'ok'");
  await script("expect({ a: 1 }).toEqual({ a: 1, b: undefined }); return 'ok'");
  await script("expect({ a: 1, b: undefined }).not.toStrictEqual({ a: 1 }); return 'ok'");
  await script("expect({ a: 1, b: undefined }).toStrictEqual({ a: 1, b: undefined }); return 'ok'");
});

test("to strict equal compares the class", async () => {
  await script("class Point { constructor() { this.x = 1; } }\n     expect(new Point()).toEqual({ x: 1 });\n     expect(new Point()).not.toStrictEqual({ x: 1 });\n     expect(new Point()).toStrictEqual(new Point());\n     return 'ok'");
});

test("to strict equal sees array holes", async () => {
  await script("expect([, 1]).toEqual([undefined, 1]); return 'ok'");
  await script("expect([, 1]).not.toStrictEqual([undefined, 1]); return 'ok'");
  await script("expect([, 1]).toStrictEqual([, 1]); return 'ok'");
});

test("to equal compares bigints and typed arrays", async () => {
  await script("expect(1n).toEqual(1n); return 'ok'");
  await script("expect(1n).not.toEqual(2n); return 'ok'");
  await script("expect(new Uint8Array([1, 2])).toEqual(new Uint8Array([1, 2])); return 'ok'");
  await script("expect(new Uint8Array([1, 2])).not.toEqual(new Uint8Array([1, 3])); return 'ok'");
});

test("a cyclic structure terminates", async () => {
  await script("const a = { name: 'a' }; a.self = a;\n     const b = { name: 'a' }; b.self = b;\n     expect(a).toEqual(b);\n     const c = { name: 'c' }; c.self = c;\n     expect(a).not.toEqual(c);\n     return 'ok'");
});

test("to have property walks the live value", async () => {
  await script("expect({ a: { b: 42 } }).toHaveProperty('a.b', 42); return 'ok'");
  await script("expect({ arr: [10, 20] }).toHaveProperty(['arr', 1], 20); return 'ok'");
  await script("expect({ m: new Date(3) }).toHaveProperty('m', new Date(3)); return 'ok'");
  await script("class Holder { get computed() { return 7; } }\n     expect(new Holder()).toHaveProperty('computed', 7);\n     return 'ok'");
  await script("expect({ a: 1 }).not.toHaveProperty('b'); return 'ok'");
});

test("to contain equal uses deep equality over live items", async () => {
  await script("expect([{ a: 1 }]).toContainEqual({ a: 1 }); return 'ok'");
  await script("expect([new Date(1)]).toContainEqual(new Date(1)); return 'ok'");
  await script("expect(new Set([{ a: 1 }])).toContainEqual({ a: 1 }); return 'ok'");
  await script("expect([{ a: 1 }]).not.toContainEqual({ a: 2 }); return 'ok'");
});

test("extend adds a matcher to the returned expect", async () => {
  await script("const within = { toBeWithin(received, lo, hi) { const pass = received >= lo && received <= hi; return { pass, message: () => `expected ${received} ${this.isNot ? 'not ' : ''}to be within ${lo}..${hi}` }; } }; const e = expect.extend(within); e(5).toBeWithin(0, 10); return 'ok'");
  {
    const error = await failure("const within = { toBeWithin(received, lo, hi) { const pass = received >= lo && received <= hi; return { pass, message: () => `expected ${received} ${this.isNot ? 'not ' : ''}to be within ${lo}..${hi}` }; } }; const e = expect.extend(within); e(50).toBeWithin(0, 10); return 'unreached'");
    assert.ok(error.includes("to be within 0..10"), error);
  }
});

test("a custom matcher inverts and reads its context", async () => {
  await script("const within = { toBeWithin(received, lo, hi) { const pass = received >= lo && received <= hi; return { pass, message: () => `expected ${received} ${this.isNot ? 'not ' : ''}to be within ${lo}..${hi}` }; } }; const e = expect.extend(within); e(50).not.toBeWithin(0, 10); return 'ok'");
  {
    const error = await failure("const within = { toBeWithin(received, lo, hi) { const pass = received >= lo && received <= hi; return { pass, message: () => `expected ${received} ${this.isNot ? 'not ' : ''}to be within ${lo}..${hi}` }; } }; const e = expect.extend(within); e(5).not.toBeWithin(0, 10); return 'unreached'");
    assert.ok(error.includes("not to be within"), error);
  }
});

test("extend publishes a new name on the original expect too", async () => {
  await script("const within = { toBeWithin(received, lo, hi) { const pass = received >= lo && received <= hi; return { pass, message: () => `expected ${received} ${this.isNot ? 'not ' : ''}to be within ${lo}..${hi}` }; } }; expect.extend(within); expect(5).toBeWithin(0, 10); return 'ok'");
});

test("extend never shadows a builtin on the original expect", async () => {
  await script("const e = expect.extend({ toBe(received, expected) { return { pass: true, message: () => 'always' }; } });\n     e(1).toBe(2);\n     let threw = false;\n     try { expect(1).toBe(2); } catch { threw = true; }\n     if (!threw) throw new Error('the original expect lost its built-in toBe');\n     return 'ok'");
});

test("extend refuses a non function", async () => {
  {
    const error = await failure("expect.extend({ toBeX: 5 }); return 'unreached'");
    assert.ok(error.includes("is not a valid matcher") && error.includes("number"), error);
  }
});

test("a custom matcher may be async and still fails", async () => {
  await script("const e = expect.extend({ async toBeLate(received) { return { pass: received === 1, message: () => 'late' }; } });\n     await e(1).toBeLate();\n     return 'ok'");
  {
    const error = await failure("const e = expect.extend({ async toBeLate(received) { return { pass: false, message: () => 'late' }; } });\n     await e(1).toBeLate();\n     return 'unreached'");
    assert.ok(error.includes("late"), error);
  }
});

test("a custom matcher returning junk says so", async () => {
  {
    const error = await failure("const e = expect.extend({ toBeX() { return 5; } }); e(1).toBeX(); return 'unreached'");
    assert.ok(error.includes("Unexpected return from a matcher function"), error);
  }
});

test("configure returns a new expect", async () => {
  await script("const quiet = expect.configure({ message: 'ids match' });\n     let msg = '';\n     try { quiet(1).toBe(2); } catch (e) { msg = String(e.message); }\n     if (!msg.includes('ids match')) throw new Error('configured message missing: ' + msg);\n     let plain = '';\n     try { expect(1).toBe(2); } catch (e) { plain = String(e.message); }\n     if (plain.includes('ids match')) throw new Error('the original expect was mutated');\n     return 'ok'");
});

test("a custom matcher observes the configured timeout", async () => {
  await script("const e = expect.configure({ timeout: 1234 }).extend({\n       toSeeTimeout(received) { return { pass: this.timeout === 1234, message: () => 'timeout was ' + this.timeout }; },\n     });\n     e(1).toSeeTimeout();\n     return 'ok'");
});

test("soft is a getter returning an expect", async () => {
  await script("if (typeof expect.soft !== 'function') throw new Error('soft is not callable'); return 'ok'");
  await script("expect.soft(1).toBe(1); return 'ok'");
  await script("if (expect.soft.soft !== expect.soft.soft.soft) { } return 'ok'");
  await script("await expect.soft.poll(() => 1, { timeout: 500, intervals: [5] }).toBe(1); return 'ok'");
});

test("get state answers an object", async () => {
  await script("if (typeof expect.getState() !== 'object') throw new Error('no state'); return 'ok'");
});

test("merge expects exposes every matcher", async () => {
  await script("const a = expect.extend({ toBeA(received) { return { pass: received === 'a', message: () => 'not a' }; } });\n     const b = expect.extend({ toBeB(received) { return { pass: received === 'b', message: () => 'not b' }; } });\n     const both = mergeExpects(a, b);\n     both('a').toBeA();\n     both('b').toBeB();\n     return 'ok'");
});

test("a custom matcher reaches the settled chain", async () => {
  await script("const e = expect.extend({ toBeA(received) { return { pass: received === 'a', message: () => 'not a' }; } });\n     await e(Promise.resolve('a')).resolves.toBeA();\n     await e(Promise.resolve('b')).resolves.not.toBeA();\n     return 'ok'");
});

test("a custom matcher is also an asymmetric matcher", async () => {
  await script("const e = expect.extend({ toBeEven(received) { return { pass: received % 2 === 0, message: () => 'odd' }; } });\n     e({ n: 4 }).toEqual({ n: e.toBeEven() });\n     e([2, 4]).toEqual([e.toBeEven(), e.toBeEven()]);\n     e({ n: 3 }).toEqual({ n: e.not.toBeEven() });\n     e({ a: { n: 8 } }).toMatchObject({ a: { n: e.toBeEven() } });\n     return 'ok'");
  {
    const error = await failure("const e = expect.extend({ toBeEven(received) { return { pass: received % 2 === 0, message: () => 'odd' }; } });\n     e({ n: 3 }).toEqual({ n: e.toBeEven() });\n     return 'unreached'");
    assert.ok(error.includes("toEqual"), error);
  }
});

test("an async custom matcher cannot be asymmetric", async () => {
  {
    const error = await failure("const e = expect.extend({ async toBeEven(received) { return { pass: true, message: () => '' }; } });\n     e({ n: 4 }).toEqual({ n: e.toBeEven() });\n     return 'unreached'");
    assert.ok(error.includes("toEqual"), error);
  }
});

test("array of matches every item", async () => {
  await script("expect([1, 2, 3]).toEqual(expect.arrayOf(expect.any(Number))); return 'ok'");
  await script("expect([]).toEqual(expect.arrayOf(expect.any(Number))); return 'ok'");
  await script("expect([1, 'two']).toEqual(expect.not.arrayOf(expect.any(Number))); return 'ok'");
  await script("expect({items: [1, 2]}).toMatchObject({items: expect.arrayOf(expect.any(Number))}); return 'ok'");
  {
    const error = await failure("expect([1, 'two']).toEqual(expect.arrayOf(expect.any(Number))); return 'unreached'");
    assert.ok(error.includes("toEqual"), error);
  }
});

test("resolves runs the matcher on the resolved value", async () => {
  await script("await expect(Promise.resolve(1)).resolves.toBe(1); return 'ok'");
  await script("await expect(Promise.resolve({a:1})).resolves.toEqual({a:1}); return 'ok'");
  await script("await expect(Promise.resolve(1)).resolves.not.toBe(2); return 'ok'");
  await script("await expect(async () => 7).resolves.toBe(7); return 'ok'");
});

test("rejects runs the matcher on the reason", async () => {
  await script("await expect(Promise.reject(new Error('boom'))).rejects.toThrow('boom'); return 'ok'");
  await script("await expect(Promise.reject(new RangeError('r'))).rejects.toThrow(RangeError); return 'ok'");
  await script("await expect(Promise.reject('plain')).rejects.toBe('plain'); return 'ok'");
  await script("await expect(Promise.reject(new Error('boom'))).rejects.not.toThrow('other'); return 'ok'");
});

test("settling the wrong way names which way", async () => {
  {
    const error = await failure("await expect(Promise.reject(new Error('x'))).resolves.toBe(1); return 'unreached'");
    assert.ok(error.includes("rejected instead of resolved"), error);
  }
  {
    const error = await failure("await expect(Promise.resolve(1)).rejects.toBe(1); return 'unreached'");
    assert.ok(error.includes("resolved instead of rejected"), error);
  }
});

test("a settled chain needs a promise", async () => {
  {
    const error = await failure("await expect(1).resolves.toBe(1); return 'unreached'");
    assert.ok(error.includes("promise, or a function returning a promise"), error);
  }
});

test("a settled matcher still fails normally", async () => {
  {
    const error = await failure("await expect(Promise.resolve(1)).resolves.toBe(2); return 'unreached'");
    assert.ok(error.includes("toBe"), error);
  }
});

test("poll refuses a settled chain", async () => {
  {
    const error = await failure("await expect.poll(() => 1).resolves.toBe(1); return 'unreached'");
    assert.ok(error.includes("does not support") && error.includes("resolves"), error);
  }
});

test("expect poll compares identity", async () => {
  await script("const wanted = {}; let n = 0;\n     await expect.poll(() => { n += 1; return n >= 2 ? wanted : {}; }, { timeout: 2000, intervals: [5] }).toBe(wanted);\n     return 'ok'");
});

test("to throw sync", async () => {
  await script("await expect(() => { throw new Error('boom'); }).toThrow(); return 'ok'");
});

test("to throw substring match", async () => {
  await script("await expect(() => { throw new Error('out of range'); }).toThrow('out of range'); return 'ok'");
});

test("to throw regex match", async () => {
  await script("await expect(() => { throw new Error('boom42'); }).toThrow(/boom\\d+/); return 'ok'");
});

test("to throw class match", async () => {
  await script("await expect(() => { throw new RangeError('bad'); }).toThrow(RangeError); return 'ok'");
});

test("to throw no throw fails", async () => {
  {
    const error = await failure("await expect(() => 42).toThrow(); return 'unreached'");
    assert.ok(error.includes("toThrow"), error);
  }
});

test("not to throw passes when no throw", async () => {
  await script("await expect(() => 42).not.toThrow(); return 'ok'");
});

test("to throw async promise", async () => {
  await script("await expect(async () => { throw new Error('async boom'); }).toThrow('async boom'); return 'ok'");
});

test("truthy and falsy", async () => {
  await script("expect(1).toBeTruthy(); return 'ok'");
  await script("expect(0).toBeFalsy(); return 'ok'");
  await script("expect('').toBeFalsy(); return 'ok'");
  await script("expect(null).toBeFalsy(); return 'ok'");
});

test("null and undefined", async () => {
  await script("expect(null).toBeNull(); return 'ok'");
  await script("expect(undefined).toBeUndefined(); return 'ok'");
  await script("expect(1).toBeDefined(); return 'ok'");
});

test("greater less than", async () => {
  await script("expect(5).toBeGreaterThan(3); return 'ok'");
  await script("expect(3).toBeGreaterThanOrEqual(3); return 'ok'");
  await script("expect(2).toBeLessThan(3); return 'ok'");
  await script("expect(3).toBeLessThanOrEqual(3); return 'ok'");
});

test("poll to equal succeeds after a few polls", async () => {
  await script("let count = 0; await expect.poll(() => { count += 1; return count; }, { timeout: 2000 }).toEqual(3); return 'ok'");
});

test("poll to satisfy with predicate", async () => {
  await script("let count = 0; await expect.poll(() => { count += 1; return count; }, { timeout: 2000 }).toSatisfy(v => v >= 3); return 'ok'");
});

test("poll timeout throws with last value", async () => {
  {
    const error = await failure("await expect.poll(() => 'never matches', { timeout: 300 }).toEqual('something'); return 'unreached'");
    assert.ok(error.includes("toEqual") && error.includes("timed out"), error);
  }
});

test("close to asymmetric", async () => {
  await script("expect({pi: 3.14159}).toEqual({pi: expect.closeTo(3.14, 2)}); return 'ok'");
});
